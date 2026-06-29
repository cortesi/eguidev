//! Project maintenance tasks for the workspace.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use clap::{Args as ClapArgs, Parser, Subcommand};
use eguidev_runtime::script_definitions;
use ruau::{
    analysis::resolve::AnalysisMode,
    typecheck::{
        checker::{Checker, Config},
    },
};
use serde_json::{Value, json};
use tmcp::{Client, schema::CallToolResult};
use tokio::{process::Command as TokioCommand, runtime::Builder};

/// Project maintenance runner.
#[derive(Parser)]
#[command(author, version, about = "Project maintenance tasks.")]
struct Args {
    /// Maintenance command to run.
    #[command(subcommand)]
    command: Task,
}

/// Supported maintenance tasks.
#[derive(Subcommand)]
enum Task {
    /// Run formatter and clippy fixes.
    Tidy,
    /// Run tests via nextest.
    Test,
    /// Run the direct smoketest suite.
    Smoke(SmokeArgs),
    /// Run the minimal edev transport smoke.
    #[command(name = "smoke-edev", visible_alias = "smoke-edit")]
    SmokeEdev(SmokeArgs),
}

#[derive(ClapArgs, Debug, Clone)]
/// Output controls for the smoke task.
struct SmokeArgs {
    /// Enable verbose smoke logging.
    #[arg(short, long)]
    verbose: bool,
    /// Run only these smoke scripts, in the order provided.
    #[arg(value_name = "SCRIPT")]
    scripts: Vec<PathBuf>,
    /// Stop the suite after the first smoketest failure.
    #[arg(long)]
    fail_fast: bool,
}

/// Entry point for the workspace xtask runner.
fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    match args.command {
        Task::Tidy => tidy(),
        Task::Test => test(),
        Task::Smoke(args) => smoke(&args),
        Task::SmokeEdev(args) => smoke_edev(&args),
    }
}

/// Run formatter and clippy with workspace defaults.
fn tidy() -> Result<(), Box<dyn Error>> {
    run_command(
        "cargo",
        &[
            "+nightly",
            "fmt",
            "--all",
            "--",
            "--config-path",
            "./rustfmt-nightly.toml",
        ],
        "cargo fmt",
    )?;
    run_command(
        "cargo",
        &[
            "clippy",
            "-q",
            "--fix",
            "--all",
            "--all-targets",
            "--all-features",
            "--allow-dirty",
            "--tests",
            "--examples",
        ],
        "cargo clippy",
    )?;
    Ok(())
}

/// Run the test suite via nextest.
fn test() -> Result<(), Box<dyn Error>> {
    run_command("cargo", &["nextest", "run", "--all"], "cargo nextest")?;
    run_command(
        "cargo",
        &["test", "-q", "-p", "eguidev_runtime"],
        "cargo test -p eguidev_runtime",
    )?;
    run_command(
        "cargo",
        &["test", "-q", "-p", "eguidev_demo", "--features", "devtools"],
        "cargo test -p eguidev_demo --features devtools",
    )?;
    run_command(
        "cargo",
        &[
            "check",
            "-q",
            "-p",
            "eguidev",
            "--target",
            "wasm32-unknown-unknown",
        ],
        "cargo check -p eguidev --target wasm32-unknown-unknown",
    )?;
    check_luau_definitions()?;
    check_default_eguidev_dependency_surface()?;
    Ok(())
}

/// Run the direct Luau smoketest suite against the demo app.
fn smoke(args: &SmokeArgs) -> Result<(), Box<dyn Error>> {
    let workspace_root = workspace_root()?;
    let mut demo_command = Command::new("cargo");
    demo_command.current_dir(&workspace_root);
    demo_command.args(["run", "-q", "-p", "edev", "--", "smoke"]);
    if args.fail_fast {
        demo_command.arg("--fail-fast");
    }
    if args.verbose {
        demo_command.arg("--verbose");
    }
    demo_command.args(&args.scripts);
    run_prepared_command_with_timeout(
        demo_command,
        "cargo run -p edev -- smoke",
        Some(Duration::from_secs(15 * 60)),
    )
}

/// Run the edev transport smoke against the demo app.
fn smoke_edev(args: &SmokeArgs) -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(smoke_edev_transport(args.verbose))
}

/// Type-check checked-in Luau definitions and shipped script sources.
fn check_luau_definitions() -> Result<(), Box<dyn Error>> {
    let definitions_path = Path::new("crates/eguidev_runtime/luau/eguidev.d.luau");
    let definitions = fs::read_to_string(definitions_path)?;
    check_luau_source(definitions_path, "eguidev.d.luau", &definitions)?;

    for source_path in luau_sources()? {
        let source = fs::read_to_string(&source_path)?;
        let module_name = source_path
            .to_str()
            .map(|path| path.replace('\\', "/"))
            .unwrap_or_else(|| "script.luau".to_string());
        let source = source_with_luau_definitions(&definitions, &source);
        check_luau_source(&source_path, &module_name, &source)?;
    }

    Ok(())
}

/// Prefix a script with the checked-in declaration surface for Ruau's single-module checker.
fn source_with_luau_definitions(definitions: &str, source: &str) -> String {
    format!("{definitions}\n\n{source}")
}

/// Check one Luau source with Ruau's checker and surface any diagnostics.
fn check_luau_source(path: &Path, module_name: &str, source: &str) -> Result<(), Box<dyn Error>> {
    let mut checker = Checker::new();
    let checked = checker.check_source_with_config(source, luau_checker_config());
    if checked.has_errors() {
        let diagnostics = checked.diagnostics().render(module_name);
        return Err(format!("Luau check failed for {}:\n{diagnostics}", path.display()).into());
    }
    Ok(())
}

/// Return the checker settings used for shipped script validation.
fn luau_checker_config() -> Config {
    Config::with_source_mode(AnalysisMode::Strict)
}

/// Enumerate checked-in example scripts that should type-check against the API definitions.
fn luau_sources() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut sources = Vec::new();
    collect_luau_files(Path::new("docs/examples"), &mut sources)?;
    collect_luau_files(Path::new("smoketest"), &mut sources)?;
    sources.sort();
    Ok(sources)
}

/// Recursively collect `.luau` files under the provided root.
fn collect_luau_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    if !root.exists() {
        return Ok(());
    }

    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_luau_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("luau") {
            files.push(path);
        }
    }

    Ok(())
}

/// Spawn edev, connect over MCP, and validate a minimal Luau transport flow.
async fn smoke_edev_transport(verbose: bool) -> Result<(), Box<dyn Error>> {
    let workspace_root = workspace_root()?;
    let mut client = Client::new("xtask-smoke", env!("CARGO_PKG_VERSION"))
        .with_request_timeout(Duration::from_secs(120));
    let mut command = TokioCommand::new("cargo");
    command.current_dir(&workspace_root);
    command.args(["run", "-q", "-p", "edev", "--", "mcp"]);
    if verbose {
        command.arg("--verbose");
    }

    let tmcp::SpawnedServer { mut process, .. } = client.connect_process(command).await?;
    let start = Instant::now();
    let smoke_result = async {
        let tools = client.list_tools(None).await?;
        for expected in [
            "start",
            "stop",
            "restart",
            "status",
            "script_eval",
            "script_api",
        ] {
            if !tools.tools.iter().any(|tool| tool.name == expected) {
                return Err(format!("missing expected tool: {expected}").into());
            }
        }

        let script_api_result = client.call_tool("script_api", json!({})).await?;
        let script_api = script_api_result
            .text()
            .ok_or("script_api response did not include text content")?;
        if script_api != script_definitions() {
            return Err("script_api payload did not match checked-in definitions".into());
        }

        let status_before = client.call_tool("status", json!({})).await?;
        let status_before_payload = status_before
            .structured_content
            .ok_or("status response did not include structured content")?;
        if status_before_payload["state"] != Value::String("not_running".to_string()) {
            return Err(
                format!("expected initial state=not_running: {status_before_payload}").into(),
            );
        }

        let start_result = client.call_tool("start", json!({})).await?;
        let start_payload = start_result
            .structured_content
            .ok_or("start response did not include structured content")?;
        if start_payload["ok"] != Value::Bool(true) {
            return Err(format!("start returned failure payload: {start_payload}").into());
        }

        let result = client
            .call_tool(
                "script_eval",
                json!({
                    "script": r#"
local available = fixtures()
local has_default = false
for _, spec in ipairs(available) do
    if spec.name == "basic.default" then
        has_default = true
        break
    end
end
assert(has_default, "basic.default fixture should be registered")
fixture("basic.default")
local submit = root():widget_get("basic.submit")
local submit_state = submit:state()
assert(submit_state.role == "button", "submit should expose button role")
local status_state = root():widget_get("basic.status"):state()
assert(status_state.label ~= nil, "status should expose text")
return {
    fixture_count = #available,
    status = tostring(status_state.label),
    submit_role = submit_state.role,
}
"#,
                    "timeout_ms": 10_000,
                    "options": {
                        "source_name": "smoke.luau"
                    }
                }),
            )
            .await?;
        let payload = parse_tool_json_text(&result)?;
        if payload["success"] != Value::Bool(true) {
            return Err(format!("script_eval returned failure payload: {payload}").into());
        }
        let status = payload["value"]["status"]
            .as_str()
            .ok_or_else(|| format!("missing final status in script_eval payload: {payload}"))?;
        if status != "Waiting for input." {
            return Err(format!("unexpected status text: {status}").into());
        }
        if payload["value"]["submit_role"] != Value::String("button".to_string()) {
            return Err(format!("expected submit_role=button in smoke payload: {payload}").into());
        }
        let fixture_count = payload["value"]["fixture_count"]
            .as_u64()
            .ok_or_else(|| format!("missing fixture_count in script_eval payload: {payload}"))?;
        if fixture_count == 0 {
            return Err(
                format!("expected at least one fixture in smoke payload: {payload}").into(),
            );
        }

        if verbose {
            println!("{payload}");
        }
        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    if process.kill().await.is_err() {
        // The child may have already exited after the smoke run completes.
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;
    match &smoke_result {
        Ok(()) => println!("[PASS] edev_transport ({elapsed_ms}ms)"),
        Err(error) => println!("[FAIL] edev_transport ({elapsed_ms}ms): {error}"),
    }
    smoke_result
}

/// Return the workspace root used for xtask subprocesses.
fn workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    Ok(env::current_dir()?.canonicalize()?)
}

/// Parse the leading text block of a tool result as JSON.
fn parse_tool_json_text(result: &CallToolResult) -> Result<Value, Box<dyn Error>> {
    if let Some(content) = &result.structured_content {
        return Ok(content.clone());
    }
    let text = result
        .text()
        .ok_or("tool result did not include a text or structured payload")?;
    Ok(serde_json::from_str(text)?)
}

/// Run a command and surface failures.
fn run_command(program: &str, args: &[&str], label: &str) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new(program);
    command.args(args);
    run_prepared_command(command, label)
}

/// Run a prepared command and surface failures.
fn run_prepared_command(command: Command, label: &str) -> Result<(), Box<dyn Error>> {
    run_prepared_command_with_timeout(command, label, None)
}

/// Run a prepared command, optionally terminating it after a timeout.
fn run_prepared_command_with_timeout(
    mut command: Command,
    label: &str,
    timeout: Option<Duration>,
) -> Result<(), Box<dyn Error>> {
    let mut child = command.spawn()?;
    let status = if let Some(timeout) = timeout {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                drop(child.kill());
                drop(child.wait());
                return Err(format!("{label} timed out after {}s", timeout.as_secs()).into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    } else {
        child.wait()?
    };
    if !status.success() {
        return Err(format!("{label} failed with status {status}").into());
    }
    Ok(())
}

/// Ensure the default `eguidev` build stays free of native runtime crates.
fn check_default_eguidev_dependency_surface() -> Result<(), Box<dyn Error>> {
    let output = Command::new("cargo")
        .args(["tree", "-e", "normal", "-p", "eguidev"])
        .output()?;
    if !output.status.success() {
        return Err("cargo tree -e normal -p eguidev failed".into());
    }

    let stdout = String::from_utf8(output.stdout)?;
    let forbidden = [
        "base64",
        "glob",
        "image",
        "luau0-src",
        "mlua",
        "mlua-sys",
        "ruau",
        "ruau-analysis",
        "ruau-ast",
        "ruau-bytecode",
        "ruau-pretty",
        "ruau-source",
        "ruau-stdlib",
        "ruau-typecheck",
        "ruau-vm",
        "ruau-vm-api",
        "tmcp",
        "tokio",
    ];
    let leaks = stdout
        .lines()
        .filter_map(|line| {
            let package = line
                .trim_start_matches([' ', '│', '├', '└', '─'])
                .split_whitespace()
                .next()?;
            forbidden.contains(&package).then_some(package.to_string())
        })
        .collect::<Vec<_>>();

    if leaks.is_empty() {
        return Ok(());
    }

    Err(format!(
        "default eguidev dependency surface leaked runtime crates: {}",
        leaks.join(", ")
    )
    .into())
}
