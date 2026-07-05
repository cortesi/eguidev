//! Script-first MCP launcher and proxy for eguidev.

use std::{
    collections::BTreeMap,
    env,
    fmt::Display,
    fs,
    future::{Future, pending},
    io::{self as std_io, IsTerminal},
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use eguidev::{FixtureParam, FixtureSpec, ParamKind, WidgetValue};
use eguidev_runtime::{
    ScriptArgValue, ScriptArgs, ScriptErrorInfo, ScriptEvalOptions, ScriptEvalOutcome,
    ScriptEvalRequest, script_definitions,
    smoke::{ScriptRunRequest, SuiteResult, discover_suite_scripts, run_suite_with},
};
use instance_registry::InstanceRegistry;
use serde::{
    Deserialize, Serialize,
    de::{DeserializeOwned, Error as SerdeDeError},
};
use syntect::{
    easy::HighlightLines,
    highlighting::ThemeSet,
    parsing::SyntaxSet,
    util::{LinesWithEndings, as_24_bit_terminal_escaped},
};
use tmcp::{
    Arguments, Error as McpError, Server, ServerCtx, ServerHandler,
    schema::{
        CallToolResponse, CallToolResult, ClientCapabilities, ContentBlock, Cursor, ImageContent,
        Implementation, InitializeResult, ListToolsResult, TaskMetadata, Tool, ToolSchema,
    },
};
use tokio::{
    io::{self as tokio_io, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    runtime::Handle,
    sync::Mutex as AsyncMutex,
    task::{JoinHandle, block_in_place},
    time::sleep,
};

mod config;
mod instance_registry;

use config::{
    DumpConfig, EdevCommand, EvalConfig, FixtureConfig, LaunchConfig, McpConfig, SmokeConfig,
};

/// Tool names forwarded from edev to the app MCP server.
const PROXIED_TOOL_NAMES: &[&str] = &["script_eval", "script_api"];
/// Timeout used for proxied request/response round-trips between edev and app MCP.
const APP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum app stdout/stderr bytes retained for diagnostics.
const APP_LOG_TAIL_LIMIT: usize = 4 * 1024 * 1024;
/// Extra log bytes retained before trimming back to the stable tail limit.
const APP_LOG_TAIL_TRIM_SLACK: usize = 256 * 1024;
/// Bundle note used when stdout is reserved for the MCP transport.
const STDOUT_TRANSPORT_NOTE: &str = "stdout is consumed by the stdio MCP transport for this launch; no app stdout log is available.\n";
/// Maximum attempts for restart when the app MCP transport closes mid-handshake.
const RESTART_MAX_ATTEMPTS: usize = 3;

/// Run the eguidev launcher on stdio.
pub async fn run() -> Result<(), EdevError> {
    match EdevCommand::from_env()? {
        EdevCommand::Help(help) => {
            print!("{help}");
            Ok(())
        }
        EdevCommand::Docs => {
            print!("{}", render_script_docs());
            Ok(())
        }
        EdevCommand::Mcp(config) => run_mcp(config).await,
        EdevCommand::Smoke(config) => run_smoke(config).await,
        EdevCommand::Eval(config) => run_eval(config).await,
        EdevCommand::Dump(config) => run_dump(config).await,
        EdevCommand::Fixture(config) => run_fixture(config).await,
    }
}

/// Run the long-lived `edev mcp` launcher server over stdio without starting the app eagerly.
async fn run_mcp(config: McpConfig) -> Result<(), EdevError> {
    let instance_registry = InstanceRegistry::register(&config.launch)?;
    let mut raw_state = State::new(config.launch, instance_registry);
    raw_state.enable_idle_shutdown(config.idle_shutdown_after);
    let state = Arc::new(AsyncMutex::new(raw_state));
    let server_state = Arc::clone(&state);
    let server = Server::new(move || EdevServer {
        state: Arc::clone(&server_state),
    });
    let server_future = server.serve_stdio();
    tokio::pin!(server_future);
    let idle_future = wait_for_idle_shutdown(Arc::clone(&state), config.idle_shutdown_after);
    tokio::pin!(idle_future);
    let result = tokio::select! {
        result = &mut server_future => result.map_err(EdevError::Mcp),
        _ = shutdown_signal() => Ok(()),
        _ = &mut idle_future => Ok(()),
    };
    {
        let mut state_guard = state.lock().await;
        if let Err(error) = state_guard.shutdown().await {
            if result.is_ok() {
                return Err(error);
            }
            eprintln!("edev: shutdown failed: {error}");
        }
    }
    result
}

/// Run the checked-in smoke suite once and exit non-zero on any smoke failure.
async fn run_smoke(config: SmokeConfig) -> Result<(), EdevError> {
    if config.list {
        return print_smoke_list(&config);
    }

    let launch = config.launch.clone().ok_or_else(|| {
        EdevError::InvalidArgs(
            "no app command configured; add app.command to .edev.toml or pass one after --"
                .to_string(),
        )
    })?;
    let instance_registry = InstanceRegistry::register(&launch)?;
    let mut state = State::new(launch.clone(), instance_registry);
    let client = start_proxy_target(&mut state, "smoke runner could not reach the app").await?;
    let bundle_context = config.bundle_dir.as_ref().and_then(|dir| {
        state.app.as_ref().map(|app| BundleContext {
            dir: dir.clone(),
            launch: launch.clone(),
            stderr_buffer: Arc::clone(&app.stderr_buffer),
            stdout_buffer: Arc::clone(&app.stdout_buffer),
            collection_timeout_ms: config
                .suite
                .script_timeout
                .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
                .unwrap_or(10_000),
        })
    });

    let result = run_smoke_suite(client, &config, bundle_context).await;
    let shutdown_result = state.shutdown().await;
    match (result, shutdown_result) {
        (Ok(summary), Ok(())) => {
            for line in summary.render_lines(config.verbose_output) {
                println!("{line}");
            }
            if summary.success() {
                Ok(())
            } else {
                Err(EdevError::SmokeFailed("smoke suite failed".to_string()))
            }
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
    }
}

/// Print discovered smoke scripts in text or JSON list format.
fn print_smoke_list(config: &SmokeConfig) -> Result<(), EdevError> {
    let scripts = discover_suite_scripts(&config.suite)?;
    if config.list_json {
        let output = serde_json::to_string_pretty(&scripts).map_err(|error| {
            EdevError::SmokeFailed(format!("failed to render list JSON: {error}"))
        })?;
        println!("{output}");
        return Ok(());
    }
    for script in scripts {
        println!("{}\t{}", script.path, script.size);
    }
    Ok(())
}

/// Run one Luau script through `script_eval`, print JSON, and write returned images.
async fn run_eval(config: EvalConfig) -> Result<(), EdevError> {
    let source = fs::read_to_string(&config.script)?;
    let instance_registry = InstanceRegistry::register(&config.launch)?;
    let mut state = State::new(config.launch.clone(), instance_registry);
    let client = start_proxy_target(&mut state, "eval command could not reach the app").await?;

    let result = run_eval_script(client, &config, source).await;
    let shutdown_result = state.shutdown().await;
    match (result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) | (Err(_), Err(error)) => Err(error),
    }
}

/// Launch the app, optionally apply a fixture, print a dump, and exit.
async fn run_dump(config: DumpConfig) -> Result<(), EdevError> {
    let instance_registry = InstanceRegistry::register(&config.launch)?;
    let mut state = State::new(config.launch.clone(), instance_registry);
    let client = start_proxy_target(&mut state, "dump command could not reach the app").await?;
    let result = run_dump_script(client, &config).await;
    let shutdown_result = state.shutdown().await;
    match (result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) | (Err(_), Err(error)) => Err(error),
    }
}

/// Execute the generated dump script and emit only the requested dump payload.
async fn run_dump_script(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    config: &DumpConfig,
) -> Result<(), EdevError> {
    let result = call_script_eval_result(
        &client,
        ScriptEvalRequest {
            script: dump_script(config),
            timeout_ms: config.timeout.map(|duration| duration.as_millis() as u64),
            options: Some(ScriptEvalOptions {
                source_name: Some("@edev_dump.luau".to_string()),
                args: Default::default(),
            }),
        },
    )
    .await
    .map_err(EdevError::EvalFailed)?;
    let outcome = parse_script_eval_outcome(&result).map_err(EdevError::EvalFailed)?;
    if !outcome.success {
        return Err(EdevError::EvalFailed(script_eval_error_message(
            outcome.error.as_ref(),
            "dump script failed",
        )));
    }
    let value = outcome
        .value
        .ok_or_else(|| EdevError::EvalFailed("dump script returned no value".to_string()))?;
    let output = dump_output(config, &value)?;
    emit_dump_output(config, &output)?;
    Ok(())
}

/// Build the internal Luau script used by `edev dump`.
fn dump_script(config: &DumpConfig) -> String {
    let mut lines = Vec::new();
    if let Some(fixture) = &config.fixture {
        let fixture = luau_string(fixture);
        if let Some(params) = luau_fixture_params(&config.params) {
            lines.push(format!("fixture({fixture}, {params})"));
        } else {
            lines.push(format!("fixture({fixture})"));
        }
    } else if config.wait_for_capture {
        lines.push("wait_for_capture()".to_string());
    }
    lines.push(format!("return {}", dump_call(config)));
    lines.join("\n")
}

/// Build the final dump helper call for the generated Luau script.
fn dump_call(config: &DumpConfig) -> String {
    let function = if config.json { "dump" } else { "dump_text" };
    let Some(viewport) = &config.viewport else {
        return format!("{function}()");
    };
    format!("{function}({{ viewport = {} }})", luau_string(viewport))
}

/// Convert the script return value into the exact CLI payload.
fn dump_output(config: &DumpConfig, value: &serde_json::Value) -> Result<String, EdevError> {
    if config.json {
        return serde_json::to_string_pretty(value).map_err(|error| {
            EdevError::EvalFailed(format!("failed to encode dump JSON: {error}"))
        });
    }
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| EdevError::EvalFailed("dump_text returned a non-string value".to_string()))
}

/// Write dump output to the configured destination.
fn emit_dump_output(config: &DumpConfig, output: &str) -> Result<(), EdevError> {
    if let Some(path) = &config.out {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, output)?;
    } else {
        println!("{output}");
    }
    Ok(())
}

/// Quote a Rust string as a Luau string literal.
fn luau_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

/// Render typed fixture params as a Luau table literal.
fn luau_fixture_params(params: &BTreeMap<String, ScriptArgValue>) -> Option<String> {
    if params.is_empty() {
        return None;
    }
    let entries = params
        .iter()
        .map(|(key, value)| format!("[{}] = {}", luau_string(key), luau_scalar(value)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("{{ {entries} }}"))
}

/// Render one typed scalar as a Luau literal.
fn luau_scalar(value: &ScriptArgValue) -> String {
    match value {
        ScriptArgValue::String(value) => luau_string(value),
        ScriptArgValue::Int(value) => value.to_string(),
        ScriptArgValue::Float(value) => value.to_string(),
        ScriptArgValue::Bool(value) => value.to_string(),
    }
}

/// Execute one script against a launched app and emit the eval result.
async fn run_eval_script(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    config: &EvalConfig,
    source: String,
) -> Result<(), EdevError> {
    let result = call_script_eval_result(
        &client,
        ScriptEvalRequest {
            script: source,
            timeout_ms: config.timeout.map(|duration| duration.as_millis() as u64),
            options: Some(ScriptEvalOptions {
                source_name: Some(config.script.display().to_string()),
                args: config.args.clone(),
            }),
        },
    )
    .await
    .map_err(EdevError::EvalFailed)?;
    let outcome = parse_script_eval_outcome(&result).map_err(EdevError::EvalFailed)?;
    let image_files = write_eval_images(config, &result, &outcome)?;
    let output = eval_output_value(&outcome, &image_files)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .map_err(|error| EdevError::EvalFailed(format!("failed to encode JSON: {error}")))?
    );
    if outcome.success {
        Ok(())
    } else {
        Err(EdevError::EvalFailed(script_eval_error_message(
            outcome.error.as_ref(),
            "script evaluation failed",
        )))
    }
}

/// Start the app, list or apply a fixture, then either exit or wait for ctrl-c.
async fn run_fixture(config: FixtureConfig) -> Result<(), EdevError> {
    let instance_registry = InstanceRegistry::register(&config.launch)?;
    let mut state = State::new(config.launch.clone(), instance_registry);
    let client = start_proxy_target(&mut state, "fixture command could not reach the app").await?;

    // Query registered fixtures.
    let fixtures =
        match eval_fixture_script(&client, "return fixtures()", "failed to query fixtures")
            .await
            .and_then(parse_fixture_list)
        {
            Ok(fixtures) => fixtures,
            Err(error) => {
                state.shutdown().await?;
                return Err(error);
            }
        };

    if fixtures.is_empty() {
        if config.json || config.markdown {
            print_fixture_list(&config, &fixtures)?;
        } else {
            println!("No fixtures registered.");
        }
        state.shutdown().await?;
        return Ok(());
    }

    let Some(name) = config.name else {
        // List-only mode.
        print_fixture_list(&config, &fixtures)?;
        state.shutdown().await?;
        return Ok(());
    };

    // Validate the fixture name exists.
    let Some(fixture) = fixtures.iter().find(|f| f.name == name) else {
        eprintln!("error: unknown fixture \"{name}\"\n");
        print_fixture_table(&fixtures);
        state.shutdown().await?;
        return Err(EdevError::FixtureFailed(format!("unknown fixture: {name}")));
    };

    let report = match call_fixture_tool(&client, &name, &config.params, !config.no_wait).await {
        Ok(report) => report,
        Err(error) => {
            state.shutdown().await?;
            return Err(error);
        }
    };
    print_fixture_apply_report(fixture, &report);
    if config.no_wait {
        println!("anchors: not waited (--no-wait)");
    }

    if config.dump {
        match eval_fixture_script(&client, "return dump_text()", "post-fixture dump failed").await {
            Ok(outcome) => print_fixture_dump(outcome)?,
            Err(error) => {
                state.shutdown().await?;
                return Err(error);
            }
        }
    }

    eprintln!("Fixture \"{name}\" applied. Press ctrl-c to stop.");
    shutdown_signal().await;
    state.shutdown().await?;
    Ok(())
}

/// Apply one fixture through the target app's typed MCP fixture tool.
async fn call_fixture_tool(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    name: &str,
    params: &BTreeMap<String, ScriptArgValue>,
    wait_for_anchors: bool,
) -> Result<FixtureApplyReport, EdevError> {
    let request = FixtureToolRequest {
        name,
        params: fixture_param_values(params),
        timeout_ms: None,
    };
    let tool_name = if wait_for_anchors {
        "fixture"
    } else {
        "fixture_apply"
    };
    let arguments = Arguments::from_struct(request).map_err(|error| {
        EdevError::FixtureFailed(format!("failed to encode fixture request: {error}"))
    })?;
    let result = client
        .lock()
        .await
        .call_tool(tool_name.to_string(), arguments)
        .await
        .map_err(|error| EdevError::FixtureFailed(error.to_string()))?;
    if result.is_error() {
        return Err(EdevError::FixtureFailed(tool_result_error_message(
            &result,
            "fixture application failed",
        )));
    }
    parse_structured_tool_result(&result, tool_name).map_err(|error| {
        EdevError::FixtureFailed(format!("failed to decode fixture result: {error}"))
    })
}

/// Convert CLI scalar args into typed fixture values.
fn fixture_param_values(
    params: &BTreeMap<String, ScriptArgValue>,
) -> BTreeMap<String, WidgetValue> {
    params
        .iter()
        .map(|(key, value)| (key.clone(), script_arg_to_widget_value(value)))
        .collect()
}

/// Convert one parsed CLI scalar into the fixture value wire type.
fn script_arg_to_widget_value(value: &ScriptArgValue) -> WidgetValue {
    match value {
        ScriptArgValue::String(value) => WidgetValue::Text(value.clone()),
        ScriptArgValue::Int(value) => WidgetValue::Int(*value),
        ScriptArgValue::Float(value) => WidgetValue::Float(*value),
        ScriptArgValue::Bool(value) => WidgetValue::Bool(*value),
    }
}

/// Extract a useful message from an errored tool result.
fn tool_result_error_message(result: &CallToolResult, fallback_message: &str) -> String {
    result
        .text()
        .map(str::to_string)
        .or_else(|| {
            result
                .structured_content
                .as_ref()
                .and_then(extract_error_message)
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback_message.to_string())
}

/// Decode a structured MCP tool result, accepting text JSON as a fallback.
fn parse_structured_tool_result<T: DeserializeOwned>(
    result: &CallToolResult,
    tool_name: &str,
) -> Result<T, String> {
    let payload = if let Some(structured) = &result.structured_content {
        structured.clone()
    } else {
        let Some(text) = result.text() else {
            return Err(format!("{tool_name} response was missing JSON content"));
        };
        serde_json::from_str(text)
            .map_err(|error| format!("failed to parse {tool_name} response: {error}"))?
    };
    serde_json::from_value(payload)
        .map_err(|error| format!("failed to decode {tool_name} response: {error}"))
}

/// Print the textual dump returned by `dump_text()`.
fn print_fixture_dump(outcome: ScriptEvalOutcome) -> Result<(), EdevError> {
    let value = outcome
        .value
        .ok_or_else(|| EdevError::FixtureFailed("dump_text() returned no value".to_string()))?;
    let text = value.as_str().ok_or_else(|| {
        EdevError::FixtureFailed("dump_text() returned a non-string value".to_string())
    })?;
    println!();
    println!("{text}");
    Ok(())
}

/// Print the typed fixture application report.
fn print_fixture_apply_report(fixture: &FixtureSpec, report: &FixtureApplyReport) {
    println!("Fixture: {}", fixture.name);
    if !fixture.description.is_empty() {
        println!("{}", fixture.description);
    }
    if !report.params.is_empty() {
        println!("params:");
        for (name, value) in &report.params {
            println!("  {name}: {}", format_widget_value(value));
        }
    }
    if !report.values.is_empty() {
        println!("values:");
        for (name, value) in &report.values {
            println!("  {name}: {}", format_widget_value(value));
        }
    }
    if !report.anchors.is_empty() {
        println!("anchors:");
        for anchor in &report.anchors {
            println!("  {}", format_anchor_report(anchor));
        }
    }
}

/// Format one readiness anchor returned by the fixture tool.
fn format_anchor_report(anchor: &FixtureAnchorReport) -> String {
    let state = if anchor.satisfied { "ok" } else { "pending" };
    let target = match &anchor.viewport_id {
        Some(viewport_id) => format!("{} in {}", anchor.widget_id, viewport_id),
        None => anchor.widget_id.clone(),
    };
    format!("[{state}] {target} {} - {}", anchor.check, anchor.detail)
}

/// Print fixture metadata in the requested list format.
fn print_fixture_list(config: &FixtureConfig, fixtures: &[FixtureSpec]) -> Result<(), EdevError> {
    if config.json {
        println!("{}", pretty_json(&fixtures)?);
    } else if config.markdown {
        print_fixture_markdown(fixtures);
    } else {
        print_fixture_table(fixtures);
    }
    Ok(())
}

/// Print fixture metadata as a Markdown table.
fn print_fixture_markdown(fixtures: &[FixtureSpec]) {
    println!("| Fixture | Description | Params | Tags | Anchors |");
    println!("| --- | --- | --- | --- | --- |");
    for fixture in fixtures {
        println!(
            "| {} | {} | {} | {} | {} |",
            markdown_cell(&fixture.name),
            markdown_cell(&fixture.description),
            markdown_cell(&fixture_params_summary(&fixture.params)),
            markdown_cell(&fixture.tags.join(", ")),
            fixture.anchors.len()
        );
    }
}

/// Escape a Markdown table cell.
fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|")
}

/// Request sent to the app-side `fixture` MCP tool.
#[derive(Debug, Serialize)]
struct FixtureToolRequest<'a> {
    /// Fixture name to apply.
    name: &'a str,
    /// Validated user-supplied fixture params.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, WidgetValue>,
    /// Optional app-side wait timeout override.
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

/// Structured response from the app-side `fixture` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FixtureApplyReport {
    /// Validated params used by the fixture handler.
    #[serde(default)]
    params: BTreeMap<String, WidgetValue>,
    /// Values returned by the fixture handler.
    #[serde(default)]
    values: BTreeMap<String, WidgetValue>,
    /// Final readiness state for static and dynamic anchors.
    #[serde(default)]
    anchors: Vec<FixtureAnchorReport>,
}

/// Readiness state for one anchor after applying a fixture.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FixtureAnchorReport {
    /// Target widget id.
    widget_id: String,
    /// Optional target viewport id.
    #[serde(default)]
    viewport_id: Option<String>,
    /// Anchor check kind.
    check: String,
    /// Whether the check was satisfied.
    satisfied: bool,
    /// Human-readable state detail.
    detail: String,
    /// Raw state snapshot, when available.
    #[serde(default)]
    current_state: Option<serde_json::Value>,
}

/// Summarize all declared fixture params for table output.
fn fixture_params_summary(params: &[FixtureParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(fixture_param_summary)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Summarize one declared fixture param for table output.
fn fixture_param_summary(param: &FixtureParam) -> String {
    let mut parts = vec![format!("{}: {}", param.name, param_kind_name(param.kind))];
    if let Some(default) = &param.default {
        parts.push(format!("default {}", format_widget_value(default)));
    }
    if !param.choices.is_empty() {
        parts.push(format!(
            "choices {}",
            param
                .choices
                .iter()
                .map(format_widget_value)
                .collect::<Vec<_>>()
                .join("/")
        ));
    }
    if param.min.is_some() || param.max.is_some() {
        parts.push(format!(
            "range {}..{}",
            param
                .min
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-inf".to_string()),
            param
                .max
                .map(|value| value.to_string())
                .unwrap_or_else(|| "inf".to_string())
        ));
    }
    parts.join(" ")
}

/// Return the CLI display name for a fixture param kind.
fn param_kind_name(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::Bool => "bool",
        ParamKind::Int => "int",
        ParamKind::Float => "float",
        ParamKind::Text => "text",
    }
}

/// Format a fixture value for human-readable CLI output.
fn format_widget_value(value: &WidgetValue) -> String {
    match value {
        WidgetValue::Text(value) => format!("{value:?}"),
        WidgetValue::Bool(_) | WidgetValue::Float(_) | WidgetValue::Int(_) => value.to_text(),
    }
}

/// Start the app and resolve the proxied client, shutting down on startup failures.
async fn start_proxy_target(
    state: &mut State,
    unavailable_message: &str,
) -> Result<Arc<AsyncMutex<tmcp::Client<()>>>, EdevError> {
    match state.restart().await? {
        LifecycleStartStatus::Running => {}
        LifecycleStartStatus::StartupFailed(output) => {
            state.shutdown().await?;
            return Err(EdevError::AppStart(output));
        }
    }

    match state.proxy_target() {
        Ok(client) => Ok(client),
        Err(error) => {
            let message = error.text().unwrap_or(unavailable_message).to_string();
            state.shutdown().await?;
            Err(EdevError::AppStart(message))
        }
    }
}

/// Call `script_eval` on the connected app and parse the outcome.
async fn call_script_eval(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    script: &str,
    timeout_ms: Option<u64>,
) -> Result<ScriptEvalOutcome, String> {
    let result = call_script_eval_result(
        client,
        ScriptEvalRequest {
            script: script.to_string(),
            timeout_ms: timeout_ms.or(Some(10_000)),
            options: None,
        },
    )
    .await?;
    parse_script_eval_outcome(&result)
}

/// Call the app-side `script_eval` tool and preserve all returned content blocks.
async fn call_script_eval_result(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    request: ScriptEvalRequest,
) -> Result<CallToolResult, String> {
    let request = script_eval_request_value(request);
    let client = client.lock().await;
    client
        .call_tool("script_eval".to_string(), request)
        .await
        .map_err(|error| error.to_string())
}

/// Decode image content blocks from the eval result into deterministic files.
fn write_eval_images(
    config: &EvalConfig,
    result: &CallToolResult,
    outcome: &ScriptEvalOutcome,
) -> Result<BTreeMap<String, PathBuf>, EdevError> {
    let Some(images) = outcome.images.as_ref() else {
        return Ok(BTreeMap::new());
    };
    fs::create_dir_all(&config.out_dir)?;
    let mut files = BTreeMap::new();
    let stem = config
        .script
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("script");
    for image in images {
        let block = result.content.get(image.content_index).ok_or_else(|| {
            EdevError::EvalFailed(format!(
                "image {} referenced missing content block {}",
                image.id, image.content_index
            ))
        })?;
        let ContentBlock::Image(content) = block else {
            return Err(EdevError::EvalFailed(format!(
                "image {} referenced non-image content block {}",
                image.id, image.content_index
            )));
        };
        let path = config.out_dir.join(format!(
            "{}-{}.{}",
            safe_file_component(stem),
            safe_file_component(&image.id),
            image_extension(&content.mime_type)
        ));
        let bytes = content.data_bytes().map_err(|error| {
            EdevError::EvalFailed(format!("failed to decode image {}: {error}", image.id))
        })?;
        fs::write(&path, bytes)?;
        files.insert(image.id.clone(), path);
    }
    Ok(files)
}

/// Add image file paths to the printed eval JSON.
fn eval_output_value(
    outcome: &ScriptEvalOutcome,
    image_files: &BTreeMap<String, PathBuf>,
) -> Result<serde_json::Value, EdevError> {
    let mut value = serde_json::to_value(outcome).map_err(|error| {
        EdevError::EvalFailed(format!("failed to serialize eval outcome: {error}"))
    })?;
    let Some(images) = value
        .get_mut("images")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Ok(value);
    };
    for image in images {
        let Some(id) = image.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(path) = image_files.get(id) else {
            continue;
        };
        if let Some(image) = image.as_object_mut() {
            image.insert(
                "file".to_string(),
                serde_json::Value::String(path.display().to_string()),
            );
        }
    }
    Ok(value)
}

/// Information needed to write a failure bundle while the app is still running.
#[derive(Clone)]
struct BundleContext {
    /// Root directory for all failure bundles in this smoke run.
    dir: PathBuf,
    /// App launch settings to record in `meta.json`.
    launch: LaunchConfig,
    /// Tail-capped app stderr captured since launch.
    stderr_buffer: Arc<Mutex<Vec<u8>>>,
    /// Tail-capped app stdout captured since launch when available.
    stdout_buffer: Arc<Mutex<Vec<u8>>>,
    /// Timeout for bundle collection script evaluation.
    collection_timeout_ms: u64,
}

/// Payload returned by the internal bundle snapshot collection script.
#[derive(Debug, Deserialize)]
struct BundleSnapshotCollection {
    /// Full structured tree dump.
    tree: serde_json::Value,
    /// Full text tree dump.
    text: String,
    /// Viewport screenshots captured by the collection script.
    #[serde(deserialize_with = "deserialize_bundle_shots")]
    shots: Vec<BundleShot>,
    /// Non-fatal screenshot collection errors.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_bundle_errors")]
    errors: Vec<BundleCollectionError>,
}

/// One viewport screenshot entry from the collection script.
#[derive(Debug, Deserialize)]
struct BundleShot {
    /// Canonical viewport id such as `root` or `vp:<hex>`.
    viewport_id: Option<String>,
    /// Semantic viewport name when one was registered.
    name: Option<String>,
    /// Image reference returned by `Viewport:screenshot()`.
    image: BundleImageRef,
}

/// Image reference returned by the Luau script runtime.
#[derive(Debug, Deserialize)]
struct BundleImageRef {
    /// Runtime image id used to find the corresponding MCP image block.
    id: String,
}

/// Non-fatal bundle collection error returned by an internal script.
#[derive(Debug, Deserialize, Serialize)]
struct BundleCollectionError {
    /// Collection phase that failed.
    kind: String,
    /// Viewport id involved in the failure when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport_id: Option<String>,
    /// Semantic viewport name involved in the failure when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Human-readable error message.
    message: String,
}

/// Internal Luau script used to collect frame artifacts after a smoke failure.
const BUNDLE_COLLECTION_SCRIPT: &str = r#"
wait_for_capture()
local shots = {}
local errors = {}
for _, viewport in ipairs(viewports()) do
    local state = viewport:state()
    local ok, image = pcall(function()
        return viewport:screenshot()
    end)
    if ok then
        table.insert(shots, {
            viewport_id = viewport.id,
            name = state.name,
            image = image,
        })
    else
        table.insert(errors, {
            kind = "screenshot",
            viewport_id = viewport.id,
            name = state.name,
            message = tostring(image),
        })
    end
end
return {
    tree = dump({ fields = "full" }),
    text = dump_text({ fields = "full" }),
    shots = shots,
    errors = errors,
}
"#;

/// Internal Luau script used to collect diagnostics after frame artifacts are written.
const BUNDLE_DIAGNOSTICS_SCRIPT: &str = r#"
return diagnostics()
"#;

/// Deserialize Luau's ambiguous empty table as an empty screenshot list.
fn deserialize_bundle_shots<'de, D>(deserializer: D) -> Result<Vec<BundleShot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_luau_array(deserializer, "screenshot")
}

/// Deserialize Luau's ambiguous empty table as an empty collection error list.
fn deserialize_bundle_errors<'de, D>(
    deserializer: D,
) -> Result<Vec<BundleCollectionError>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_luau_array(deserializer, "collection error")
}

/// Deserialize a Luau array while accepting an empty table encoded as `{}`.
fn deserialize_luau_array<'de, D, T>(deserializer: D, label: &str) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(SerdeDeError::custom),
        serde_json::Value::Object(map) if map.is_empty() => Ok(Vec::new()),
        other => Err(SerdeDeError::custom(format!(
            "expected {label} array, got {other}"
        ))),
    }
}

/// Convert arbitrary script/image names into portable path components.
fn safe_file_component(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if safe.is_empty() {
        "image".to_string()
    } else {
        safe
    }
}

/// Choose a file extension for an MCP image content MIME type.
fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// Write one deterministic failure bundle for a failed smoke script.
async fn write_failure_bundle(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    context: &BundleContext,
    script_path: &str,
    round: Option<u32>,
    args: &ScriptArgs,
    outcome: &ScriptEvalOutcome,
) -> Result<(), EdevError> {
    let bundle_key = match round {
        Some(round) => format!("{script_path}-round-{round}"),
        None => script_path.to_string(),
    };
    let bundle_dir = context.dir.join(format!(
        "{}-{}",
        safe_file_component(&bundle_key),
        stable_hash8(&bundle_key)
    ));
    replace_dir(&bundle_dir)?;
    fs::write(
        bundle_dir.join("meta.json"),
        bundle_meta(context, script_path, round, args, outcome)?,
    )?;
    fs::write(bundle_dir.join("failure.txt"), failure_text(outcome)?)?;
    fs::write(
        bundle_dir.join("app.stderr.log"),
        snapshot_output(&context.stderr_buffer),
    )?;
    fs::write(
        bundle_dir.join("app.stdout.log"),
        stdout_bundle_text(&context.stdout_buffer),
    )?;

    let collection_result = match call_script_eval_result(
        client,
        ScriptEvalRequest {
            script: BUNDLE_COLLECTION_SCRIPT.to_string(),
            timeout_ms: Some(context.collection_timeout_ms),
            options: Some(ScriptEvalOptions {
                source_name: Some(format!("<bundle:{script_path}>")),
                args: ScriptArgs::default(),
            }),
        },
    )
    .await
    {
        Ok(result) => Some(result),
        Err(message) => {
            fs::write(
                bundle_dir.join("collection-error.txt"),
                format!("bundle collection failed: {message}\n"),
            )?;
            None
        }
    };

    let collection = match collection_result.as_ref() {
        Some(result) => match parse_script_eval_outcome(result) {
            Ok(collection_outcome) if collection_outcome.success => {
                match collection_outcome.value {
                    Some(value) => {
                        match serde_json::from_value::<BundleSnapshotCollection>(value) {
                            Ok(collection) => Some(collection),
                            Err(error) => {
                                append_collection_error(
                                    &bundle_dir,
                                    format!("invalid bundle payload: {error}\n"),
                                )?;
                                None
                            }
                        }
                    }
                    None => {
                        append_collection_error(
                            &bundle_dir,
                            "bundle collection script returned no value\n",
                        )?;
                        None
                    }
                }
            }
            Ok(collection_outcome) => {
                append_collection_error(
                    &bundle_dir,
                    format!(
                        "bundle collection script failed: {}\n",
                        script_eval_error_message(collection_outcome.error.as_ref(), "failed")
                    ),
                )?;
                None
            }
            Err(error) => {
                append_collection_error(
                    &bundle_dir,
                    format!("failed to decode bundle collection result: {error}\n"),
                )?;
                None
            }
        },
        None => None,
    };

    if let (Some(result), Some(collection)) = (collection_result.as_ref(), collection.as_ref()) {
        fs::write(bundle_dir.join("tree.json"), pretty_json(&collection.tree)?)?;
        fs::write(bundle_dir.join("tree.txt"), &collection.text)?;
        if !collection.errors.is_empty() {
            append_collection_error(
                &bundle_dir,
                format!(
                    "bundle snapshot collection errors:\n{}",
                    pretty_json(&collection.errors)?
                ),
            )?;
        }
        if let Err(error) = write_bundle_images(&bundle_dir, result, collection) {
            append_collection_error(
                &bundle_dir,
                format!("bundle image extraction failed: {error}\n"),
            )?;
        }
    } else {
        fs::write(bundle_dir.join("tree.json"), "{}\n")?;
        fs::write(bundle_dir.join("tree.txt"), "bundle collection failed\n")?;
    }
    fs::write(
        bundle_dir.join("diagnostics.json"),
        pretty_json(&collect_bundle_diagnostics(client, context, script_path).await?)?,
    )?;
    Ok(())
}

/// Collect diagnostics for a bundle without coupling them to tree/screenshot capture.
async fn collect_bundle_diagnostics(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    context: &BundleContext,
    script_path: &str,
) -> Result<serde_json::Value, EdevError> {
    let fallback = serde_json::json!({
        "values": {},
        "errors": {
            "_collection": {
                "code": "collection_failed",
                "message": "bundle diagnostics collection failed",
            },
        },
    });
    let result = match call_script_eval_result(
        client,
        ScriptEvalRequest {
            script: BUNDLE_DIAGNOSTICS_SCRIPT.to_string(),
            timeout_ms: Some(context.collection_timeout_ms),
            options: Some(ScriptEvalOptions {
                source_name: Some(format!("<bundle-diagnostics:{script_path}>")),
                args: ScriptArgs::default(),
            }),
        },
    )
    .await
    {
        Ok(result) => result,
        Err(message) => {
            return Ok(serde_json::json!({
                "values": {},
                "errors": {
                    "_collection": {
                        "code": "collection_failed",
                        "message": format!("bundle diagnostics failed: {message}"),
                    },
                },
            }));
        }
    };
    let outcome = match parse_script_eval_outcome(&result) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Ok(serde_json::json!({
                "values": {},
                "errors": {
                    "_collection": {
                        "code": "collection_failed",
                        "message": format!("failed to decode bundle diagnostics: {error}"),
                    },
                },
            }));
        }
    };
    if !outcome.success {
        return Ok(serde_json::json!({
            "values": {},
            "errors": {
                "_collection": {
                    "code": "collection_failed",
                    "message": script_eval_error_message(outcome.error.as_ref(), "bundle diagnostics failed"),
                },
            },
        }));
    }
    Ok(outcome.value.unwrap_or(fallback))
}

/// Append one collection warning to `collection-error.txt`.
fn append_collection_error(bundle_dir: &Path, message: impl AsRef<str>) -> Result<(), EdevError> {
    use std::io::Write;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(bundle_dir.join("collection-error.txt"))?;
    file.write_all(message.as_ref().as_bytes())?;
    Ok(())
}

/// Replace a deterministic bundle directory with an empty directory.
fn replace_dir(path: &Path) -> Result<(), EdevError> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

/// Build `meta.json` for a failure bundle.
fn bundle_meta(
    context: &BundleContext,
    script_path: &str,
    round: Option<u32>,
    args: &ScriptArgs,
    outcome: &ScriptEvalOutcome,
) -> Result<String, EdevError> {
    let script = match round {
        Some(round) => serde_json::json!({
            "path": script_path,
            "round": round,
            "args": args,
        }),
        None => serde_json::json!({
            "path": script_path,
            "args": args,
        }),
    };
    let value = serde_json::json!({
        "script": script,
        "fixtures": &outcome.fixtures,
        "app": {
            "command": &context.launch.command,
            "cwd": context.launch.cwd.display().to_string(),
        },
        "eguidev_version": env!("CARGO_PKG_VERSION"),
        "failure": {
            "message": script_eval_error_message(outcome.error.as_ref(), "script failed"),
            "details": outcome.error.as_ref().and_then(|error| error.details.clone()),
            "error": &outcome.error,
        },
    });
    pretty_json(&value)
}

/// Render the human-readable failure summary for `failure.txt`.
fn failure_text(outcome: &ScriptEvalOutcome) -> Result<String, EdevError> {
    let mut text = String::new();
    text.push_str(&format!(
        "failure: {}\n",
        script_eval_error_message(outcome.error.as_ref(), "script failed")
    ));
    if let Some(error) = &outcome.error {
        if let Some(code) = &error.code {
            text.push_str(&format!("code: {code}\n"));
        }
        if let Some(location) = &error.location {
            text.push_str(&format!(
                "location: {}:{}\n",
                location.line,
                location.column.unwrap_or(1)
            ));
        }
        if let Some(details) = &error.details {
            text.push_str("\ndetails:\n");
            text.push_str(&pretty_json(details)?);
        }
    }
    if !outcome.logs.is_empty() {
        text.push_str("\nlogs:\n");
        for log in &outcome.logs {
            text.push_str("- ");
            text.push_str(log);
            text.push('\n');
        }
    }
    if !outcome.assertions.is_empty() {
        text.push_str("\nassertions:\n");
        text.push_str(&pretty_json(&outcome.assertions)?);
    }
    if !outcome.fixtures.is_empty() {
        text.push_str("\nfixtures:\n");
        text.push_str(&pretty_json(&outcome.fixtures)?);
    }
    Ok(text)
}

/// Write viewport screenshot image blocks referenced by the collection payload.
fn write_bundle_images(
    bundle_dir: &Path,
    result: &CallToolResult,
    collection: &BundleSnapshotCollection,
) -> Result<(), EdevError> {
    for shot in &collection.shots {
        let image = collection_image_content(result, &shot.image.id)?;
        let name = shot
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(shot.viewport_id.as_deref())
            .unwrap_or(&shot.image.id);
        let path = bundle_dir.join(format!(
            "viewport-{}.{}",
            safe_file_component(name),
            image_extension(&image.mime_type)
        ));
        let bytes = image.data_bytes().map_err(|error| {
            EdevError::EvalFailed(format!("failed to decode image {}: {error}", shot.image.id))
        })?;
        fs::write(path, bytes)?;
    }
    Ok(())
}

/// Return the MCP image content block for a collected image id.
fn collection_image_content<'a>(
    result: &'a CallToolResult,
    image_id: &str,
) -> Result<&'a ImageContent, EdevError> {
    let outcome = parse_script_eval_outcome(result).map_err(EdevError::EvalFailed)?;
    let image = outcome
        .images
        .as_ref()
        .and_then(|images| images.iter().find(|image| image.id == image_id))
        .ok_or_else(|| EdevError::EvalFailed(format!("missing bundle image {image_id}")))?;
    let block = result.content.get(image.content_index).ok_or_else(|| {
        EdevError::EvalFailed(format!(
            "image {} referenced missing content block {}",
            image.id, image.content_index
        ))
    })?;
    let ContentBlock::Image(content) = block else {
        return Err(EdevError::EvalFailed(format!(
            "image {} referenced non-image content block {}",
            image.id, image.content_index
        )));
    };
    Ok(content)
}

/// Serialize bundle JSON with a trailing newline for stable files.
fn pretty_json(value: &impl Serialize) -> Result<String, EdevError> {
    let mut text = serde_json::to_string_pretty(value).map_err(|error| {
        EdevError::EvalFailed(format!("failed to serialize bundle JSON: {error}"))
    })?;
    text.push('\n');
    Ok(text)
}

/// Stable eight-hex hash for deterministic bundle directory names.
fn stable_hash8(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

/// Run a fixture-related script and convert script failures into fixture errors.
async fn eval_fixture_script(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    script: &str,
    fallback_message: &str,
) -> Result<ScriptEvalOutcome, EdevError> {
    match call_script_eval(client, script, None).await {
        Ok(outcome) if outcome.success => Ok(outcome),
        Ok(outcome) => Err(EdevError::FixtureFailed(script_eval_error_message(
            outcome.error.as_ref(),
            fallback_message,
        ))),
        Err(message) => Err(EdevError::FixtureFailed(message)),
    }
}

/// Decode the `fixtures()` result into the checked-in fixture metadata shape.
fn parse_fixture_list(outcome: ScriptEvalOutcome) -> Result<Vec<FixtureSpec>, EdevError> {
    serde_json::from_value(
        outcome
            .value
            .ok_or_else(|| EdevError::FixtureFailed("fixtures() returned no value".to_string()))?,
    )
    .map_err(|error| EdevError::FixtureFailed(format!("failed to decode fixtures list: {error}")))
}

/// Prefer the runtime's script error text and fall back to a caller-provided message.
fn script_eval_error_message(error: Option<&ScriptErrorInfo>, fallback_message: &str) -> String {
    error
        .map(|error| error.message.as_str())
        .unwrap_or(fallback_message)
        .to_string()
}

/// Print a formatted fixture table to stdout.
fn print_fixture_table(fixtures: &[FixtureSpec]) {
    let max_name = fixtures
        .iter()
        .map(|f| f.name.len())
        .max()
        .unwrap_or(0)
        .max(4);
    for f in fixtures {
        let mut details = Vec::new();
        if !f.params.is_empty() {
            details.push(format!(
                "{} param{}",
                f.params.len(),
                if f.params.len() == 1 { "" } else { "s" }
            ));
        }
        if !f.tags.is_empty() {
            details.push(format!("tags: {}", f.tags.join(", ")));
        }
        if !f.anchors.is_empty() {
            details.push(format!(
                "{} anchor{}",
                f.anchors.len(),
                if f.anchors.len() == 1 { "" } else { "s" }
            ));
        }
        let details = if details.is_empty() {
            String::new()
        } else {
            format!(" [{}]", details.join("; "))
        };
        if f.description.is_empty() {
            println!("  {}{}", f.name, details);
        } else {
            println!(
                "  {:width$}  {}{}",
                f.name,
                f.description,
                details,
                width = max_name
            );
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Errors returned by the edev launcher.
pub enum EdevError {
    /// Argument parsing error.
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// MCP transport error.
    #[error("mcp error: {0}")]
    Mcp(#[from] McpError),
    /// IO error.
    #[error("io error: {0}")]
    Io(#[from] std_io::Error),
    /// Application startup error.
    #[error("app start failed: {0}")]
    AppStart(String),
    /// Smoke suite failure.
    #[error("smoke failed: {0}")]
    SmokeFailed(String),
    /// One-shot script evaluation failure.
    #[error("eval failed: {0}")]
    EvalFailed(String),
    /// Fixture operation failure.
    #[error("fixture failed: {0}")]
    FixtureFailed(String),
    /// Instance registry error.
    #[error("instance registry error: {0}")]
    InstanceRegistry(String),
}

/// Render the checked-in Luau API definitions, highlighting them for terminal output when possible.
fn render_script_docs() -> String {
    let definitions = script_definitions();
    if !std_io::stdout().is_terminal() || env::var_os("NO_COLOR").is_some() {
        return definitions.to_string();
    }
    highlight_script_definitions(definitions).unwrap_or_else(|| definitions.to_string())
}

/// Apply terminal syntax highlighting to checked-in Luau definitions when the output is a TTY.
fn highlight_script_definitions(definitions: &str) -> Option<String> {
    let syntax_set = SyntaxSet::load_defaults_newlines();
    let syntax = syntax_set
        .find_syntax_by_extension("luau")
        .or_else(|| syntax_set.find_syntax_by_extension("lua"))?;
    let theme_set = ThemeSet::load_defaults();
    let theme = [
        "base16-eighties.dark",
        "Solarized (dark)",
        "base16-ocean.dark",
        "Monokai Extended",
    ]
    .into_iter()
    .find_map(|name| theme_set.themes.get(name))
    .or_else(|| theme_set.themes.values().next())?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut rendered = String::new();
    for line in LinesWithEndings::from(definitions) {
        let ranges = highlighter.highlight_line(line, &syntax_set).ok()?;
        rendered.push_str(&as_24_bit_terminal_escaped(&ranges, false));
    }
    Some(rendered)
}

#[derive(Clone, Debug, Default)]
/// Lightweight logger for app and launcher messages.
struct LogState {
    /// Whether launcher lifecycle logs should be emitted.
    verbose: bool,
}

impl LogState {
    /// Build a logger that conditionally emits messages.
    fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    /// Record a single log line, preserving newlines when present.
    fn record_line(&self, line: &str) {
        if !self.verbose {
            return;
        }
        if line.ends_with('\n') || line.ends_with('\r') {
            eprint!("{line}");
        } else {
            eprintln!("{line}");
        }
    }
}

/// Mutable runtime state for the edev process manager.
struct State {
    /// Launcher configuration.
    config: LaunchConfig,
    /// Instance registry entry for this launcher.
    instance_registry: InstanceRegistry,
    /// Active app process, if running.
    app: Option<AppProcess>,
    /// Current lifecycle status.
    status: AppStatus,
    /// Timestamp of the last MCP interaction handled by this launcher.
    last_activity: Instant,
    /// Configured idle guard for the launcher, if this is an MCP session.
    idle_shutdown_after: Option<Duration>,
    /// Whether the stdio MCP client completed initialization.
    mcp_client_attached: bool,
    /// Logger for launcher lifecycle messages.
    log_state: LogState,
}

/// Future returned by the app spawn helper.
type SpawnFuture<'a> = Pin<Box<dyn Future<Output = Result<AppProcess, AppStartError>> + Send + 'a>>;

/// Future returned by MCP server handlers.
impl State {
    /// Create a new runtime state from the provided configuration.
    fn new(config: LaunchConfig, instance_registry: InstanceRegistry) -> Self {
        let log_state = LogState::new(config.verbose);
        Self {
            config,
            instance_registry,
            app: None,
            status: AppStatus::NotRunning,
            last_activity: Instant::now(),
            idle_shutdown_after: None,
            mcp_client_attached: false,
            log_state,
        }
    }

    /// Enable the MCP pre-client idle guard.
    fn enable_idle_shutdown(&mut self, idle_after: Duration) {
        self.idle_shutdown_after = Some(idle_after);
    }

    /// Record an internal launcher log line into the shared log buffer.
    fn log_edev(&self, line: impl AsRef<str>) {
        let line = line.as_ref();
        self.log_state.record_line(&format!("edev: {line}"));
    }

    /// Mark launcher activity from user-visible tool execution.
    fn mark_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Mark the stdio MCP client as initialized and attached.
    fn mark_client_attached(&mut self) {
        self.mcp_client_attached = true;
        self.mark_activity();
    }

    /// Stop the managed app process and reset launcher state without unregistering the launcher.
    async fn stop_app(&mut self) -> Result<StopStatus, EdevError> {
        if let Some(app) = self.app.take() {
            app.shutdown().await;
            self.instance_registry.clear_app()?;
            self.status = AppStatus::NotRunning;
            return Ok(StopStatus::Stopped);
        }
        if matches!(self.status, AppStatus::StartupFailed { .. }) {
            self.status = AppStatus::NotRunning;
            self.instance_registry.clear_app()?;
            return Ok(StopStatus::Stopped);
        }
        self.status = AppStatus::NotRunning;
        self.instance_registry.clear_app()?;
        Ok(StopStatus::AlreadyStopped)
    }

    /// Stop the app process and unregister the launcher.
    async fn shutdown(&mut self) -> Result<(), EdevError> {
        let _stopped = self.stop_app().await?;
        self.instance_registry.unregister()?;
        Ok(())
    }

    /// Start the app process unless it is already running.
    async fn start(&mut self) -> Result<StartStatus, EdevError> {
        match &self.status {
            AppStatus::Running => return Ok(StartStatus::AlreadyRunning),
            AppStatus::StartupFailed { output } => {
                return Ok(StartStatus::RestartRequired(output.clone()));
            }
            AppStatus::Starting => return Ok(StartStatus::AppStarting),
            AppStatus::NotRunning => {}
        }
        self.start_with(|config, log_state| Box::pin(spawn_app(config, log_state)))
            .await
    }

    /// Restart the app process using the default spawn behavior.
    async fn restart(&mut self) -> Result<LifecycleStartStatus, EdevError> {
        let mut attempt = 1;
        loop {
            let result = self
                .restart_with(|config, log_state| Box::pin(spawn_app(config, log_state)))
                .await;
            if restart_result_is_transport_closed(&result) && attempt < RESTART_MAX_ATTEMPTS {
                self.log_edev(format!(
                    "restart attempt {attempt} failed with closed transport; retrying"
                ));
                attempt += 1;
                continue;
            }
            return result;
        }
    }

    /// Resolve the current proxied app client, or return a lifecycle-specific tool error.
    fn proxy_target(&self) -> Result<Arc<AsyncMutex<tmcp::Client<()>>>, CallToolResult> {
        match &self.status {
            AppStatus::Running => {
                let Some(app) = &self.app else {
                    self.log_edev("proxy call failed: app not running");
                    return Err(tool_error(
                        ErrorKind::AppNotRunning,
                        "App process not running. Call start.",
                    ));
                };
                Ok(Arc::clone(&app.client))
            }
            AppStatus::Starting => {
                self.log_edev("proxy call failed: app starting");
                Err(tool_error(
                    ErrorKind::AppStarting,
                    "App is starting. Try again shortly.",
                ))
            }
            AppStatus::StartupFailed { output } => Err(tool_error_with_data(
                ErrorKind::RestartRequired,
                "App startup failed. Fix the issue and call restart.",
                &serde_json::json!({ "startup_output": output }),
            )),
            AppStatus::NotRunning => {
                self.log_edev("proxy call failed: app not running");
                Err(tool_error(
                    ErrorKind::AppNotRunning,
                    "App is not running. Call start.",
                ))
            }
        }
    }

    /// Proxy a tool call to the app if the app is running.
    #[cfg(test)]
    async fn proxy_call(&self, name: &str, arguments: Option<Arguments>) -> CallToolResult {
        let client = match self.proxy_target() {
            Ok(client) => client,
            Err(error) => return error,
        };
        call_proxy_tool(client, self.log_state.clone(), name.to_string(), arguments).await
    }

    /// Start the app process using a caller-provided spawn routine.
    async fn start_with<F>(&mut self, spawn: F) -> Result<StartStatus, EdevError>
    where
        F: for<'a> FnOnce(&'a LaunchConfig, LogState) -> SpawnFuture<'a>,
    {
        let status = self
            .spawn_with(LifecycleAction::Start, false, spawn)
            .await?;
        Ok(match status {
            LifecycleStartStatus::Running => StartStatus::Started,
            LifecycleStartStatus::StartupFailed(output) => StartStatus::StartupFailed(output),
        })
    }

    /// Restart the app process using a caller-provided spawn routine.
    async fn restart_with<F>(&mut self, spawn: F) -> Result<LifecycleStartStatus, EdevError>
    where
        F: for<'a> FnOnce(&'a LaunchConfig, LogState) -> SpawnFuture<'a>,
    {
        self.spawn_with(LifecycleAction::Restart, true, spawn).await
    }

    /// Spawn and attach an app process for either a start or restart transition.
    async fn spawn_with<F>(
        &mut self,
        action: LifecycleAction,
        replace_existing: bool,
        spawn: F,
    ) -> Result<LifecycleStartStatus, EdevError>
    where
        F: for<'a> FnOnce(&'a LaunchConfig, LogState) -> SpawnFuture<'a>,
    {
        self.status = AppStatus::Starting;
        if replace_existing {
            if let Some(app) = self.app.take() {
                app.shutdown().await;
            }
            self.instance_registry.clear_app()?;
        }
        self.log_edev(format!("{} requested", action.as_str()));
        match spawn(&self.config, self.log_state.clone()).await {
            Ok(app) => {
                if let Err(error) = self
                    .instance_registry
                    .set_app_process_group_id(app.process_group_id)
                {
                    app.shutdown().await;
                    self.status = AppStatus::NotRunning;
                    return Err(error);
                }
                if let Err(output) = probe_script_eval_ready(&app.client).await {
                    app.shutdown().await;
                    self.status = AppStatus::StartupFailed {
                        output: output.clone(),
                    };
                    self.instance_registry.clear_app()?;
                    self.log_edev(format!("{} failed during app startup", action.as_str()));
                    return Ok(LifecycleStartStatus::StartupFailed(output));
                }
                self.status = AppStatus::Running;
                self.app = Some(app);
                self.log_edev(format!("{} completed", action.as_str()));
                Ok(LifecycleStartStatus::Running)
            }
            Err(AppStartError::StartupFailed(output)) => {
                self.status = AppStatus::StartupFailed {
                    output: output.clone(),
                };
                self.log_edev(format!("{} failed during app startup", action.as_str()));
                Ok(LifecycleStartStatus::StartupFailed(output))
            }
            Err(AppStartError::Other(message)) => {
                self.status = AppStatus::NotRunning;
                self.log_edev(format!("{} failed: {message}", action.as_str()));
                Err(EdevError::AppStart(message))
            }
        }
    }

    /// Build the static host-side tool list.
    fn tools_list(&self) -> Vec<Tool> {
        vec![
            start_tool(),
            stop_tool(),
            restart_tool(),
            status_tool(),
            script_eval_tool(),
            script_api_tool(),
        ]
    }

    /// Build a structured status snapshot of the managed app lifecycle.
    fn status_report(&self) -> StatusReport {
        let (app_present, process_group_id) = self
            .app
            .as_ref()
            .map(|app| (true, app.process_group_id))
            .unwrap_or((false, None));
        let startup_output = match &self.status {
            AppStatus::StartupFailed { output } => Some(output.clone()),
            _ => None,
        };
        StatusReport {
            state: self.status.as_str(),
            app_present,
            process_group_id,
            startup_output,
            mcp_client_attached: self.mcp_client_attached,
            idle_shutdown: self.idle_shutdown_report(),
            app_health: None,
            app_health_error: None,
        }
    }

    /// Build the user-facing MCP idle guard state.
    fn idle_shutdown_report(&self) -> IdleShutdownReport {
        let Some(idle_after) = self.idle_shutdown_after else {
            return IdleShutdownReport {
                state: "disabled",
                configured_secs: None,
                remaining_secs: None,
            };
        };
        if self.mcp_client_attached {
            return IdleShutdownReport {
                state: "suspended_while_client_attached",
                configured_secs: Some(idle_after.as_secs()),
                remaining_secs: None,
            };
        }
        let elapsed = self.last_activity.elapsed();
        IdleShutdownReport {
            state: "waiting_for_initial_client",
            configured_secs: Some(idle_after.as_secs()),
            remaining_secs: Some(idle_after.saturating_sub(elapsed).as_secs()),
        }
    }
}

/// Running app process and its connected MCP client.
struct AppProcess {
    /// Child process handle for `cargo run`.
    child: Option<Child>,
    /// Process group id for the running app process tree.
    process_group_id: Option<i32>,
    /// Connected MCP client speaking to the app over stdio.
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    /// Background task streaming stderr.
    stderr_task: Option<JoinHandle<()>>,
    /// Captured stderr output, primarily for startup errors.
    stderr_buffer: Arc<Mutex<Vec<u8>>>,
    /// Captured stdout output when stdout is not consumed by the MCP transport.
    stdout_buffer: Arc<Mutex<Vec<u8>>>,
    /// Logger for process lifecycle messages.
    log_state: LogState,
}

impl AppProcess {
    /// Trigger immediate app termination without waiting for child process exit.
    fn start_termination(&mut self) {
        terminate_process_group(self.process_group_id.take(), &self.log_state);
        if let Some(child) = self.child.as_mut() {
            let _start_kill_result = child.start_kill();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }

    /// Terminate the app process and tear down stderr streaming.
    async fn shutdown(mut self) {
        self.start_termination();
        if let Some(mut child) = self.child.take() {
            let _wait_result = child.wait().await;
        }
        let _drain_result = drain_stderr(&self.stderr_buffer).await;
    }
}

impl Drop for AppProcess {
    fn drop(&mut self) {
        self.start_termination();
    }
}

#[derive(Debug)]
/// Current lifecycle state for the managed app.
enum AppStatus {
    /// The app is starting and MCP handshake has not completed.
    Starting,
    /// The app is running and MCP is connected.
    Running,
    /// The app is not running.
    NotRunning,
    /// The last startup attempt failed before the app became ready.
    StartupFailed {
        /// Captured startup output.
        output: String,
    },
}

impl AppStatus {
    /// Stable string identifier for lifecycle serialization.
    fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::NotRunning => "not_running",
            Self::StartupFailed { .. } => "startup_failed",
        }
    }
}

#[derive(Debug)]
/// Errors emitted while starting the app process.
enum AppStartError {
    /// App startup failed before the MCP handshake completed.
    StartupFailed(String),
    /// Other startup failure.
    Other(String),
}

#[derive(Debug)]
/// Outcome of a completed start or restart path.
enum LifecycleStartStatus {
    /// Startup completed successfully.
    Running,
    /// Startup failed before the app became ready.
    StartupFailed(String),
}

#[derive(Debug)]
/// Outcome of a start attempt.
enum StartStatus {
    /// Start completed successfully.
    Started,
    /// The app was already running.
    AlreadyRunning,
    /// Another lifecycle action is currently starting the app.
    AppStarting,
    /// The previous startup failed and the caller must use restart.
    RestartRequired(String),
    /// Startup failed before the app became ready.
    StartupFailed(String),
}

#[derive(Debug)]
/// Outcome of a stop attempt.
enum StopStatus {
    /// A running app was stopped or a failed startup state was cleared.
    Stopped,
    /// No app was running.
    AlreadyStopped,
}

#[derive(Debug, Clone, Copy)]
/// Host-side lifecycle operation.
enum LifecycleAction {
    /// Start the app without replacing a running process.
    Start,
    /// Replace any existing app process with a fresh one.
    Restart,
}

impl LifecycleAction {
    /// Lowercase label for logging.
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Restart => "restart",
        }
    }
}

/// Spawn the app via `cargo run` and connect an MCP client over stdio.
async fn spawn_app(
    config: &LaunchConfig,
    log_state: LogState,
) -> Result<AppProcess, AppStartError> {
    log_state.record_line("edev: spawning app");
    let mut command = config.app_command();
    command.kill_on_drop(true);
    configure_child_process(&mut command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        log_state.record_line(&format!("edev: spawn failed: {error}"));
        AppStartError::Other(format!("Failed to spawn cargo run: {error}"))
    })?;
    log_state.record_line("edev: app process spawned");
    let process_group_id = process_group_id(&child);
    let stdout = child.stdout.take().ok_or_else(|| {
        terminate_process_group(process_group_id, &log_state);
        AppStartError::Other("Failed to capture app stdout".to_string())
    })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        terminate_process_group(process_group_id, &log_state);
        AppStartError::Other("Failed to capture app stdin".to_string())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        terminate_process_group(process_group_id, &log_state);
        AppStartError::Other("Failed to capture app stderr".to_string())
    })?;

    let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
    let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
    let stderr_buffer_clone = Arc::clone(&stderr_buffer);
    let stderr_log_state = log_state.clone();
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = match reader.read_line(&mut line).await {
                Ok(bytes) => bytes,
                Err(_) => break,
            };
            if bytes == 0 {
                break;
            }
            append_tail_capped(&stderr_buffer_clone, line.as_bytes());
            stderr_log_state.record_line(&line);
            let _write_result = tokio_io::stderr().write_all(line.as_bytes()).await;
        }
    });

    let mut client = tmcp::Client::new("edev", env!("CARGO_PKG_VERSION"))
        .with_request_timeout(APP_REQUEST_TIMEOUT);
    if let Err(error) = client.connect_stream_raw(stdout, stdin).await {
        return Err(fail_startup_handshake(
            &mut child,
            process_group_id,
            &stderr_buffer,
            &log_state,
            "connect",
            &error,
        )
        .await);
    }
    if let Err(error) = client.init().await {
        return Err(fail_startup_handshake(
            &mut child,
            process_group_id,
            &stderr_buffer,
            &log_state,
            "init",
            &error,
        )
        .await);
    }
    log_state.record_line("edev: app MCP connected");

    Ok(AppProcess {
        child: Some(child),
        process_group_id,
        client: Arc::new(AsyncMutex::new(client)),
        stderr_task: Some(stderr_task),
        stderr_buffer,
        stdout_buffer,
        log_state,
    })
}

/// Finalize app startup after an MCP handshake failure.
async fn fail_startup_handshake(
    child: &mut Child,
    process_group_id: Option<i32>,
    stderr_buffer: &Arc<Mutex<Vec<u8>>>,
    log_state: &LogState,
    stage: &str,
    error: &McpError,
) -> AppStartError {
    log_state.record_line(&format!("edev: app {stage} failed: {error}"));
    terminate_process_group(process_group_id, log_state);
    let output = drain_stderr(stderr_buffer).await;
    let _wait_result = child.wait().await;
    AppStartError::StartupFailed(format_startup_output(error, &output))
}

/// Combine the handshake error with any captured stderr for diagnostics.
fn format_startup_output(error: impl Display, output: &str) -> String {
    let output = output.trim_end();
    if output.is_empty() {
        error.to_string()
    } else {
        format!("{error}\n{output}")
    }
}

#[cfg(unix)]
/// Wait for shutdown signals to terminate the app process cleanly.
async fn shutdown_signal() {
    use tokio::{
        signal,
        signal::unix::{SignalKind, signal as unix_signal},
    };

    let mut term = unix_signal(SignalKind::terminate()).ok();
    if let Some(term) = term.as_mut() {
        tokio::select! {
            _ = signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    } else {
        let _ctrl_c = signal::ctrl_c().await;
    }
}

#[cfg(not(unix))]
/// Wait for shutdown signals to terminate the app process cleanly.
async fn shutdown_signal() {
    let _ctrl_c = tokio::signal::ctrl_c().await;
}

/// Wait until pre-client launcher inactivity exceeds the configured timeout.
async fn wait_for_idle_shutdown(state: Arc<AsyncMutex<State>>, idle_after: Duration) {
    loop {
        let action = {
            let state = state.lock().await;
            if state.mcp_client_attached {
                state.log_edev("MCP client attached; idle shutdown suspended");
                IdleShutdownAction::Suspend
            } else {
                let elapsed = state.last_activity.elapsed();
                if elapsed >= idle_after {
                    state.log_edev(format!("idle for {}s; shutting down", idle_after.as_secs()));
                    IdleShutdownAction::Shutdown
                } else {
                    IdleShutdownAction::Sleep(idle_after - elapsed)
                }
            }
        };
        match action {
            IdleShutdownAction::Sleep(sleep_for) => sleep(sleep_for).await,
            IdleShutdownAction::Suspend => pending::<()>().await,
            IdleShutdownAction::Shutdown => return,
        }
    }
}

/// Next step for the MCP idle shutdown guard.
enum IdleShutdownAction {
    /// Re-check idle state after this duration.
    Sleep(Duration),
    /// A client is attached, so the guard should stay pending until stdio exits.
    Suspend,
    /// No client attached before the idle budget elapsed.
    Shutdown,
}

/// Drain buffered stderr output into a string.
async fn drain_stderr(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    let mut output = String::new();
    if let Ok(mut data) = buffer.lock() {
        output = String::from_utf8_lossy(&data).to_string();
        data.clear();
    }
    output
}

/// Return buffered process output without clearing it.
fn snapshot_output(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    buffer.lock().map_or_else(
        |_| String::new(),
        |data| String::from_utf8_lossy(&data).to_string(),
    )
}

/// Return bundle stdout text, explaining the normal stdio-MCP empty case.
fn stdout_bundle_text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    let output = snapshot_output(buffer);
    if output.is_empty() {
        STDOUT_TRANSPORT_NOTE.to_string()
    } else {
        output
    }
}

/// Append bytes to a tail-capped process-output buffer.
fn append_tail_capped(buffer: &Arc<Mutex<Vec<u8>>>, bytes: &[u8]) {
    let Ok(mut data) = buffer.lock() else {
        return;
    };
    data.extend_from_slice(bytes);
    if data.len() > APP_LOG_TAIL_LIMIT + APP_LOG_TAIL_TRIM_SLACK {
        let drop_len = data.len() - APP_LOG_TAIL_LIMIT;
        data.drain(..drop_len);
    }
}

/// Fetch the process group id for a spawned child process.
fn process_group_id(child: &Child) -> Option<i32> {
    child.id().and_then(|id| i32::try_from(id).ok())
}

/// Configure child process behavior for cleanup and termination.
fn configure_child_process(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(all(unix, target_os = "linux"))]
    {
        let parent_pid = libc::pid_t::try_from(std::process::id()).ok();
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std_io::Error::last_os_error());
                }
                if let Some(parent_pid) = parent_pid
                    && libc::getppid() != parent_pid
                {
                    return Err(std_io::Error::other("parent process changed before exec"));
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

/// Terminate the process group for a running app process tree.
fn terminate_process_group(process_group_id: Option<i32>, log_state: &LogState) {
    #[cfg(unix)]
    {
        if let Some(pgid) = process_group_id {
            let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
            if result != 0 {
                let error = std_io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    log_state.record_line(&format!(
                        "edev: failed to kill process group {pgid}: {error}"
                    ));
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (process_group_id, log_state);
    }
}

/// MCP server implementation that proxies tool calls to the app.
struct EdevServer {
    /// Shared runtime state for proxying and host-side lifecycle control.
    state: Arc<AsyncMutex<State>>,
}

/// Check whether a tool name should be proxied to the app.
fn is_proxied_tool(name: &str) -> bool {
    PROXIED_TOOL_NAMES.iter().copied().any(|tool| tool == name)
}

#[async_trait]
impl ServerHandler for EdevServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: String,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        {
            let mut state = self.state.lock().await;
            state.mark_client_attached();
        }
        let version = env!("CARGO_PKG_VERSION").to_string();
        Ok(InitializeResult::new("edev")
            .with_version(version)
            .with_tools(Some(false)))
    }

    async fn list_tools(
        &self,
        _context: &ServerCtx,
        _cursor: Option<Cursor>,
    ) -> tmcp::Result<ListToolsResult> {
        let state = Arc::clone(&self.state);
        let state = state.lock().await;
        Ok(ListToolsResult::new().with_tools(state.tools_list()))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        let state = Arc::clone(&self.state);
        {
            let mut state_guard = state.lock().await;
            state_guard.mark_activity();
        }
        if !is_host_tool(&name) && !is_proxied_tool(&name) {
            return Err(McpError::ToolNotFound(name));
        }
        let result = match name.as_str() {
            "start" => {
                let mut state = state.lock().await;
                let start = Instant::now();
                match state.start().await {
                    Ok(StartStatus::Started) => lifecycle_success("started", start.elapsed()),
                    Ok(StartStatus::AlreadyRunning) => {
                        lifecycle_success("already_running", start.elapsed())
                    }
                    Ok(StartStatus::AppStarting) => tool_error(
                        ErrorKind::AppStarting,
                        "App is starting. Try again shortly.",
                    ),
                    Ok(StartStatus::RestartRequired(output)) => tool_error_with_data(
                        ErrorKind::RestartRequired,
                        "App startup previously failed. Fix the issue and call restart.",
                        &serde_json::json!({ "startup_output": output }),
                    ),
                    Ok(StartStatus::StartupFailed(output)) => lifecycle_startup_failed(
                        "App startup failed. Fix the issue and call restart again.",
                        &output,
                        start.elapsed(),
                    ),
                    Err(error) => lifecycle_failed(
                        ErrorKind::StartFailed,
                        format!("Start failed: {error}"),
                        start.elapsed(),
                    ),
                }
            }
            "stop" => {
                let mut state = state.lock().await;
                let start = Instant::now();
                match state.stop_app().await {
                    Ok(StopStatus::Stopped) => lifecycle_success("stopped", start.elapsed()),
                    Ok(StopStatus::AlreadyStopped) => {
                        lifecycle_success("already_stopped", start.elapsed())
                    }
                    Err(error) => lifecycle_failed(
                        ErrorKind::StopFailed,
                        format!("Stop failed: {error}"),
                        start.elapsed(),
                    ),
                }
            }
            "restart" => {
                let mut state = state.lock().await;
                let start = Instant::now();
                match state.restart().await {
                    Ok(LifecycleStartStatus::Running) => {
                        lifecycle_success("completed", start.elapsed())
                    }
                    Ok(LifecycleStartStatus::StartupFailed(output)) => lifecycle_startup_failed(
                        "App startup failed. Fix the issue and call restart again.",
                        &output,
                        start.elapsed(),
                    ),
                    Err(error) => lifecycle_failed(
                        ErrorKind::RestartFailed,
                        format!("Restart failed: {error}"),
                        start.elapsed(),
                    ),
                }
            }
            "status" => {
                let (report, app_client, log_state) = {
                    let state = state.lock().await;
                    let app_client = if matches!(state.status, AppStatus::Running) {
                        state.app.as_ref().map(|app| Arc::clone(&app.client))
                    } else {
                        None
                    };
                    (state.status_report(), app_client, state.log_state.clone())
                };
                let report = if let Some(client) = app_client {
                    report.with_app_health(call_app_health(client, log_state).await)
                } else {
                    report
                };
                CallToolResult::new()
                    .with_structured_content(serde_json::to_value(report).expect("status report"))
            }
            "script_api" => {
                let (app_client, log_state) = {
                    let state = state.lock().await;
                    let app_client = if matches!(state.status, AppStatus::Running) {
                        state.app.as_ref().map(|app| Arc::clone(&app.client))
                    } else {
                        None
                    };
                    (app_client, state.log_state.clone())
                };
                if let Some(client) = app_client {
                    call_proxy_tool(client, log_state, "script_api".to_string(), arguments).await
                } else {
                    CallToolResult::new().with_text_content(script_definitions())
                }
            }
            _ => {
                let (client, log_state) = {
                    let state = state.lock().await;
                    match state.proxy_target() {
                        Ok(client) => (client, state.log_state.clone()),
                        Err(error) => return Ok(error.into()),
                    }
                };
                call_proxy_tool(client, log_state, name, arguments).await
            }
        };
        Ok(result.into())
    }
}

/// Proxy a tool call through the connected app client.
async fn call_proxy_tool(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    log_state: LogState,
    name: String,
    arguments: Option<Arguments>,
) -> CallToolResult {
    let client = client.lock().await;
    match client.call_tool(name, arguments.unwrap_or_default()).await {
        Ok(result) => normalize_tool_result(result),
        Err(error) => {
            log_state.record_line(&format!("edev: proxy call failed: {error}"));
            tool_error(ErrorKind::ProxyFailed, error.to_string())
        }
    }
}

/// Query the running app's health tool and normalize the response payload.
async fn call_app_health(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    log_state: LogState,
) -> Result<serde_json::Value, String> {
    let result = {
        let client = client.lock().await;
        client
            .call_tool("health".to_string(), Arguments::default())
            .await
            .map_err(|error| {
                log_state.record_line(&format!("edev: app health proxy failed: {error}"));
                error.to_string()
            })?
    };
    let result = normalize_tool_result(result);
    if result.is_error() {
        let message = result
            .text()
            .map(str::to_string)
            .or_else(|| result.structured_content.as_ref().map(ToString::to_string))
            .unwrap_or_else(|| "app health proxy failed".to_string());
        log_state.record_line(&format!("edev: app health proxy failed: {message}"));
        return Err(message);
    }
    if let Some(content) = result.structured_content {
        return Ok(content);
    }
    let Some(text) = result.text() else {
        return Err("app health response was empty".to_string());
    };
    serde_json::from_str(text).map_err(|error| format!("failed to parse app health: {error}"))
}

/// Execute the resolved smoke suite by calling `script_eval` for each discovered script.
async fn run_smoke_suite(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    config: &SmokeConfig,
    bundle_context: Option<BundleContext>,
) -> Result<SuiteResult, EdevError> {
    Ok(run_suite_with(
        &config.suite,
        |request: ScriptRunRequest| {
            let script_path = request.path.clone();
            let script_args = request.args.clone();
            let payload = script_eval_request_value(ScriptEvalRequest {
                script: request.source,
                timeout_ms: request.timeout_ms,
                options: Some(ScriptEvalOptions {
                    source_name: Some(script_path.clone()),
                    args: request.args,
                }),
            });
            let result = block_in_place(|| {
                Handle::current().block_on(async {
                    let client = client.lock().await;
                    client
                        .call_tool("script_eval".to_string(), payload)
                        .await
                        .map_err(|error| error.to_string())
                })
            })?;
            let outcome = parse_script_eval_outcome(&result)?;
            if !outcome.success
                && let Some(context) = bundle_context.as_ref()
            {
                let bundle_round = if config.suite.round_limit() > 1 {
                    Some(request.round)
                } else {
                    None
                };
                let bundle_result = block_in_place(|| {
                    Handle::current().block_on(write_failure_bundle(
                        &client,
                        context,
                        &script_path,
                        bundle_round,
                        &script_args,
                        &outcome,
                    ))
                });
                if let Err(error) = bundle_result {
                    eprintln!("edev: failed to write failure bundle for {script_path}: {error}");
                }
            }
            Ok(outcome)
        },
    ))
}

/// Decode a proxied `script_eval` tool result back into the checked-in outcome shape.
fn parse_script_eval_outcome(result: &CallToolResult) -> Result<ScriptEvalOutcome, String> {
    let payload = if let Some(structured) = &result.structured_content {
        structured.clone()
    } else {
        let Some(text) = result.text() else {
            return Err("script_eval response was missing JSON content".to_string());
        };
        serde_json::from_str(text)
            .map_err(|error| format!("failed to parse script_eval response: {error}"))?
    };
    serde_json::from_value(payload)
        .map_err(|error| format!("failed to decode script_eval outcome: {error}"))
}

/// Serialize a `script_eval` request into an MCP arguments object.
fn script_eval_request_value(request: ScriptEvalRequest) -> Arguments {
    Arguments::from_struct(request).expect("script eval request should serialize")
}

/// Probe the app's `script_eval` tool to confirm the script runtime is ready.
async fn probe_script_eval_ready(client: &Arc<AsyncMutex<tmcp::Client<()>>>) -> Result<(), String> {
    let request = script_eval_request_value(ScriptEvalRequest {
        script: "return true".to_string(),
        timeout_ms: Some(1_000),
        options: None,
    });
    let result = {
        let client = client.lock().await;
        client
            .call_tool("script_eval".to_string(), request)
            .await
            .map_err(|error| error.to_string())?
    };
    let outcome = parse_script_eval_outcome(&result)?;
    if outcome.success {
        Ok(())
    } else {
        let message = outcome
            .error
            .as_ref()
            .map(|error| error.message.as_str())
            .unwrap_or("script_eval readiness probe failed");
        Err(message.to_string())
    }
}

/// Return true when a tool is handled directly by the launcher.
fn is_host_tool(name: &str) -> bool {
    matches!(name, "start" | "stop" | "restart" | "status" | "script_api")
}

/// Tool definition for starting the app process.
fn start_tool() -> Tool {
    Tool::new("start", ToolSchema::default())
        .with_description("Start the underlying app process if it is not already running.")
}

/// Tool definition for stopping the app process.
fn stop_tool() -> Tool {
    Tool::new("stop", ToolSchema::default())
        .with_description("Stop the underlying app process if it is running.")
}

/// Tool definition for restarting the app process.
fn restart_tool() -> Tool {
    Tool::new("restart", ToolSchema::default())
        .with_description("Restart the underlying app process.")
}

/// Tool definition for reporting launcher lifecycle state.
fn status_tool() -> Tool {
    Tool::new("status", ToolSchema::default())
        .with_description("Report the current launcher and app lifecycle state.")
}

/// Tool definition for proxying `script_eval` through the launcher.
fn script_eval_tool() -> Tool {
    Tool::new(
        "script_eval",
        ToolSchema::from_json_schema::<ScriptEvalRequest>(),
    )
    .with_description(
        "Evaluate a Luau script with DevMCP helpers. Scripts are assumed to be strict.",
    )
}

/// Tool definition for exposing the checked-in Luau API definitions.
fn script_api_tool() -> Tool {
    Tool::new("script_api", ToolSchema::default())
        .with_description("Return the checked-in Luau definitions for the full scripting API.")
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
struct LifecycleReport {
    status: &'static str,
    elapsed_ms: u64,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
struct StatusReport {
    state: &'static str,
    app_present: bool,
    process_group_id: Option<i32>,
    startup_output: Option<String>,
    mcp_client_attached: bool,
    idle_shutdown: IdleShutdownReport,
    app_health: Option<serde_json::Value>,
    app_health_error: Option<String>,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
struct IdleShutdownReport {
    state: &'static str,
    configured_secs: Option<u64>,
    remaining_secs: Option<u64>,
}

impl StatusReport {
    /// Attach app health data or a health proxy error to the status report.
    fn with_app_health(mut self, health: Result<serde_json::Value, String>) -> Self {
        match health {
            Ok(health) => self.app_health = Some(health),
            Err(error) => self.app_health_error = Some(error),
        }
        self
    }
}

/// Build a successful lifecycle tool result.
fn lifecycle_success(status: &'static str, elapsed: Duration) -> CallToolResult {
    CallToolResult::new().with_structured_content(serde_json::json!({
        "ok": true,
        "report": LifecycleReport {
            status,
            elapsed_ms: elapsed.as_millis() as u64,
        },
    }))
}

/// Build a lifecycle tool result for startup failure.
fn lifecycle_startup_failed(
    message: &'static str,
    output: &str,
    elapsed: Duration,
) -> CallToolResult {
    tool_error_with_data(
        ErrorKind::StartupFailed,
        message,
        &serde_json::json!({
            "startup_output": output,
            "report": LifecycleReport {
                status: "startup_failed",
                elapsed_ms: elapsed.as_millis() as u64,
            },
        }),
    )
}

/// Build a lifecycle tool result for non-startup failures.
fn lifecycle_failed(kind: ErrorKind, message: String, elapsed: Duration) -> CallToolResult {
    tool_error_with_data(
        kind,
        message,
        &serde_json::json!({
            "report": LifecycleReport {
                status: "failed",
                elapsed_ms: elapsed.as_millis() as u64,
            },
        }),
    )
}

#[derive(Debug, Clone, Copy)]
/// Error kinds returned in structured tool failures.
enum ErrorKind {
    /// App is starting and cannot accept tool calls.
    AppStarting,
    /// App process is not running.
    AppNotRunning,
    /// Restart is required to recover.
    RestartRequired,
    /// Start failed for a non-startup reason.
    StartFailed,
    /// Stop failed.
    StopFailed,
    /// Restart failed for a non-startup reason.
    RestartFailed,
    /// Startup failed before the app became ready.
    StartupFailed,
    /// Proxy call to the app failed.
    ProxyFailed,
}

/// Build a structured tool error result.
fn tool_error(kind: ErrorKind, message: impl Into<String>) -> CallToolResult {
    build_tool_error(kind, message.into(), None)
}

/// Build a structured tool error result with extra data.
fn tool_error_with_data(
    kind: ErrorKind,
    message: impl Into<String>,
    data: &serde_json::Value,
) -> CallToolResult {
    build_tool_error(kind, message.into(), Some(data))
}

/// Build a structured tool error result with optional data payload.
fn build_tool_error(
    kind: ErrorKind,
    message: String,
    data: Option<&serde_json::Value>,
) -> CallToolResult {
    let mut error = serde_json::json!({
        "kind": kind.as_str(),
        "message": &message,
    });
    if let Some(data) = data {
        error
            .as_object_mut()
            .expect("tool error payload should be an object")
            .insert("data".to_string(), data.clone());
    }
    CallToolResult::new()
        .with_is_error(true)
        .with_text_content(message)
        .with_structured_content(serde_json::json!({ "error": error }))
}

/// Ensure tool error results include readable text content.
fn normalize_tool_result(result: CallToolResult) -> CallToolResult {
    if !matches!(result.is_error, Some(true)) || !result.content.is_empty() {
        return result;
    }
    let message = result
        .structured_content
        .as_ref()
        .and_then(extract_error_message)
        .map(str::to_string);
    match message {
        Some(message) => result.with_text_content(message),
        None => result,
    }
}

/// Extract an error message from a structured tool error payload.
fn extract_error_message(structured: &serde_json::Value) -> Option<&str> {
    structured
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .or_else(|| {
            structured
                .get("message")
                .and_then(|message| message.as_str())
        })
}

impl ErrorKind {
    /// Stable string identifier for error serialization.
    fn as_str(self) -> &'static str {
        match self {
            Self::AppStarting => "app_starting",
            Self::AppNotRunning => "app_not_running",
            Self::RestartRequired => "restart_required",
            Self::StartFailed => "start_failed",
            Self::StopFailed => "stop_failed",
            Self::RestartFailed => "restart_failed",
            Self::StartupFailed => "startup_failed",
            Self::ProxyFailed => "proxy_failed",
        }
    }
}

/// Return true when a restart result indicates transient transport closure.
fn restart_result_is_transport_closed(result: &Result<LifecycleStartStatus, EdevError>) -> bool {
    matches!(
        result,
        Err(EdevError::Mcp(
            McpError::TransportDisconnected | McpError::ConnectionClosed
        ))
    ) || matches!(
        result,
        Err(EdevError::Mcp(McpError::Transport(message)))
            if transport_message_is_closed(message)
    ) || matches!(
        result,
        Ok(LifecycleStartStatus::StartupFailed(output)) if transport_message_is_closed(output)
    )
}

/// Return true when an MCP transport error string indicates closed transport.
fn transport_message_is_closed(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["closed", "disconnect", "broken pipe", "eof"]
        .iter()
        .any(|fragment| message.contains(fragment))
}

#[cfg(test)]
fn test_tempdir() -> tempfile::TempDir {
    use std::fs;

    fs::create_dir_all("tmp").expect("create tmp");
    tempfile::Builder::new()
        .prefix("edev-test-")
        .tempdir_in("tmp")
        .expect("tempdir")
}

#[cfg(test)]
fn test_config(cwd: PathBuf) -> LaunchConfig {
    LaunchConfig {
        cwd,
        command: vec![
            "cargo".to_string(),
            "run".to_string(),
            "--dev-mcp".to_string(),
        ],
        env: Default::default(),
        verbose: false,
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use eguidev_runtime::{
        ScriptArgValue, ScriptArgs, ScriptErrorInfo, ScriptImageInfo,
        smoke::{SuiteConfig, SuiteRunMode},
    };
    use tempfile::TempDir;
    use tmcp::{
        Client, Server, ServerCtx, ServerHandle, ServerHandler,
        schema::{
            CallToolResponse, CallToolResult, ContentBlock, Cursor, ImageContent, InitializeResult,
            ListToolsResult, TaskMetadata,
        },
        testutils::{TestServerContext, make_duplex_pair},
    };
    use tokio::time::timeout;

    use super::*;

    fn make_state(tempdir: &TempDir) -> State {
        let config = test_config(tempdir.path().to_path_buf());
        let registry = InstanceRegistry::register(&config).expect("instance registry");
        State::new(config, registry)
    }

    fn successful_script_eval_result() -> CallToolResult {
        CallToolResult::new()
            .with_json_text(serde_json::json!({
                "success": true,
                "value": true,
                "logs": [],
                "assertions": [],
                "timing": {
                    "compile_ms": 0,
                    "exec_ms": 0,
                    "total_ms": 0
                }
            }))
            .expect("script eval json")
    }

    fn successful_outcome(value: &serde_json::Value) -> ScriptEvalOutcome {
        parse_script_eval_outcome(
            &CallToolResult::new()
                .with_json_text(serde_json::json!({
                    "success": true,
                    "value": value,
                    "logs": [],
                    "assertions": [],
                    "timing": {
                        "compile_ms": 0,
                        "exec_ms": 0,
                        "total_ms": 0
                    }
                }))
                .expect("script eval json"),
        )
        .expect("script eval outcome")
    }

    fn dump_config(tempdir: &TempDir) -> DumpConfig {
        DumpConfig {
            launch: test_config(tempdir.path().to_path_buf()),
            fixture: None,
            params: BTreeMap::new(),
            viewport: None,
            wait_for_capture: true,
            json: false,
            out: None,
            timeout: None,
        }
    }

    #[test]
    fn dump_script_waits_for_capture_without_fixture() {
        let tempdir = test_tempdir();
        let mut config = dump_config(&tempdir);
        config.viewport = Some("secondary".to_string());

        assert_eq!(
            dump_script(&config),
            "wait_for_capture()\nreturn dump_text({ viewport = \"secondary\" })"
        );
    }

    #[test]
    fn dump_script_uses_fixture_without_extra_capture_wait() {
        let tempdir = test_tempdir();
        let mut config = dump_config(&tempdir);
        config.fixture = Some("basic.default".to_string());
        config.wait_for_capture = false;
        config.json = true;

        assert_eq!(
            dump_script(&config),
            "fixture(\"basic.default\")\nreturn dump()"
        );
    }

    #[test]
    fn dump_script_passes_fixture_params() {
        let tempdir = test_tempdir();
        let mut config = dump_config(&tempdir);
        config.fixture = Some("basic.scrolled".to_string());
        config
            .params
            .insert("enabled".to_string(), ScriptArgValue::Bool(true));
        config.params.insert(
            "label".to_string(),
            ScriptArgValue::String("A|B".to_string()),
        );
        config
            .params
            .insert("offset".to_string(), ScriptArgValue::Int(180));
        config.wait_for_capture = false;

        assert_eq!(
            dump_script(&config),
            "fixture(\"basic.scrolled\", { [\"enabled\"] = true, [\"label\"] = \"A|B\", [\"offset\"] = 180 })\nreturn dump_text()"
        );
    }

    #[test]
    fn eval_output_value_adds_image_files() {
        let mut outcome = successful_outcome(&serde_json::json!({
            "capture": {
                "type": "image_ref",
                "id": "image-0"
            }
        }));
        outcome.images = Some(vec![ScriptImageInfo {
            id: "image-0".to_string(),
            content_index: 1,
            kind: "viewport".to_string(),
            viewport_id: Some("root".to_string()),
            target: None,
            rect: None,
            metadata: None,
        }]);
        let files = BTreeMap::from([(
            "image-0".to_string(),
            PathBuf::from("/tmp/eval/capture-image-0.jpg"),
        )]);

        let output = eval_output_value(&outcome, &files).expect("eval output");

        assert_eq!(
            output["images"][0]["file"],
            serde_json::json!("/tmp/eval/capture-image-0.jpg")
        );
    }

    #[test]
    fn stable_hash8_is_deterministic_and_path_sensitive() {
        assert_eq!(
            stable_hash8("nested/fail.luau"),
            stable_hash8("nested/fail.luau")
        );
        assert_ne!(
            stable_hash8("nested/fail.luau"),
            stable_hash8("other/fail.luau")
        );
        assert_eq!(stable_hash8("nested/fail.luau").len(), 8);
    }

    #[test]
    fn replace_dir_overwrites_existing_bundle_directory() {
        let tempdir = test_tempdir();
        let bundle_dir = tempdir.path().join("bundle");
        fs::create_dir_all(&bundle_dir).expect("create bundle");
        fs::write(bundle_dir.join("old.txt"), "old").expect("write old");

        replace_dir(&bundle_dir).expect("replace dir");

        assert!(bundle_dir.is_dir());
        assert!(!bundle_dir.join("old.txt").exists());
    }

    #[test]
    fn bundle_meta_and_failure_text_include_script_context() {
        let tempdir = test_tempdir();
        let outcome = parse_script_eval_outcome(
            &CallToolResult::new()
                .with_json_text(serde_json::json!({
                    "success": false,
                    "logs": ["before failure"],
                    "assertions": [{
                        "passed": false,
                        "message": "expected ready",
                        "location": "fail.luau:3"
                    }],
                    "fixtures": [{
                        "name": "basic.default",
                        "params": {
                            "offset": 180
                        }
                    }],
                    "timing": {
                        "compile_ms": 0,
                        "exec_ms": 1,
                        "total_ms": 1
                    },
                    "error": {
                        "type": "assertion",
                        "message": "expected ready",
                        "code": "assertion_failed",
                        "details": {
                            "widget": "basic.status"
                        }
                    }
                }))
                .expect("script eval json"),
        )
        .expect("outcome");
        let context = BundleContext {
            dir: tempdir.path().join("bundles"),
            launch: test_config(tempdir.path().to_path_buf()),
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            collection_timeout_ms: 10_000,
        };
        let args = ScriptArgs::from([(
            "name".to_string(),
            ScriptArgValue::String("Sky".to_string()),
        )]);

        let meta = bundle_meta(&context, "nested/fail.luau", Some(2), &args, &outcome)
            .expect("bundle meta");
        let meta: serde_json::Value = serde_json::from_str(&meta).expect("meta json");
        assert_eq!(meta["script"]["path"], "nested/fail.luau");
        assert_eq!(meta["script"]["round"], 2);
        assert_eq!(meta["script"]["args"]["name"], "Sky");
        assert_eq!(meta["fixtures"][0]["name"], "basic.default");
        assert_eq!(meta["fixtures"][0]["params"]["offset"], 180);
        assert_eq!(meta["failure"]["details"]["widget"], "basic.status");

        let text = failure_text(&outcome).expect("failure text");
        assert!(text.contains("failure: expected ready"));
        assert!(text.contains("before failure"));
        assert!(text.contains("basic.default"));
    }

    #[test]
    fn stdout_bundle_text_explains_stdio_transport_when_empty() {
        let empty = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(stdout_bundle_text(&empty), STDOUT_TRANSPORT_NOTE);

        let captured = Arc::new(Mutex::new(b"captured stdout\n".to_vec()));
        assert_eq!(stdout_bundle_text(&captured), "captured stdout\n");
    }

    #[tokio::test]
    async fn eval_script_calls_script_eval_with_timeout_and_args() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (app, _handle) = make_recording_eval_app(Arc::clone(&requests)).await;
        let tempdir = test_tempdir();
        let config = EvalConfig {
            launch: test_config(tempdir.path().to_path_buf()),
            script: tempdir.path().join("probe.luau"),
            out_dir: tempdir.path().join("eval-out"),
            timeout: Some(Duration::from_millis(1_234)),
            args: ScriptArgs::from([(
                "name".to_string(),
                ScriptArgValue::String("Sky".to_string()),
            )]),
        };

        run_eval_script(
            Arc::clone(&app.client),
            &config,
            "return args.name".to_string(),
        )
        .await
        .expect("eval script");

        let requests = requests.lock().expect("requests lock poisoned");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.script, "return args.name");
        assert_eq!(request.timeout_ms, Some(1_234));
        assert_eq!(
            request.options.as_ref().expect("options").args.get("name"),
            Some(&ScriptArgValue::String("Sky".to_string()))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_smoke_suite_writes_deterministic_failure_bundle() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (app, _handle) = make_bundle_smoke_app(Arc::clone(&requests)).await;
        let tempdir = test_tempdir();
        let suite_dir = tempdir.path().join("suite");
        fs::create_dir_all(&suite_dir).expect("create suite");
        fs::write(suite_dir.join("10_fail.luau"), "assert(false, \"boom\")").expect("write script");
        let bundle_dir = tempdir.path().join("bundles");
        let stderr_buffer = Arc::new(Mutex::new(b"app stderr\n".to_vec()));
        let stdout_buffer = Arc::new(Mutex::new(b"app stdout\n".to_vec()));
        let context = BundleContext {
            dir: bundle_dir.clone(),
            launch: test_config(tempdir.path().to_path_buf()),
            stderr_buffer,
            stdout_buffer,
            collection_timeout_ms: 1_000,
        };
        let config = SmokeConfig {
            launch: Some(test_config(tempdir.path().to_path_buf())),
            suite: SuiteConfig {
                suite_dir,
                scripts: Vec::new(),
                only: Vec::new(),
                suite_timeout: Duration::from_secs(10),
                script_timeout: Some(Duration::from_secs(1)),
                fail_fast: false,
                run_mode: SuiteRunMode::ONCE,
                args: ScriptArgs::from([(
                    "name".to_string(),
                    ScriptArgValue::String("Sky".to_string()),
                )]),
            },
            verbose_output: false,
            list: false,
            list_json: false,
            bundle_dir: Some(bundle_dir.clone()),
        };

        let result = run_smoke_suite(Arc::clone(&app.client), &config, Some(context.clone()))
            .await
            .expect("smoke suite");

        assert_eq!(result.failed(), 1);
        assert_eq!(result.results[0].message.as_deref(), Some("boom"));
        assert_eq!(result.results[0].path, "10_fail.luau");
        let script_dir = bundle_dir.join(format!(
            "{}-{}",
            safe_file_component(&result.results[0].path),
            stable_hash8(&result.results[0].path)
        ));
        assert!(
            script_dir.join("meta.json").is_file(),
            "bundle entries: {:?}",
            fs::read_dir(&bundle_dir)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.file_name())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        );
        assert!(script_dir.join("failure.txt").is_file());
        assert!(script_dir.join("tree.json").is_file());
        assert!(script_dir.join("tree.txt").is_file());
        assert!(script_dir.join("diagnostics.json").is_file());
        assert_eq!(
            fs::read_to_string(script_dir.join("app.stderr.log")).expect("stderr"),
            "app stderr\n"
        );
        assert_eq!(
            fs::read_to_string(script_dir.join("app.stdout.log")).expect("stdout"),
            "app stdout\n"
        );
        assert_eq!(
            fs::read(script_dir.join("viewport-root.jpg")).expect("viewport image"),
            b"jpeg"
        );
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(script_dir.join("meta.json")).expect("meta"))
                .expect("meta json");
        assert_eq!(meta["script"]["args"]["name"], "Sky");
        assert_eq!(meta["fixtures"][0]["name"], "basic.default");
        assert_eq!(meta["fixtures"][0]["params"]["offset"], 180);

        fs::write(script_dir.join("stale.txt"), "stale").expect("write stale");
        let second = run_smoke_suite(Arc::clone(&app.client), &config, Some(context))
            .await
            .expect("second smoke suite");
        assert_eq!(second.failed(), 1);
        assert!(!script_dir.join("stale.txt").exists());
        assert_eq!(requests.lock().expect("requests").len(), 6);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_smoke_suite_writes_round_suffixed_failure_bundles() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (app, _handle) = make_bundle_smoke_app(Arc::clone(&requests)).await;
        let tempdir = test_tempdir();
        let suite_dir = tempdir.path().join("suite");
        fs::create_dir_all(&suite_dir).expect("create suite");
        fs::write(suite_dir.join("10_fail.luau"), "assert(false, \"boom\")").expect("write script");
        let bundle_dir = tempdir.path().join("bundles");
        let context = BundleContext {
            dir: bundle_dir.clone(),
            launch: test_config(tempdir.path().to_path_buf()),
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            collection_timeout_ms: 1_000,
        };
        let config = SmokeConfig {
            launch: Some(test_config(tempdir.path().to_path_buf())),
            suite: SuiteConfig {
                suite_dir,
                scripts: Vec::new(),
                only: Vec::new(),
                suite_timeout: Duration::from_secs(10),
                script_timeout: Some(Duration::from_secs(1)),
                fail_fast: false,
                run_mode: SuiteRunMode::Repeat(2),
                args: ScriptArgs::default(),
            },
            verbose_output: false,
            list: false,
            list_json: false,
            bundle_dir: Some(bundle_dir.clone()),
        };

        let result = run_smoke_suite(Arc::clone(&app.client), &config, Some(context))
            .await
            .expect("smoke suite");

        assert_eq!(result.failed(), 2);
        for round in 1_u32..=2 {
            let key = format!("10_fail.luau-round-{round}");
            let script_dir = bundle_dir.join(format!(
                "{}-{}",
                safe_file_component(&key),
                stable_hash8(&key)
            ));
            assert!(
                script_dir.join("meta.json").is_file(),
                "bundle entries: {:?}",
                fs::read_dir(&bundle_dir)
                    .map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|entry| entry.file_name())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            );
            let meta: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(script_dir.join("meta.json")).expect("meta"),
            )
            .expect("meta json");
            assert_eq!(meta["script"]["path"], "10_fail.luau");
            assert_eq!(meta["script"]["round"].as_u64(), Some(u64::from(round)));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_smoke_suite_preserves_failure_when_bundle_write_fails() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (app, _handle) = make_bundle_smoke_app(Arc::clone(&requests)).await;
        let tempdir = test_tempdir();
        let suite_dir = tempdir.path().join("suite");
        fs::create_dir_all(&suite_dir).expect("create suite");
        fs::write(suite_dir.join("10_fail.luau"), "assert(false, \"boom\")").expect("write script");
        let bundle_root = tempdir.path().join("bundle-root-file");
        fs::write(&bundle_root, "not a directory").expect("write bundle root file");
        let context = BundleContext {
            dir: bundle_root.clone(),
            launch: test_config(tempdir.path().to_path_buf()),
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            collection_timeout_ms: 1_000,
        };
        let config = SmokeConfig {
            launch: Some(test_config(tempdir.path().to_path_buf())),
            suite: SuiteConfig {
                suite_dir,
                scripts: Vec::new(),
                only: Vec::new(),
                suite_timeout: Duration::from_secs(10),
                script_timeout: Some(Duration::from_secs(1)),
                fail_fast: false,
                run_mode: SuiteRunMode::ONCE,
                args: ScriptArgs::default(),
            },
            verbose_output: false,
            list: false,
            list_json: false,
            bundle_dir: Some(bundle_root),
        };

        let result = run_smoke_suite(Arc::clone(&app.client), &config, Some(context))
            .await
            .expect("smoke suite");

        assert_eq!(result.failed(), 1);
        assert_eq!(result.results[0].message.as_deref(), Some("boom"));
        assert_eq!(result.results[0].logs, vec!["before failure"]);
        assert_eq!(result.results[0].fixtures[0].name, "basic.default");
        assert_eq!(requests.lock().expect("requests").len(), 1);
    }

    #[test]
    fn safe_file_component_keeps_filenames_portable() {
        assert_eq!(safe_file_component("form/result 1"), "form-result-1");
        assert_eq!(safe_file_component("***"), "image");
    }

    struct MockServer;

    #[async_trait]
    impl ServerHandler for MockServer {
        async fn initialize(
            &self,
            _context: &ServerCtx,
            _protocol_version: String,
            _capabilities: ClientCapabilities,
            _client_info: Implementation,
        ) -> tmcp::Result<InitializeResult> {
            Ok(InitializeResult::new("mock"))
        }

        async fn list_tools(
            &self,
            _context: &ServerCtx,
            _cursor: Option<Cursor>,
        ) -> tmcp::Result<ListToolsResult> {
            Ok(ListToolsResult::new())
        }

        async fn call_tool(
            &self,
            _context: &ServerCtx,
            name: String,
            _arguments: Option<Arguments>,
            _task: Option<TaskMetadata>,
        ) -> tmcp::Result<CallToolResponse> {
            if name == "script_eval" {
                Ok(successful_script_eval_result().into())
            } else if name == "script_api" {
                Ok(CallToolResult::new()
                    .with_text_content("live app script api")
                    .into())
            } else if name == "health" {
                Ok(CallToolResult::new()
                    .with_structured_content(serde_json::json!({
                        "frame_count": 4,
                        "fixture_epoch": 2,
                        "known_viewports": ["root"],
                        "stalled": false,
                        "viewports": []
                    }))
                    .into())
            } else {
                Err(McpError::ToolNotFound(name))
            }
        }
    }

    struct HealthFailingServer;

    struct RecordingEvalServer {
        requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
    }

    struct BundleSmokeServer {
        requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
    }

    #[async_trait]
    impl ServerHandler for HealthFailingServer {
        async fn initialize(
            &self,
            _context: &ServerCtx,
            _protocol_version: String,
            _capabilities: ClientCapabilities,
            _client_info: Implementation,
        ) -> tmcp::Result<InitializeResult> {
            Ok(InitializeResult::new("mock"))
        }

        async fn list_tools(
            &self,
            _context: &ServerCtx,
            _cursor: Option<Cursor>,
        ) -> tmcp::Result<ListToolsResult> {
            Ok(ListToolsResult::new())
        }

        async fn call_tool(
            &self,
            _context: &ServerCtx,
            name: String,
            _arguments: Option<Arguments>,
            _task: Option<TaskMetadata>,
        ) -> tmcp::Result<CallToolResponse> {
            if name == "script_eval" {
                Ok(successful_script_eval_result().into())
            } else if name == "health" {
                Err(McpError::InternalError("health boom".to_string()))
            } else {
                Err(McpError::ToolNotFound(name))
            }
        }
    }

    #[async_trait]
    impl ServerHandler for RecordingEvalServer {
        async fn initialize(
            &self,
            _context: &ServerCtx,
            _protocol_version: String,
            _capabilities: ClientCapabilities,
            _client_info: Implementation,
        ) -> tmcp::Result<InitializeResult> {
            Ok(InitializeResult::new("recording"))
        }

        async fn list_tools(
            &self,
            _context: &ServerCtx,
            _cursor: Option<Cursor>,
        ) -> tmcp::Result<ListToolsResult> {
            Ok(ListToolsResult::new())
        }

        async fn call_tool(
            &self,
            _context: &ServerCtx,
            name: String,
            arguments: Option<Arguments>,
            _task: Option<TaskMetadata>,
        ) -> tmcp::Result<CallToolResponse> {
            if name != "script_eval" {
                return Err(McpError::ToolNotFound(name));
            }
            let request = arguments
                .ok_or_else(|| McpError::InternalError("missing arguments".to_string()))?
                .deserialize::<ScriptEvalRequest>()
                .map_err(|error| McpError::InternalError(error.to_string()))?;
            self.requests
                .lock()
                .expect("requests lock poisoned")
                .push(request.clone());
            Ok(CallToolResult::new()
                .with_json_text(serde_json::json!({
                    "success": true,
                    "value": {
                        "script": request.script,
                        "timeout_ms": request.timeout_ms,
                    },
                    "logs": [],
                    "assertions": [],
                    "timing": {
                        "compile_ms": 0,
                        "exec_ms": 0,
                        "total_ms": 0
                    }
                }))
                .expect("script eval json")
                .into())
        }
    }

    #[async_trait]
    impl ServerHandler for BundleSmokeServer {
        async fn initialize(
            &self,
            _context: &ServerCtx,
            _protocol_version: String,
            _capabilities: ClientCapabilities,
            _client_info: Implementation,
        ) -> tmcp::Result<InitializeResult> {
            Ok(InitializeResult::new("bundle-smoke"))
        }

        async fn list_tools(
            &self,
            _context: &ServerCtx,
            _cursor: Option<Cursor>,
        ) -> tmcp::Result<ListToolsResult> {
            Ok(ListToolsResult::new())
        }

        async fn call_tool(
            &self,
            _context: &ServerCtx,
            name: String,
            arguments: Option<Arguments>,
            _task: Option<TaskMetadata>,
        ) -> tmcp::Result<CallToolResponse> {
            if name != "script_eval" {
                return Err(McpError::ToolNotFound(name));
            }
            let request = arguments
                .ok_or_else(|| McpError::InternalError("missing arguments".to_string()))?
                .deserialize::<ScriptEvalRequest>()
                .map_err(|error| McpError::InternalError(error.to_string()))?;
            self.requests
                .lock()
                .expect("requests lock poisoned")
                .push(request.clone());
            if request.script == BUNDLE_COLLECTION_SCRIPT {
                let result = CallToolResult::new()
                    .with_json_text(serde_json::json!({
                        "success": true,
                        "value": {
                            "tree": {
                                "viewports": []
                            },
                            "text": "viewport root\n",
                            "shots": [{
                                "viewport_id": "root",
                                "name": "root",
                                "image": {
                                    "type": "image_ref",
                                    "id": "image-0"
                                }
                            }],
                            "errors": []
                        },
                        "images": [{
                            "id": "image-0",
                            "content_index": 1,
                            "kind": "viewport",
                            "viewport_id": "root"
                        }],
                        "logs": [],
                        "assertions": [],
                        "timing": {
                            "compile_ms": 0,
                            "exec_ms": 0,
                            "total_ms": 0
                        }
                    }))
                    .expect("collection json")
                    .with_content(ContentBlock::Image(
                        ImageContent::new("", "image/jpeg").with_data_bytes(b"jpeg"),
                    ));
                return Ok(result.into());
            }
            if request.script == BUNDLE_DIAGNOSTICS_SCRIPT {
                return Ok(CallToolResult::new()
                    .with_json_text(serde_json::json!({
                        "success": true,
                        "value": {
                            "values": {
                                "demo.runtime": {
                                    "ready": true
                                }
                            },
                            "errors": {}
                        },
                        "logs": [],
                        "assertions": [],
                        "timing": {
                            "compile_ms": 0,
                            "exec_ms": 0,
                            "total_ms": 0
                        }
                    }))
                    .expect("diagnostics json")
                    .into());
            }

            Ok(CallToolResult::new()
                .with_json_text(serde_json::json!({
                    "success": false,
                    "logs": ["before failure"],
                    "assertions": [{
                        "passed": false,
                        "message": "boom",
                        "location": "10_fail.luau:1"
                    }],
                    "fixtures": [{
                        "name": "basic.default",
                        "params": {
                            "offset": 180
                        }
                    }],
                    "timing": {
                        "compile_ms": 0,
                        "exec_ms": 1,
                        "total_ms": 1
                    },
                    "error": {
                        "type": "assertion",
                        "message": "boom",
                        "code": "assertion_failed",
                        "details": {
                            "widget": "basic.status"
                        }
                    }
                }))
                .expect("failure json")
                .into())
        }
    }

    struct FailingServer;

    #[async_trait]
    impl ServerHandler for FailingServer {
        async fn initialize(
            &self,
            _context: &ServerCtx,
            _protocol_version: String,
            _capabilities: ClientCapabilities,
            _client_info: Implementation,
        ) -> tmcp::Result<InitializeResult> {
            Ok(InitializeResult::new("mock"))
        }

        async fn list_tools(
            &self,
            _context: &ServerCtx,
            _cursor: Option<Cursor>,
        ) -> tmcp::Result<ListToolsResult> {
            Ok(ListToolsResult::new())
        }

        async fn call_tool(
            &self,
            _context: &ServerCtx,
            name: String,
            _arguments: Option<Arguments>,
            _task: Option<TaskMetadata>,
        ) -> tmcp::Result<CallToolResponse> {
            if name == "script_eval" {
                Err(McpError::InternalError("boom".to_string()))
            } else {
                Err(McpError::ToolNotFound(name))
            }
        }
    }

    async fn make_mock_app() -> (AppProcess, ServerHandle) {
        let ((server_reader, server_writer), (client_reader, client_writer)) = {
            let (sr, sw, cr, cw) = make_duplex_pair();
            ((sr, sw), (cr, cw))
        };

        let server = Server::new(|| MockServer);
        let handle = ServerHandle::from_stream(server, server_reader, server_writer)
            .await
            .expect("server handle");

        let mut client = Client::new("test", "0.1.0");
        client
            .connect_stream_raw(client_reader, client_writer)
            .await
            .expect("connect");
        client.init().await.expect("init");

        let app = AppProcess {
            child: None,
            process_group_id: None,
            client: Arc::new(AsyncMutex::new(client)),
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
        };

        (app, handle)
    }

    async fn make_failing_app() -> (AppProcess, ServerHandle) {
        let ((server_reader, server_writer), (client_reader, client_writer)) = {
            let (sr, sw, cr, cw) = make_duplex_pair();
            ((sr, sw), (cr, cw))
        };

        let server = Server::new(|| FailingServer);
        let handle = ServerHandle::from_stream(server, server_reader, server_writer)
            .await
            .expect("server handle");

        let mut client = Client::new("test", "0.1.0");
        client
            .connect_stream_raw(client_reader, client_writer)
            .await
            .expect("connect");
        client.init().await.expect("init");

        let app = AppProcess {
            child: None,
            process_group_id: None,
            client: Arc::new(AsyncMutex::new(client)),
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
        };

        (app, handle)
    }

    async fn make_health_failing_app() -> (AppProcess, ServerHandle) {
        let ((server_reader, server_writer), (client_reader, client_writer)) = {
            let (sr, sw, cr, cw) = make_duplex_pair();
            ((sr, sw), (cr, cw))
        };

        let server = Server::new(|| HealthFailingServer);
        let handle = ServerHandle::from_stream(server, server_reader, server_writer)
            .await
            .expect("server handle");

        let mut client = Client::new("test", "0.1.0");
        client
            .connect_stream_raw(client_reader, client_writer)
            .await
            .expect("connect");
        client.init().await.expect("init");

        let app = AppProcess {
            child: None,
            process_group_id: None,
            client: Arc::new(AsyncMutex::new(client)),
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
        };

        (app, handle)
    }

    async fn make_recording_eval_app(
        requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
    ) -> (AppProcess, ServerHandle) {
        let ((server_reader, server_writer), (client_reader, client_writer)) = {
            let (sr, sw, cr, cw) = make_duplex_pair();
            ((sr, sw), (cr, cw))
        };

        let server = Server::new(move || RecordingEvalServer {
            requests: Arc::clone(&requests),
        });
        let handle = ServerHandle::from_stream(server, server_reader, server_writer)
            .await
            .expect("server handle");

        let mut client = Client::new("test", "0.1.0");
        client
            .connect_stream_raw(client_reader, client_writer)
            .await
            .expect("connect");
        client.init().await.expect("init");

        let app = AppProcess {
            child: None,
            process_group_id: None,
            client: Arc::new(AsyncMutex::new(client)),
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
        };

        (app, handle)
    }

    async fn make_bundle_smoke_app(
        requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
    ) -> (AppProcess, ServerHandle) {
        let ((server_reader, server_writer), (client_reader, client_writer)) = {
            let (sr, sw, cr, cw) = make_duplex_pair();
            ((sr, sw), (cr, cw))
        };

        let server = Server::new(move || BundleSmokeServer {
            requests: Arc::clone(&requests),
        });
        let handle = ServerHandle::from_stream(server, server_reader, server_writer)
            .await
            .expect("server handle");

        let mut client = Client::new("test", "0.1.0");
        client
            .connect_stream_raw(client_reader, client_writer)
            .await
            .expect("connect");
        client.init().await.expect("init");

        let app = AppProcess {
            child: None,
            process_group_id: None,
            client: Arc::new(AsyncMutex::new(client)),
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
        };

        (app, handle)
    }

    #[tokio::test]
    async fn proxy_forwards_tool_calls() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();

        let mut state = make_state(&tempdir);
        state.status = AppStatus::Running;
        state.app = Some(app);

        let result = state
            .proxy_call(
                "script_eval",
                Some(script_eval_request_value(ScriptEvalRequest {
                    script: "return true".to_string(),
                    timeout_ms: Some(1_000),
                    options: None,
                })),
            )
            .await;
        let outcome = parse_script_eval_outcome(&result).expect("script eval outcome");
        assert!(outcome.success);
    }

    #[tokio::test]
    async fn restart_updates_state_on_success() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);

        let status = state
            .restart_with(|_, _| Box::pin(async move { Ok(app) }))
            .await
            .expect("restart");

        assert!(matches!(status, LifecycleStartStatus::Running));
        assert!(matches!(state.status, AppStatus::Running));
        assert!(state.app.is_some());
    }

    #[tokio::test]
    async fn start_is_idempotent_when_running() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        state.status = AppStatus::Running;
        state.app = Some(app);

        let status = state.start().await.expect("start");
        assert!(matches!(status, StartStatus::AlreadyRunning));
    }

    #[tokio::test]
    async fn restart_reports_startup_failure() {
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);

        let status = state
            .restart_with(|_, _| {
                Box::pin(async { Err(AppStartError::StartupFailed("startup output".to_string())) })
            })
            .await
            .expect("restart");

        assert!(matches!(
            status,
            LifecycleStartStatus::StartupFailed(ref output) if output == "startup output"
        ));
        assert!(matches!(
            state.status,
            AppStatus::StartupFailed { ref output } if output == "startup output"
        ));
    }

    #[tokio::test]
    async fn restart_sets_not_running_on_spawn_error() {
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);

        let error = state
            .restart_with(|_, _| {
                Box::pin(async { Err(AppStartError::Other("spawn failed".to_string())) })
            })
            .await
            .expect_err("restart should fail");

        assert!(matches!(error, EdevError::AppStart(_)));
        assert!(matches!(state.status, AppStatus::NotRunning));
        assert!(state.app.is_none());
    }

    #[tokio::test]
    async fn shutdown_clears_running_app() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        state.status = AppStatus::Running;
        state.app = Some(app);

        state.shutdown().await.expect("shutdown");

        assert!(matches!(state.status, AppStatus::NotRunning));
        assert!(state.app.is_none());
    }

    #[tokio::test]
    async fn restart_reports_startup_failure_when_readiness_probe_fails() {
        let (app, _handle) = make_failing_app().await;
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);

        let status = state
            .restart_with(|_, _| Box::pin(async move { Ok(app) }))
            .await
            .expect("restart");

        assert!(matches!(status, LifecycleStartStatus::StartupFailed(_)));
        assert!(matches!(state.status, AppStatus::StartupFailed { .. }));
        assert!(state.app.is_none());
    }

    #[tokio::test]
    async fn start_reports_restart_required_after_prior_startup_failure() {
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        state.status = AppStatus::StartupFailed {
            output: "boom".to_string(),
        };

        let status = state.start().await.expect("start");
        assert!(matches!(status, StartStatus::RestartRequired(ref output) if output == "boom"));
    }

    #[test]
    fn tools_list_is_static_across_lifecycle_states() {
        let tempdir = test_tempdir();
        let state = make_state(&tempdir);
        let stopped_names = state
            .tools_list()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut running_state = make_state(&tempdir);
        running_state.status = AppStatus::Running;
        let running_names = running_state
            .tools_list()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            stopped_names,
            vec![
                "start".to_string(),
                "stop".to_string(),
                "restart".to_string(),
                "status".to_string(),
                "script_eval".to_string(),
                "script_api".to_string(),
            ]
        );
        assert_eq!(stopped_names, running_names);
    }

    #[test]
    fn proxied_tool_names_include_live_script_surfaces() {
        assert!(is_proxied_tool("script_eval"));
        assert!(is_proxied_tool("script_api"));
        assert!(!is_proxied_tool("start"));
        assert!(!is_proxied_tool("restart"));
        assert!(!is_proxied_tool("widget_list"));
    }

    #[test]
    fn tool_error_reports_restart_failed_kind() {
        let result = tool_error(ErrorKind::RestartFailed, "restart failed");
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload["error"]["kind"], "restart_failed");
    }

    #[test]
    fn normalize_tool_result_adds_text_for_structured_errors() {
        let result = CallToolResult::error("INTERNAL", "Something broke");
        assert!(result.content.is_empty());
        let normalized = normalize_tool_result(result);
        assert!(!normalized.content.is_empty());
        match &normalized.content[0] {
            ContentBlock::Text(text) => assert_eq!(text.text, "Something broke"),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn parse_fixture_list_decodes_registered_fixtures() {
        let fixtures = parse_fixture_list(successful_outcome(&serde_json::json!([
            {
                "name": "basic.default",
                "description": "baseline",
                "anchors": [{ "widget_id": "basic.status", "check": "Visible" }],
                "params": [{
                    "name": "offset",
                    "kind": "float",
                    "description": "Scroll offset.",
                    "default": 300.0,
                    "min": 0.0,
                    "max": 600.0
                }],
                "tags": ["scroll"]
            }
        ])))
        .expect("fixtures");

        assert_eq!(fixtures.len(), 1);
        assert_eq!(fixtures[0].name, "basic.default");
        assert_eq!(fixtures[0].description, "baseline");
        assert_eq!(fixtures[0].anchors.len(), 1);
        assert_eq!(fixtures[0].params[0].name, "offset");
        assert_eq!(fixtures[0].tags, vec!["scroll"]);
    }

    #[test]
    fn parse_fixture_list_rejects_missing_payload() {
        let outcome = successful_outcome(&serde_json::Value::Null);

        let error = parse_fixture_list(outcome).expect_err("missing payload should fail");
        assert!(matches!(
            error,
            EdevError::FixtureFailed(ref message) if message == "fixtures() returned no value"
        ));
    }

    #[test]
    fn script_eval_error_message_prefers_runtime_error() {
        let message = script_eval_error_message(
            Some(&ScriptErrorInfo {
                error_type: "runtime".to_string(),
                message: "script exploded".to_string(),
                location: None,
                backtrace: None,
                code: None,
                details: None,
            }),
            "fallback",
        );

        assert_eq!(message, "script exploded");
    }

    #[test]
    fn restart_retry_detector_matches_transport_closed_variants() {
        assert!(restart_result_is_transport_closed(&Err(EdevError::Mcp(
            McpError::TransportDisconnected,
        ))));
        assert!(restart_result_is_transport_closed(&Err(EdevError::Mcp(
            McpError::ConnectionClosed,
        ))));
        assert!(restart_result_is_transport_closed(&Err(EdevError::Mcp(
            McpError::Transport("transport closed".to_string()),
        ))));
    }

    #[test]
    fn restart_retry_detector_ignores_non_transport_errors() {
        assert!(!restart_result_is_transport_closed(&Err(EdevError::Mcp(
            McpError::InternalError("boom".to_string()),
        ))));
        assert!(!restart_result_is_transport_closed(&Err(
            EdevError::AppStart("spawn failed".to_string(),)
        )));
    }

    #[test]
    fn restart_result_detector_matches_startup_failed_transport_closed() {
        assert!(restart_result_is_transport_closed(&Ok(
            LifecycleStartStatus::StartupFailed("Transport disconnected unexpectedly".to_string()),
        )));
        assert!(!restart_result_is_transport_closed(&Ok(
            LifecycleStartStatus::StartupFailed("error[E0432]: unresolved import".to_string()),
        )));
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        state.status = AppStatus::Running;
        state.app = Some(app);

        let stopped = state.stop_app().await.expect("stop");
        let already_stopped = state.stop_app().await.expect("stop");

        assert!(matches!(stopped, StopStatus::Stopped));
        assert!(matches!(already_stopped, StopStatus::AlreadyStopped));
        assert!(matches!(state.status, AppStatus::NotRunning));
    }

    #[test]
    fn status_report_covers_all_lifecycle_states() {
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        assert_eq!(state.status_report().state, "not_running");
        assert_eq!(state.status_report().idle_shutdown.state, "disabled");
        assert!(state.status_report().app_health.is_none());

        state.status = AppStatus::Starting;
        assert_eq!(state.status_report().state, "starting");

        state.status = AppStatus::Running;
        assert_eq!(state.status_report().state, "running");

        state.status = AppStatus::StartupFailed {
            output: "boom".to_string(),
        };
        let report = state.status_report();
        assert_eq!(report.state, "startup_failed");
        assert_eq!(report.startup_output.as_deref(), Some("boom"));
    }

    #[test]
    fn status_report_covers_mcp_idle_state() {
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        state.enable_idle_shutdown(Duration::from_secs(30));

        let report = state.status_report();
        assert!(!report.mcp_client_attached);
        assert_eq!(report.idle_shutdown.state, "waiting_for_initial_client");
        assert_eq!(report.idle_shutdown.configured_secs, Some(30));
        assert!(matches!(report.idle_shutdown.remaining_secs, Some(0..=30)));

        state.mark_client_attached();
        let report = state.status_report();
        assert!(report.mcp_client_attached);
        assert_eq!(
            report.idle_shutdown.state,
            "suspended_while_client_attached"
        );
        assert_eq!(report.idle_shutdown.configured_secs, Some(30));
        assert_eq!(report.idle_shutdown.remaining_secs, None);
    }

    #[tokio::test]
    async fn status_omits_app_health_when_lifecycle_only() {
        let tempdir = test_tempdir();
        let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
        let server = EdevServer { state };
        let ctx = TestServerContext::new();

        let result = server
            .call_tool(ctx.ctx(), "status".to_string(), None, None)
            .await
            .expect("status")
            .into_result()
            .expect("immediate result");
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload["state"], "not_running");
        assert!(payload["app_health"].is_null());
        assert!(payload["app_health_error"].is_null());
    }

    #[tokio::test]
    async fn status_merges_running_app_health() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();
        let mut raw_state = make_state(&tempdir);
        raw_state.status = AppStatus::Running;
        raw_state.app = Some(app);
        let state = Arc::new(AsyncMutex::new(raw_state));
        let server = EdevServer { state };
        let ctx = TestServerContext::new();

        let result = server
            .call_tool(ctx.ctx(), "status".to_string(), None, None)
            .await
            .expect("status")
            .into_result()
            .expect("immediate result");
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload["state"], "running");
        assert_eq!(payload["app_health"]["frame_count"], 4);
        assert_eq!(payload["app_health"]["known_viewports"][0], "root");
        assert!(payload["app_health_error"].is_null());
    }

    #[tokio::test]
    async fn status_tolerates_running_app_health_proxy_failure() {
        let (app, _handle) = make_health_failing_app().await;
        let tempdir = test_tempdir();
        let mut raw_state = make_state(&tempdir);
        raw_state.status = AppStatus::Running;
        raw_state.app = Some(app);
        let state = Arc::new(AsyncMutex::new(raw_state));
        let server = EdevServer { state };
        let ctx = TestServerContext::new();

        let result = server
            .call_tool(ctx.ctx(), "status".to_string(), None, None)
            .await
            .expect("status")
            .into_result()
            .expect("immediate result");
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload["state"], "running");
        assert!(payload["app_health"].is_null());
        assert!(
            payload["app_health_error"]
                .as_str()
                .expect("health error")
                .contains("health boom")
        );
    }

    #[tokio::test]
    async fn script_api_is_available_while_stopped() {
        let tempdir = test_tempdir();
        let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
        let server = EdevServer { state };
        let ctx = TestServerContext::new();

        let result = server
            .call_tool(ctx.ctx(), "script_api".to_string(), None, None)
            .await
            .expect("script_api")
            .into_result()
            .expect("immediate result");
        assert_eq!(result.text().expect("text"), script_definitions());
    }

    #[tokio::test]
    async fn script_api_proxies_to_running_app() {
        let (app, _handle) = make_mock_app().await;
        let tempdir = test_tempdir();
        let mut raw_state = make_state(&tempdir);
        raw_state.status = AppStatus::Running;
        raw_state.app = Some(app);
        let state = Arc::new(AsyncMutex::new(raw_state));
        let server = EdevServer { state };
        let ctx = TestServerContext::new();

        let result = server
            .call_tool(ctx.ctx(), "script_api".to_string(), None, None)
            .await
            .expect("script_api")
            .into_result()
            .expect("immediate result");
        assert_eq!(result.text().expect("text"), "live app script api");
    }

    #[tokio::test]
    async fn script_eval_returns_call_start_when_app_is_stopped() {
        let tempdir = test_tempdir();
        let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
        let server = EdevServer { state };
        let ctx = TestServerContext::new();

        let result = server
            .call_tool(
                ctx.ctx(),
                "script_eval".to_string(),
                Some(script_eval_request_value(ScriptEvalRequest {
                    script: "return true".to_string(),
                    timeout_ms: Some(1_000),
                    options: None,
                })),
                None,
            )
            .await
            .expect("script_eval")
            .into_result()
            .expect("immediate result");
        let payload = result
            .structured_content
            .clone()
            .expect("structured content");
        assert_eq!(payload["error"]["kind"], "app_not_running");
        assert_eq!(
            result.text().expect("text"),
            "App is not running. Call start."
        );
    }

    #[tokio::test]
    async fn idle_shutdown_waits_for_inactivity() {
        let tempdir = test_tempdir();
        let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
        {
            let mut state_guard = state.lock().await;
            state_guard.mark_activity();
        }

        let waiter = tokio::spawn(wait_for_idle_shutdown(
            Arc::clone(&state),
            Duration::from_millis(80),
        ));

        sleep(Duration::from_millis(30)).await;
        {
            let mut state_guard = state.lock().await;
            state_guard.mark_activity();
        }
        sleep(Duration::from_millis(40)).await;
        assert!(!waiter.is_finished());

        sleep(Duration::from_millis(60)).await;
        assert!(waiter.is_finished());
    }

    #[tokio::test]
    async fn idle_shutdown_stays_pending_while_client_attached() {
        let tempdir = test_tempdir();
        let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
        {
            let mut state_guard = state.lock().await;
            state_guard.last_activity = Instant::now() - Duration::from_millis(250);
            state_guard.mark_client_attached();
        }

        let wait_result = timeout(
            Duration::from_millis(20),
            wait_for_idle_shutdown(Arc::clone(&state), Duration::from_millis(100)),
        )
        .await;
        assert!(
            wait_result.is_err(),
            "attached MCP clients should suspend idle shutdown"
        );
    }

    #[tokio::test]
    async fn list_tools_does_not_delay_idle_shutdown() {
        let tempdir = test_tempdir();
        let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
        {
            let mut state_guard = state.lock().await;
            state_guard.last_activity = Instant::now() - Duration::from_millis(250);
        }

        let server = EdevServer {
            state: Arc::clone(&state),
        };
        let ctx = TestServerContext::new();
        let _result = server
            .list_tools(ctx.ctx(), None::<Cursor>)
            .await
            .expect("list tools");

        let wait_result = timeout(
            Duration::from_millis(20),
            wait_for_idle_shutdown(Arc::clone(&state), Duration::from_millis(100)),
        )
        .await;
        assert!(
            wait_result.is_ok(),
            "list_tools should not refresh idle activity"
        );
    }
}
