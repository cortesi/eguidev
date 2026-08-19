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
use eguidev_runtime::{check_script_source, script_definitions};
use serde_json::{Value, json};
use tmcp::{
    Client,
    schema::{CallToolResult, ToolResultMode},
};
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
    /// Install this repository's SKILL.md for local coding agents.
    #[command(name = "install-skill")]
    InstallSkill,
    /// Run the direct smoketest suite.
    Smoke(SmokeArgs),
    /// Run the direct smoketest suite with the root viewport occluded.
    #[command(name = "smoke-occlusion")]
    SmokeOcclusion(SmokeArgs),
    /// Run the minimal edev transport smoke.
    #[command(name = "smoke-edev", visible_alias = "smoke-edit")]
    SmokeEdev(SmokeEdevArgs),
}

#[derive(ClapArgs, Debug, Clone)]
/// Output controls for the smoke task.
struct SmokeArgs {
    /// Enable verbose smoke logging.
    #[arg(short, long)]
    verbose: bool,
    /// Print discovered smoke scripts without launching the app.
    #[arg(long)]
    list: bool,
    /// Emit list output as JSON.
    #[arg(long, requires = "list")]
    json: bool,
    /// Filter discovered smoke scripts by display-path glob. Repeat to select more scripts (union).
    #[arg(long = "only", value_name = "GLOB")]
    only: Vec<String>,
    /// Run the selected smoke scripts this many times.
    #[arg(
        long,
        value_parser = clap::value_parser!(u32).range(1..),
        conflicts_with = "until_fail"
    )]
    repeat: Option<u32>,
    /// Repeat until the first failure, stopping after at most this many rounds.
    #[arg(
        long = "until-fail",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    until_fail: Option<u32>,
    /// Run only these smoke scripts, in the order provided.
    #[arg(value_name = "SCRIPT")]
    scripts: Vec<PathBuf>,
    /// Pass a typed suite-wide script arg.
    #[arg(long = "arg", value_name = "KEY=VALUE")]
    script_args: Vec<String>,
    /// Write failure bundles to the configured/default bundle directory.
    #[arg(long)]
    bundle: bool,
    /// Write failure bundles to this directory.
    #[arg(long = "bundle-dir")]
    bundle_dir: Option<PathBuf>,
    /// Stop the suite after the first smoketest failure.
    #[arg(long)]
    fail_fast: bool,
}

#[derive(ClapArgs, Debug, Clone)]
/// Output controls for the edev transport smoke.
struct SmokeEdevArgs {
    /// Enable verbose smoke logging.
    #[arg(short, long)]
    verbose: bool,
}

/// Entry point for the workspace xtask runner.
fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    match args.command {
        Task::Tidy => tidy(),
        Task::Test => test(),
        Task::InstallSkill => install_skill(),
        Task::Smoke(args) => smoke(&args),
        Task::SmokeOcclusion(args) => smoke_occlusion(&args),
        Task::SmokeEdev(args) => smoke_edev(&args),
    }
}

/// Run formatter and clippy with workspace defaults, then sync doc snippets.
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
            "--locked",
            "-q",
            "--fix",
            "--all",
            "--all-targets",
            "--all-features",
            "--allow-dirty",
            "--tests",
            "--examples",
        ],
        "cargo clippy --locked",
    )?;
    sync_doc_snippets()?;
    Ok(())
}

/// Rewrite Markdown snippets that no longer match their source files.
fn sync_doc_snippets() -> Result<(), Box<dyn Error>> {
    run_command("snips", &["--commands", "deny"], "snips").map_err(|error| {
        format!("{error}\ninstall the snippet tool with `cargo install snips`").into()
    })
}

/// Run the test suite via nextest.
fn test() -> Result<(), Box<dyn Error>> {
    run_command(
        "cargo",
        &["nextest", "run", "--locked", "--all"],
        "cargo nextest --locked",
    )?;
    run_command(
        "cargo",
        &["test", "--locked", "-q", "-p", "eguidev_runtime", "--tests"],
        "cargo test --locked -p eguidev_runtime --tests",
    )?;
    run_command(
        "cargo",
        &["test", "--locked", "-q", "-p", "eguidev_demo", "--tests"],
        "cargo test --locked -p eguidev_demo --tests",
    )?;
    run_command(
        "cargo",
        &[
            "check",
            "--locked",
            "-q",
            "-p",
            "eguidev",
            "--target",
            "wasm32-unknown-unknown",
        ],
        "cargo check --locked -p eguidev --target wasm32-unknown-unknown",
    )?;
    check_luau_scripts()?;
    check_default_eguidev_dependency_surface()?;
    Ok(())
}

/// Install the repository skill into local agent skill directories.
fn install_skill() -> Result<(), Box<dyn Error>> {
    let source = workspace_root()?.join("skills").join("SKILL.md");
    if !source.is_file() {
        return Err(format!("skill source does not exist: {}", source.display()).into());
    }

    let home = home_dir()?;
    for target_root in skill_install_roots(&home) {
        let target_dir = target_root.join("eguidev");
        fs::create_dir_all(&target_dir)?;
        let target = target_dir.join("SKILL.md");
        let byte_count = fs::copy(&source, &target)?;
        println!("installed {} ({} bytes)", target.display(), byte_count);
    }

    Ok(())
}

/// Return the user's home directory for local skill installs.
fn home_dir() -> Result<PathBuf, Box<dyn Error>> {
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    if home.as_os_str().is_empty() {
        return Err("HOME is empty".into());
    }
    Ok(home)
}

/// Return the local skill roots used by the major coding agents.
fn skill_install_roots(home: &Path) -> [PathBuf; 2] {
    [
        home.join(".agents").join("skills"),
        home.join(".claude").join("skills"),
    ]
}

/// Run the direct Luau smoketest suite against the demo app.
fn smoke(args: &SmokeArgs) -> Result<(), Box<dyn Error>> {
    smoke_with_app_command(args, None)
}

/// Run the direct Luau smoketest suite with an optional app command override.
fn smoke_with_app_command(
    args: &SmokeArgs,
    app_command: Option<&[&str]>,
) -> Result<(), Box<dyn Error>> {
    let workspace_root = workspace_root()?;
    let mut demo_command = Command::new("cargo");
    demo_command.current_dir(&workspace_root);
    demo_command.args([
        "run", "--locked", "-q", "-p", "edev", "--bin", "edev", "--", "smoke",
    ]);
    if args.list {
        demo_command.arg("--list");
    }
    if args.json {
        demo_command.arg("--json");
    }
    for only in &args.only {
        demo_command.args(["--only", only]);
    }
    if let Some(repeat) = args.repeat {
        demo_command.arg("--repeat").arg(repeat.to_string());
    }
    if let Some(until_fail) = args.until_fail {
        demo_command.arg("--until-fail").arg(until_fail.to_string());
    }
    if args.fail_fast {
        demo_command.arg("--fail-fast");
    }
    if args.verbose {
        demo_command.arg("--verbose");
    }
    if args.bundle {
        demo_command.arg("--bundle");
    }
    if let Some(bundle_dir) = &args.bundle_dir {
        demo_command.arg("--bundle-dir").arg(bundle_dir);
    }
    for script_arg in &args.script_args {
        demo_command.args(["--arg", script_arg]);
    }
    demo_command.args(&args.scripts);
    if let Some(app_command) = app_command {
        demo_command.arg("--");
        demo_command.args(app_command);
    }
    run_prepared_command_with_timeout(
        demo_command,
        "cargo run --locked -p edev --bin edev -- smoke",
        Some(Duration::from_secs(15 * 60)),
    )
}

/// Run the full smoke suite with the root viewport covered by the test occluder.
fn smoke_occlusion(args: &SmokeArgs) -> Result<(), Box<dyn Error>> {
    let mut occlusion_args = args.clone();
    occlusion_args
        .script_args
        .push("force_occluder=true".to_string());
    smoke_with_app_command(&occlusion_args, Some(&occlusion_demo_command()))
}

/// Command used by occlusion smoke to launch the demo with a persistent cover viewport.
fn occlusion_demo_command() -> [&'static str; 10] {
    [
        "cargo",
        "run",
        "--quiet",
        "--locked",
        "-p",
        "eguidev_demo",
        "--bin",
        "eguidev_demo",
        "--",
        "--force-occluder",
    ]
}

/// Run the edev transport smoke against the demo app.
fn smoke_edev(args: &SmokeEdevArgs) -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(smoke_edev_transport(args.verbose))
}

/// Type-check every checked-in Luau script against the runtime declaration.
fn check_luau_scripts() -> Result<(), Box<dyn Error>> {
    for source_path in luau_sources()? {
        let source = fs::read_to_string(&source_path)?;
        check_luau_source(&source_path, &source)?;
    }

    Ok(())
}

/// Check one Luau source against the runtime's exact declaration environment.
fn check_luau_source(path: &Path, source: &str) -> Result<(), Box<dyn Error>> {
    let source_name = path.to_string_lossy().replace('\\', "/");
    check_script_source(&source_name, source)
        .map_err(|error| format!("Luau check failed for {}:\n{error}", path.display()).into())
}

/// Enumerate checked-in example scripts that should type-check against the API definitions.
fn luau_sources() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let root = workspace_root()?;
    let mut sources = Vec::new();
    collect_luau_files(&root.join("docs/examples"), &mut sources)?;
    collect_luau_files(&root.join("smoketest"), &mut sources)?;
    collect_luau_files(&root.join("crates/edev/luau"), &mut sources)?;
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
    command.args([
        "run", "--locked", "-q", "-p", "edev", "--bin", "edev", "--", "mcp",
    ]);
    if verbose {
        command.arg("--verbose");
    }

    let tmcp::SpawnedServer { mut process, .. } = client.connect_process(command).await?;
    let start = Instant::now();
    let smoke_result = async {
        let tools = client.list_tools(None).await?;
        for expected in ["start", "stop", "restart", "status"] {
            if !tools.tools.iter().any(|tool| tool.name == expected) {
                return Err(format!("missing expected tool: {expected}").into());
            }
        }

        if tools
            .tools
            .iter()
            .any(|tool| tool.name == "script_eval" || tool.name == "script_api")
        {
            return Err("launcher exposed app-owned script tools".into());
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
        let endpoint = start_payload["report"]["connection"]["endpoint"]
            .as_str()
            .ok_or_else(|| format!("start did not return an app endpoint: {start_payload}"))?;
        let mut app_client = Client::new("xtask-smoke-app", env!("CARGO_PKG_VERSION"))
            .with_request_timeout(Duration::from_secs(120));
        app_client.connect_tcp(endpoint.to_string()).await?;
        let app_tools = app_client.list_tools(None).await?;
        for expected in ["script_eval", "script_api"] {
            if !app_tools.tools.iter().any(|tool| tool.name == expected) {
                return Err(format!("missing app-owned tool: {expected}").into());
            }
        }

        let script_api_result = app_client.call_tool("script_api", json!({})).await?;
        let script_api = script_api_result
            .text()
            .ok_or("script_api response did not include text content")?;
        if script_api != script_definitions() {
            return Err("script_api payload did not match checked-in definitions".into());
        }

        let result = app_client
            .call_tool(
                "script_eval",
                json!({
                    "script": r#"
local available = eguidev.fixtures()
local has_default = false
for _, spec in ipairs(available) do
    if spec.name == "basic.default" then
        has_default = true
        break
    end
end
assert(has_default, "basic.default fixture should be registered")
eguidev.fixture("basic.default")
local submit = eguidev.widget("basic.submit")
local submit_state = submit:state()
assert(submit_state ~= nil)
assert(submit_state.role == "button", "submit should expose button role")
local status_state = eguidev.widget("basic.status"):state()
assert(status_state ~= nil)
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
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()?)
}

/// Parse the leading text block of a tool result as JSON.
fn parse_tool_json_text(result: &CallToolResult) -> Result<Value, Box<dyn Error>> {
    Ok(result.extract_as(ToolResultMode::StructuredOrFirstJsonText)?)
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
        .args(["tree", "--locked", "-e", "normal", "-p", "eguidev"])
        .output()?;
    if !output.status.success() {
        return Err("cargo tree --locked -e normal -p eguidev failed".into());
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn workspace_root_finds_checked_in_sources() {
        let root = workspace_root().expect("workspace root");
        assert!(root.join("smoketest").is_dir());
        assert!(root.join("skills").join("SKILL.md").is_file());
        let sources = luau_sources().expect("luau sources");
        assert!(
            sources
                .iter()
                .any(|path| path.ends_with("10_basic_form.luau")),
            "{sources:?}"
        );
    }

    #[test]
    fn smoke_edev_rejects_unknown_list_flag() {
        let error = match Args::try_parse_from(["xtask", "smoke-edev", "--list"]) {
            Ok(_) => panic!("smoke-edev should reject --list"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unexpected argument"), "{error}");
    }

    #[test]
    fn occlusion_demo_command_passes_locked() {
        let command = occlusion_demo_command();
        assert!(command.contains(&"--locked"));
        assert!(command.contains(&"--quiet"));
        assert!(command.contains(&"--force-occluder"));
    }

    #[test]
    fn smoke_only_help_says_union() {
        let help = Args::command()
            .find_subcommand("smoke")
            .expect("smoke")
            .get_arguments()
            .find(|argument| argument.get_long() == Some("only"))
            .expect("only")
            .get_help()
            .expect("help")
            .to_string();
        assert!(help.contains("union"), "{help}");
        assert!(!help.contains("intersect"), "{help}");
    }
}
