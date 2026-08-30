//! One-shot Edev command execution.

use super::*;

/// Run the Eguidev launcher on stdio.
pub async fn run() -> Result<(), EdevError> {
    let args = env::args_os().collect::<Vec<_>>();
    if process_lifecycle::is_supervisor_invocation(&args) {
        return process_lifecycle::run_hidden_supervisor(&args)
            .await
            .map_err(EdevError::AppStart);
    }
    match EdevCommand::from_env()? {
        EdevCommand::Help(help) => {
            print!("{help}");
            Ok(())
        }
        EdevCommand::Docs => {
            print!("{}", script_definitions());
            Ok(())
        }
        EdevCommand::Mcp(config) => run_mcp(config).await,
        EdevCommand::Smoke(config) => run_smoke(config).await,
        EdevCommand::Record(config) => run_record(config).await,
        EdevCommand::Eval(config) => run_eval(config).await,
        EdevCommand::Dump(config) => run_dump(config).await,
        EdevCommand::Fixture(config) => run_fixture(config).await,
    }
}

/// Run the long-lived `edev mcp` launcher server over stdio without starting
/// the app eagerly.
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

/// Run a smoke suite while recording the selected app window.
async fn run_record(config: RecordConfig) -> Result<(), EdevError> {
    recording::ensure_supported()?;
    prepare_record_outfile(&config.outfile)?;

    let session =
        start_smoke_session(&config.smoke, "record command could not reach the app").await?;
    let title = match config.window_title {
        Some(title) if !title.trim().is_empty() => {
            wait_for_initial_capture_refresh(&session.client, config.smoke.suite.script_timeout)
                .await?;
            title
        }
        Some(_) | None => {
            root_viewport_title(&session.client, config.smoke.suite.script_timeout).await?
        }
    };
    let app_process_ids = recording::process_group_members(session.process_group_id())
        .into_iter()
        .collect::<BTreeSet<_>>();
    let recording_request = recording::RecordingRequest {
        outfile: config.outfile.clone(),
        title,
        app_process_ids,
    };
    let recorder = match start_recording_with_retries(
        &session.client,
        config.smoke.suite.script_timeout,
        recording_request,
    )
    .await
    {
        Ok(recorder) => recorder,
        Err(error) => {
            if let Err(shutdown_error) = session.shutdown().await {
                eprintln!("edev: shutdown failed after recording startup error: {shutdown_error}");
            }
            return Err(error);
        }
    };

    let suite_config = config.smoke.clone();
    let suite_client = Arc::clone(&session.client);
    let suite_bundle_context = session.bundle_context(&suite_config);
    let suite_task = tokio::spawn(async move {
        run_smoke_suite(suite_client, &suite_config, suite_bundle_context).await
    });
    tokio::pin!(suite_task);
    let suite_result = tokio::select! {
        result = &mut suite_task => match result {
            Ok(result) => result,
            Err(error) => Err(EdevError::SmokeFailed(format!("smoke task failed: {error}"))),
        },
        _ = shutdown_signal() => {
            suite_task.abort();
            Err(EdevError::RecordFailed(
                "recording interrupted by shutdown signal".to_string(),
            ))
        },
    };
    let recording_result = block_in_place(|| recorder.stop());
    let shutdown_result = session.shutdown().await;
    finish_record_run(
        suite_result,
        recording_result,
        shutdown_result,
        config.smoke.verbose_output,
    )
}

/// Run the checked-in smoke suite once and exit non-zero on any smoke failure.
async fn run_smoke(config: SmokeConfig) -> Result<(), EdevError> {
    if config.list {
        return print_smoke_list(&config);
    }

    let session = start_smoke_session(&config, "smoke runner could not reach the app").await?;
    let bundle_context = session.bundle_context(&config);
    let result = run_smoke_suite(Arc::clone(&session.client), &config, bundle_context).await;
    let shutdown_result = session.shutdown().await;
    finish_smoke_run(result, shutdown_result, config.verbose_output)
}

/// Finish a smoke command with the historical failure precedence.
fn finish_smoke_run(
    result: Result<SuiteResult, EdevError>,
    shutdown_result: Result<(), EdevError>,
    verbose_output: bool,
) -> Result<(), EdevError> {
    match result {
        Ok(summary) => {
            for line in summary.render_lines(verbose_output) {
                println!("{line}");
            }
            if let Err(error) = shutdown_result {
                eprintln!("edev: shutdown failed: {error}");
            }
            if summary.success() {
                Ok(())
            } else {
                Err(EdevError::SmokeFailed("smoke suite failed".to_string()))
            }
        }
        Err(error) => {
            if let Err(shutdown_error) = shutdown_result {
                eprintln!("edev: shutdown failed: {shutdown_error}");
            }
            Err(error)
        }
    }
}

/// Finish a record command while preserving smoke failures over recording
/// errors.
fn finish_record_run(
    suite_result: Result<SuiteResult, EdevError>,
    recording_result: Result<recording::RecordingSummary, EdevError>,
    shutdown_result: Result<(), EdevError>,
    verbose_output: bool,
) -> Result<(), EdevError> {
    match suite_result {
        Err(error) => {
            if let Err(recording_error) = recording_result {
                eprintln!("edev: recording finalization failed: {recording_error}");
            }
            if let Err(shutdown_error) = shutdown_result {
                eprintln!("edev: shutdown failed: {shutdown_error}");
            }
            Err(error)
        }
        Ok(summary) => {
            for line in summary.render_lines(verbose_output) {
                println!("{line}");
            }
            let recording_summary = match recording_result {
                Ok(summary) => summary,
                Err(error) => {
                    if let Err(shutdown_error) = shutdown_result {
                        eprintln!("edev: shutdown failed after recording error: {shutdown_error}");
                    }
                    return Err(error);
                }
            };
            shutdown_result?;
            let file_size = fs::metadata(&recording_summary.outfile)
                .map(|metadata| metadata.len())
                .unwrap_or(recording_summary.file_size)
                .max(recording_summary.file_size);
            eprintln!(
                "edev: wrote recording {} ({} bytes)",
                recording_summary.outfile.display(),
                file_size
            );
            if summary.success() {
                Ok(())
            } else {
                Err(EdevError::SmokeFailed("smoke suite failed".to_string()))
            }
        }
    }
}

/// Start an app-backed smoke session and connect to the app MCP server.
async fn start_smoke_session(
    config: &SmokeConfig,
    unavailable_message: &str,
) -> Result<AppSession, EdevError> {
    let launch = required_smoke_launch(config)?;
    AppSession::start(launch, unavailable_message).await
}

/// Return the launch config required for app-backed smoke execution.
fn required_smoke_launch(config: &SmokeConfig) -> Result<LaunchConfig, EdevError> {
    config.launch.clone().ok_or_else(|| {
        EdevError::InvalidArgs(
            "no app command configured; add app.command to .edev.toml or pass one after --"
                .to_string(),
        )
    })
}

/// Start native recording, waiting on fresh captures if the window server lags
/// startup.
async fn start_recording_with_retries(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    timeout: Option<Duration>,
    request: recording::RecordingRequest,
) -> Result<recording::NativeRecording, EdevError> {
    let mut last_error = None;
    for attempt in 0..RECORD_WINDOW_DISCOVERY_ATTEMPTS {
        if attempt > 0 {
            wait_for_initial_capture_refresh(client, timeout).await?;
        }
        match block_in_place(|| recording::start(&request)) {
            Ok(recording) => return Ok(recording),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        EdevError::RecordFailed("recording could not find a native window".to_string())
    }))
}

/// Internal script used to synchronize native window probing with a fresh app
/// capture.
const WAIT_FOR_CAPTURE_SCRIPT: &str = r#"
eguidev.root:wait_capture()
return true
"#;

/// Wait for the app to publish a fresh capture through the existing script API.
async fn wait_for_initial_capture_refresh(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    timeout: Option<Duration>,
) -> Result<(), EdevError> {
    let timeout_ms = timeout.map(|duration| duration.as_millis() as u64);
    let outcome = call_script_eval(client, WAIT_FOR_CAPTURE_SCRIPT, timeout_ms)
        .await
        .map_err(EdevError::RecordFailed)?;
    if outcome.success {
        Ok(())
    } else {
        Err(EdevError::RecordFailed(script_eval_error_message(
            outcome.error.as_ref(),
            "failed to wait for a fresh capture before recording",
        )))
    }
}

/// Internal script used to get the root viewport title for native window
/// matching.
const ROOT_VIEWPORT_TITLE_SCRIPT: &str = r#"
eguidev.root:wait_capture()
local state = eguidev.root:state()
return if state == nil then nil else state.title
"#;

/// Read the root viewport title through the existing script API.
async fn root_viewport_title(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    timeout: Option<Duration>,
) -> Result<String, EdevError> {
    let timeout_ms = timeout.map(|duration| duration.as_millis() as u64);
    let outcome = call_script_eval(client, ROOT_VIEWPORT_TITLE_SCRIPT, timeout_ms)
        .await
        .map_err(EdevError::RecordFailed)?;
    if !outcome.success {
        return Err(EdevError::RecordFailed(script_eval_error_message(
            outcome.error.as_ref(),
            "failed to read root viewport title",
        )));
    }
    let title = outcome
        .value
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| {
            EdevError::RecordFailed(
                "root viewport has no title; pass --window-title <TITLE>".to_string(),
            )
        })?;
    Ok(title.to_string())
}

/// Prepare the recording output path before starting the app.
fn prepare_record_outfile(path: &Path) -> Result<(), EdevError> {
    if path.is_dir() {
        return Err(EdevError::RecordFailed(format!(
            "recording output is a directory: {}",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
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

/// Run one Luau script through `script_eval`, print JSON, and write returned
/// images.
async fn run_eval(config: EvalConfig) -> Result<(), EdevError> {
    let source = fs::read_to_string(&config.script)?;
    let session = AppSession::start(
        config.launch.clone(),
        "eval command could not reach the app",
    )
    .await?;
    let result = run_eval_script(Arc::clone(&session.client), &config, source).await;
    session.finish(result).await
}

/// Launch the app, optionally apply a fixture, print a dump, and exit.
async fn run_dump(config: DumpConfig) -> Result<(), EdevError> {
    let session = AppSession::start(
        config.launch.clone(),
        "dump command could not reach the app",
    )
    .await?;
    let result = run_dump_script(Arc::clone(&session.client), &config).await;
    session.finish(result).await
}

/// Execute the generated dump script and emit only the requested dump payload.
async fn run_dump_script(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    config: &DumpConfig,
) -> Result<(), EdevError> {
    let result = call_script_eval_result(
        &client,
        ScriptEvalRequest {
            script: DUMP_SCRIPT.to_string(),
            timeout_ms: config.timeout.map(|duration| duration.as_millis() as u64),
            options: Some(ScriptEvalOptions {
                source_name: Some("@edev_dump.luau".to_string()),
                args: dump_script_args(config),
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

/// Build scalar script arguments for the checked-in dump projection.
pub fn dump_script_args(config: &DumpConfig) -> ScriptArgs {
    let mut args = config.params.clone();
    if let Some(name) = &config.fixture {
        args.insert(
            "__fixture_name".to_string(),
            ScriptArgValue::String(name.clone()),
        );
    }
    args.insert(
        "__dump_wait_capture".to_string(),
        ScriptArgValue::Bool(config.wait_for_initial_capture),
    );
    args.insert("__dump_json".to_string(), ScriptArgValue::Bool(config.json));
    if let Some(viewport) = &config.viewport {
        args.insert(
            "__dump_viewport".to_string(),
            ScriptArgValue::String(viewport.clone()),
        );
    }
    args
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

/// Execute one script against a launched app and emit the eval result.
pub async fn run_eval_script(
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
    let session = AppSession::start(
        config.launch.clone(),
        "fixture command could not reach the app",
    )
    .await?;
    let client = Arc::clone(&session.client);

    // Query registered fixtures.
    let fixtures =
        match eval_fixture_script(&client, FIXTURE_LIST_SCRIPT, "failed to query fixtures")
            .await
            .and_then(parse_fixture_list)
        {
            Ok(fixtures) => fixtures,
            Err(error) => {
                session.shutdown().await?;
                return Err(error);
            }
        };

    if fixtures.is_empty() {
        if config.json || config.markdown {
            print_fixture_list(&config, &fixtures)?;
        } else {
            println!("No fixtures registered.");
        }
        session.shutdown().await?;
        return Ok(());
    }

    let Some(name) = config.name else {
        // List-only mode.
        print_fixture_list(&config, &fixtures)?;
        session.shutdown().await?;
        return Ok(());
    };

    // Validate the fixture name exists.
    let Some(fixture) = fixtures.iter().find(|f| f.name == name) else {
        eprintln!("error: unknown fixture \"{name}\"\n");
        print_fixture_table(&fixtures);
        session.shutdown().await?;
        return Err(EdevError::FixtureFailed(format!("unknown fixture: {name}")));
    };

    let outcome = match eval_fixture_apply(&client, &name, &config.params, !config.no_wait).await {
        Ok(outcome) => outcome,
        Err(error) => {
            session.shutdown().await?;
            return Err(error);
        }
    };
    print_fixture_result(fixture, outcome.value.as_ref());
    if config.no_wait {
        println!("ready: not waited (--no-wait)");
    }

    if config.dump {
        match eval_fixture_script(
            &client,
            "return eguidev.dump_text()",
            "post-fixture dump failed",
        )
        .await
        {
            Ok(outcome) => print_fixture_dump(outcome)?,
            Err(error) => {
                session.shutdown().await?;
                return Err(error);
            }
        }
    }

    eprintln!("Fixture \"{name}\" applied. Press ctrl-c to stop.");
    shutdown_signal().await;
    session.shutdown().await?;
    Ok(())
}

/// Apply one fixture through the checked-in `script_eval` projection.
async fn eval_fixture_apply(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    name: &str,
    params: &BTreeMap<String, ScriptArgValue>,
    wait: bool,
) -> Result<ScriptEvalOutcome, EdevError> {
    let mut args = params.clone();
    args.insert(
        "__fixture_name".to_string(),
        ScriptArgValue::String(name.to_string()),
    );
    args.insert("__fixture_wait".to_string(), ScriptArgValue::Bool(wait));
    let result = call_script_eval_result(
        client,
        ScriptEvalRequest {
            script: FIXTURE_APPLY_SCRIPT.to_string(),
            timeout_ms: Some(10_000),
            options: Some(ScriptEvalOptions {
                source_name: Some("@edev_fixture_apply.luau".to_string()),
                args,
            }),
        },
    )
    .await
    .map_err(EdevError::FixtureFailed)?;
    let outcome = parse_script_eval_outcome(&result).map_err(EdevError::FixtureFailed)?;
    if outcome.success {
        Ok(outcome)
    } else {
        Err(EdevError::FixtureFailed(script_eval_error_message(
            outcome.error.as_ref(),
            "fixture application failed",
        )))
    }
}

/// Decodes structured content or the first JSON text block with stable domain
/// errors.
pub fn decode_tool_result<T: DeserializeOwned>(
    result: &CallToolResult,
    tool_name: &str,
    decoded_name: &str,
) -> Result<T, String> {
    result
        .extract_as(ToolResultMode::StructuredOrFirstJsonText)
        .map_err(|error| match error {
            ToolResultDecodeError::Extract(ToolResultExtractError::MissingTextContent) => {
                format!("{tool_name} response was missing JSON content")
            }
            ToolResultDecodeError::Extract(ToolResultExtractError::InvalidJsonText { message }) => {
                format!("failed to parse {tool_name} response: {message}")
            }
            ToolResultDecodeError::Deserialize { message } => {
                format!("failed to decode {decoded_name}: {message}")
            }
            other => format!("failed to parse {tool_name} response: {other}"),
        })
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

/// Print the shared script result returned by fixture application.
fn print_fixture_result(fixture: &FixtureSpec, result: Option<&serde_json::Value>) {
    println!("Fixture: {}", fixture.name);
    if !fixture.description.is_empty() {
        println!("{}", fixture.description);
    }
    if let Some(result) = result {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        );
    }
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
    println!("| Fixture | Description | Params | Tags | Pre | Ready |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for fixture in fixtures {
        println!(
            "| {} | {} | {} | {} | {} | {} |",
            markdown_cell(&fixture.name),
            markdown_cell(&fixture.description),
            markdown_cell(&fixture_params_summary(&fixture.params)),
            markdown_cell(&fixture.tags.join(", ")),
            fixture.preconditions.len(),
            fixture.ready.len()
        );
    }
}

/// Escape a Markdown table cell.
fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|")
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

/// Start the app and resolve its direct client, shutting down on startup
/// failures.
pub async fn start_app_client(
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

    match state.app_client() {
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

/// Call the app-side `script_eval` tool and preserve all returned content
/// blocks.
pub async fn call_script_eval_result(
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
pub fn eval_output_value(
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

/// Prefer the runtime's script error text and fall back to a caller-provided
/// message.
pub fn script_eval_error_message(
    error: Option<&ScriptErrorInfo>,
    fallback_message: &str,
) -> String {
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
        if f.is_transition() {
            details.push(format!("{} pre", f.preconditions.len()));
        }
        if !f.ready.is_empty() {
            details.push(format!(
                "{} ready{}",
                f.ready.len(),
                if f.ready.len() == 1 { "" } else { "s" }
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

#[cfg(test)]
mod tests {
    use eguidev_runtime::smoke::{ScriptResult, ScriptStatus, SuiteResult};

    use super::*;

    fn failed_suite() -> SuiteResult {
        SuiteResult {
            results: vec![ScriptResult {
                round: 1,
                path: "10_fail.luau".to_string(),
                status: ScriptStatus::Fail,
                elapsed_ms: 5,
                message: Some("boom".to_string()),
                logs: Vec::new(),
                fixtures: Vec::new(),
                egui_diagnostics: eguidev_runtime::EguiDiagnosticBatch::default(),
                details: None,
            }],
            rounds: Vec::new(),
            requested_rounds: 1,
            elapsed_ms: 5,
        }
    }

    #[test]
    fn finish_smoke_run_keeps_suite_failure_through_forced_shutdown() {
        let result = finish_smoke_run(
            Ok(failed_suite()),
            Err(EdevError::AppStart("forced".to_string())),
            false,
        );
        assert!(
            matches!(result, Err(EdevError::SmokeFailed(_))),
            "{result:?}"
        );
    }
}
