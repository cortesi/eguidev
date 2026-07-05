#![allow(clippy::missing_docs_in_private_items)]

//! CLI parsing and `.edev.toml` loading for the edev launcher.

use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs,
    iter::once,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, error::ErrorKind};
use eguidev_runtime::{
    ScriptArgValue, ScriptArgs,
    smoke::{SuiteConfig, SuiteRunMode},
};
use serde::Deserialize;
use tokio::process::Command;

use crate::EdevError;

const DEFAULT_CONFIG_FILE: &str = ".edev.toml";
const DEFAULT_SUITE_DIR: &str = "smoketest";
const DEFAULT_BUNDLE_DIR: &str = "tmp/edev-bundles";
const DEFAULT_SUITE_TIMEOUT_SECS: u64 = 600;
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_IDLE_SHUTDOWN_AFTER_SECS: u64 = 20 * 60;

#[derive(Debug, Clone)]
/// Fully resolved app launch configuration.
pub struct LaunchConfig {
    /// Canonical working directory used for app execution and instance locking.
    pub(crate) cwd: PathBuf,
    /// Full argv used to launch the app with DevMCP enabled.
    pub(crate) command: Vec<String>,
    /// Extra environment variables injected into the app process.
    pub(crate) env: BTreeMap<String, String>,
    /// Whether launcher lifecycle logs are enabled.
    pub(crate) verbose: bool,
}

impl LaunchConfig {
    /// Build the app command from the resolved argv and process settings.
    pub(crate) fn app_command(&self) -> Command {
        let mut command = Command::new(&self.command[0]);
        command.args(&self.command[1..]);
        command.current_dir(&self.cwd);
        command.envs(&self.env);
        command
    }
}

#[derive(Debug, Clone)]
/// Resolved configuration for `edev mcp`.
pub struct McpConfig {
    /// Shared app launch settings.
    pub(crate) launch: LaunchConfig,
    /// Idle guard duration for abandoned pre-client launcher sessions.
    pub(crate) idle_shutdown_after: Duration,
}

#[derive(Debug, Clone)]
/// Resolved configuration for `edev smoke`.
pub struct SmokeConfig {
    /// Shared app launch settings. List mode does not need an app command.
    pub(crate) launch: Option<LaunchConfig>,
    /// Suite runner configuration.
    pub(crate) suite: SuiteConfig,
    /// Whether to emit verbose smoke output.
    pub(crate) verbose_output: bool,
    /// Print selected smoke scripts and exit without launching the app.
    pub(crate) list: bool,
    /// Emit list output as JSON.
    pub(crate) list_json: bool,
    /// Optional failure bundle output directory.
    pub(crate) bundle_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
/// Resolved configuration for `edev eval`.
pub struct EvalConfig {
    /// Shared app launch settings.
    pub(crate) launch: LaunchConfig,
    /// Script file to evaluate.
    pub(crate) script: PathBuf,
    /// Directory where returned image refs are written.
    pub(crate) out_dir: PathBuf,
    /// Per-script timeout, defaulting from `[smoke].script_timeout_secs`.
    pub(crate) timeout: Option<Duration>,
    /// Args passed to the script, merged over `[smoke].args`.
    pub(crate) args: ScriptArgs,
}

#[derive(Debug, Clone)]
/// Resolved configuration for `edev dump`.
pub struct DumpConfig {
    /// Shared app launch settings.
    pub(crate) launch: LaunchConfig,
    /// Optional fixture to apply before dumping.
    pub(crate) fixture: Option<String>,
    /// Params passed to the pre-dump fixture.
    pub(crate) params: BTreeMap<String, ScriptArgValue>,
    /// Optional viewport selector to dump.
    pub(crate) viewport: Option<String>,
    /// Whether to wait for a fresh capture before dumping when no fixture is set.
    pub(crate) wait_for_capture: bool,
    /// Emit structured JSON instead of text.
    pub(crate) json: bool,
    /// Optional output file. Stdout is used when unset.
    pub(crate) out: Option<PathBuf>,
    /// Per-script timeout, defaulting from `[smoke].script_timeout_secs`.
    pub(crate) timeout: Option<Duration>,
}

#[derive(Debug, Clone)]
/// Resolved configuration for `edev fixture` / `edev fixtures`.
pub struct FixtureConfig {
    /// Shared app launch settings.
    pub(crate) launch: LaunchConfig,
    /// Fixture name to apply. `None` means list-only mode (`edev fixtures`).
    pub(crate) name: Option<String>,
    /// Params passed to `edev fixture NAME`.
    pub(crate) params: BTreeMap<String, ScriptArgValue>,
    /// Emit fixture list as JSON.
    pub(crate) json: bool,
    /// Emit fixture list as Markdown.
    pub(crate) markdown: bool,
    /// Print `dump_text()` after applying a fixture.
    pub(crate) dump: bool,
    /// Apply without waiting for readiness anchors.
    pub(crate) no_wait: bool,
}

#[derive(Debug, Clone)]
/// Parsed top-level command for the edev binary.
pub enum EdevCommand {
    /// Run the launcher MCP proxy.
    Mcp(McpConfig),
    /// Run the checked-in smoke suite through the launcher.
    Smoke(SmokeConfig),
    /// Evaluate one Luau script through the launcher.
    Eval(EvalConfig),
    /// Dump the app's captured widget tree and exit.
    Dump(DumpConfig),
    /// Start the app and list or apply a fixture.
    Fixture(FixtureConfig),
    /// Render scripting API documentation and exit.
    Docs,
    /// Render help text and exit.
    Help(String),
}

impl EdevCommand {
    /// Parse edev CLI arguments from the process environment.
    pub(crate) fn from_env() -> Result<Self, EdevError> {
        let args = env::args_os().skip(1).collect::<Vec<_>>();
        let current_dir = env::current_dir()?;
        Self::parse_args_in_dir(&args, &current_dir)
    }

    /// Parse edev CLI arguments relative to an explicit current directory.
    fn parse_args_in_dir<S: AsRef<OsStr>>(
        args: &[S],
        current_dir: &Path,
    ) -> Result<Self, EdevError> {
        let (cli_args, app_argv) = split_cli_args_and_app_argv(args)?;
        if cli_args.is_empty() {
            return Ok(Self::Help(render_help()));
        }

        let argv = once(OsString::from("edev"))
            .chain(cli_args)
            .collect::<Vec<_>>();
        let parsed = match Cli::try_parse_from(argv) {
            Ok(parsed) => parsed,
            Err(error) if error.kind() == ErrorKind::DisplayHelp => {
                return Ok(Self::Help(error.to_string()));
            }
            Err(error) => return Err(EdevError::InvalidArgs(error.to_string())),
        };

        let Some(command) = parsed.command else {
            return Ok(Self::Help(render_help()));
        };

        if matches!(command, CliCommand::Docs) && app_argv.is_some() {
            return Err(EdevError::InvalidArgs(
                "`edev docs` does not accept an app command after `--`".to_string(),
            ));
        }
        if matches!(command, CliCommand::Docs) {
            return Ok(Self::Docs);
        }

        let current_dir = current_dir.canonicalize()?;
        let loaded = load_project_config(parsed.config, &current_dir)?;
        match command {
            CliCommand::Docs => unreachable!("docs handled before config resolution"),
            CliCommand::Mcp(cli) => {
                let mut options = McpCliOptions::from(cli);
                options.common.command = app_argv;
                Ok(Self::Mcp(resolve_mcp_config(
                    &options,
                    loaded.as_ref(),
                    &current_dir,
                )?))
            }
            CliCommand::Smoke(cli) => {
                let mut options = SmokeCliOptions::from(cli);
                options.common.command = app_argv;
                Ok(Self::Smoke(resolve_smoke_config(
                    options,
                    loaded.as_ref(),
                    &current_dir,
                )?))
            }
            CliCommand::Eval(cli) => {
                let mut options = EvalCliOptions::from(cli);
                options.common.command = app_argv;
                Ok(Self::Eval(resolve_eval_config(
                    options,
                    loaded.as_ref(),
                    &current_dir,
                )?))
            }
            CliCommand::Dump(cli) => {
                let mut options = DumpCliOptions::from(cli);
                options.common.command = app_argv;
                Ok(Self::Dump(resolve_dump_config(
                    options,
                    loaded.as_ref(),
                    &current_dir,
                )?))
            }
            CliCommand::Fixture(cli) => {
                let mut options = FixtureCliOptions::from(cli);
                options.common.command = app_argv;
                Ok(Self::Fixture(resolve_fixture_config(
                    options,
                    loaded.as_ref(),
                    &current_dir,
                )?))
            }
            CliCommand::Fixtures(cli) => {
                let mut options = FixtureCliOptions::from(cli);
                options.common.command = app_argv;
                Ok(Self::Fixture(resolve_fixture_config(
                    options,
                    loaded.as_ref(),
                    &current_dir,
                )?))
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
struct CommonCliOptions {
    cwd: Option<PathBuf>,
    verbose: Option<bool>,
    command: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone)]
struct McpCliOptions {
    common: CommonCliOptions,
    idle_shutdown_after_secs: Option<u64>,
}

#[derive(Debug, Default, Clone)]
struct SmokeCliOptions {
    common: CommonCliOptions,
    suite_dir: Option<PathBuf>,
    scripts: Vec<PathBuf>,
    only: Vec<String>,
    list: bool,
    list_json: bool,
    repeat: Option<u32>,
    until_fail: Option<u32>,
    fail_fast: Option<bool>,
    suite_timeout_secs: Option<u64>,
    script_timeout_secs: Option<u64>,
    args: ScriptArgs,
    bundle: bool,
    bundle_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
struct EvalCliOptions {
    common: CommonCliOptions,
    script: PathBuf,
    out_dir: Option<PathBuf>,
    script_timeout_secs: Option<u64>,
    args: ScriptArgs,
}

#[derive(Debug, Default, Clone)]
struct DumpCliOptions {
    common: CommonCliOptions,
    fixture: Option<String>,
    params: BTreeMap<String, ScriptArgValue>,
    viewport: Option<String>,
    script_timeout_secs: Option<u64>,
    json: bool,
    out: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
struct FixtureCliOptions {
    common: CommonCliOptions,
    name: Option<String>,
    params: BTreeMap<String, ScriptArgValue>,
    json: bool,
    markdown: bool,
    dump: bool,
    no_wait: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "edev",
    about = "eguidev MCP launcher, smoke runner, and fixture tool",
    disable_version_flag = true
)]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Print Luau API definitions and exit.
    Docs,
    /// Run as an MCP proxy server for agent automation.
    Mcp(McpArgs),
    /// Run the smoketest suite and exit.
    Smoke(SmokeArgs),
    /// Run one Luau script and print the structured result.
    Eval(EvalArgs),
    /// Print a canonical widget tree dump and exit.
    Dump(DumpArgs),
    /// Start the app and list registered fixtures, then exit.
    Fixtures(FixturesArgs),
    /// Start the app, apply a fixture, and keep running.
    Fixture(FixtureArgs),
}

#[derive(Debug, Args, Clone)]
struct CommonArgs {
    /// Override the app working directory.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Enable verbose launcher output.
    #[arg(long, short = 'v', action = ArgAction::SetTrue, conflicts_with = "quiet")]
    verbose: bool,
    /// Disable verbose launcher output.
    #[arg(long, action = ArgAction::SetTrue)]
    quiet: bool,
}

#[derive(Debug, Args)]
struct McpArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Override the pre-client MCP idle guard.
    #[arg(long = "idle-shutdown-after-secs")]
    idle_shutdown_after_secs: Option<u64>,
}

#[derive(Debug, Args)]
struct SmokeArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Override the smoke suite directory.
    #[arg(long = "suite")]
    suite_dir: Option<PathBuf>,
    /// Run only these smoke scripts, in the order provided.
    #[arg(value_name = "SCRIPT")]
    scripts: Vec<PathBuf>,
    /// Print discovered smoke scripts without launching the app.
    #[arg(long = "list", action = ArgAction::SetTrue)]
    list: bool,
    /// Emit list output as JSON.
    #[arg(long = "json", action = ArgAction::SetTrue, requires = "list")]
    list_json: bool,
    /// Filter discovered smoke scripts by display-path glob. Repeat to intersect filters.
    #[arg(long = "only", value_name = "GLOB")]
    only: Vec<String>,
    /// Run the selected smoke scripts this many times.
    #[arg(
        long = "repeat",
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
    /// Stop the smoke suite after the first failure.
    #[arg(long = "fail-fast", action = ArgAction::SetTrue, conflicts_with = "no_fail_fast")]
    fail_fast: bool,
    /// Keep running after failures.
    #[arg(long = "no-fail-fast", action = ArgAction::SetTrue)]
    no_fail_fast: bool,
    /// Override the suite wall-clock timeout.
    #[arg(long = "suite-timeout-secs")]
    suite_timeout_secs: Option<u64>,
    /// Override the per-script timeout.
    #[arg(long = "script-timeout-secs")]
    script_timeout_secs: Option<u64>,
    /// Pass a typed suite-wide script arg.
    #[arg(long = "arg", value_parser = parse_script_arg_cli)]
    args: Vec<(String, ScriptArgValue)>,
    /// Write failure bundles to the configured/default bundle directory.
    #[arg(long = "bundle", action = ArgAction::SetTrue)]
    bundle: bool,
    /// Write failure bundles to this directory.
    #[arg(long = "bundle-dir")]
    bundle_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct EvalArgs {
    /// Script file to evaluate.
    script: PathBuf,
    #[command(flatten)]
    common: CommonArgs,
    /// Directory for returned image files. Defaults to the script directory.
    #[arg(long = "out-dir")]
    out_dir: Option<PathBuf>,
    /// Override the per-script timeout.
    #[arg(long = "script-timeout-secs")]
    script_timeout_secs: Option<u64>,
    /// Pass a typed script arg.
    #[arg(long = "arg", value_parser = parse_script_arg_cli)]
    args: Vec<(String, ScriptArgValue)>,
}

#[derive(Debug, Args)]
struct DumpArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Apply this fixture before dumping.
    #[arg(long)]
    fixture: Option<String>,
    /// Pass a typed fixture param as KEY=VALUE.
    #[arg(long = "param", value_parser = parse_fixture_param_cli)]
    params: Vec<(String, ScriptArgValue)>,
    /// Restrict the dump to one viewport selector.
    #[arg(long)]
    viewport: Option<String>,
    /// Override the dump script timeout.
    #[arg(long = "script-timeout-secs")]
    script_timeout_secs: Option<u64>,
    /// Emit structured JSON instead of text.
    #[arg(long, action = ArgAction::SetTrue)]
    json: bool,
    /// Write the dump to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct FixturesArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Emit structured JSON.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "markdown")]
    json: bool,
    /// Emit Markdown.
    #[arg(long, action = ArgAction::SetTrue)]
    markdown: bool,
}

#[derive(Debug, Args)]
struct FixtureArgs {
    /// Fixture name to apply.
    name: String,
    #[command(flatten)]
    common: CommonArgs,
    /// Pass a typed fixture param as KEY=VALUE.
    #[arg(long = "param", value_parser = parse_fixture_param_cli)]
    params: Vec<(String, ScriptArgValue)>,
    /// Print `dump_text()` after applying the fixture.
    #[arg(long, action = ArgAction::SetTrue)]
    dump: bool,
    /// Apply the fixture without waiting for readiness anchors.
    #[arg(long = "no-wait", action = ArgAction::SetTrue)]
    no_wait: bool,
}

impl From<CommonArgs> for CommonCliOptions {
    fn from(args: CommonArgs) -> Self {
        Self {
            cwd: args.cwd,
            verbose: if args.verbose {
                Some(true)
            } else if args.quiet {
                Some(false)
            } else {
                None
            },
            command: None,
        }
    }
}

impl From<McpArgs> for McpCliOptions {
    fn from(args: McpArgs) -> Self {
        Self {
            common: args.common.into(),
            idle_shutdown_after_secs: args.idle_shutdown_after_secs,
        }
    }
}

impl From<SmokeArgs> for SmokeCliOptions {
    fn from(args: SmokeArgs) -> Self {
        let mut script_args = ScriptArgs::default();
        script_args.extend(args.args);
        let fail_fast = if args.fail_fast {
            Some(true)
        } else if args.no_fail_fast {
            Some(false)
        } else {
            None
        };
        Self {
            common: args.common.into(),
            suite_dir: args.suite_dir,
            scripts: args.scripts,
            only: args.only,
            list: args.list,
            list_json: args.list_json,
            repeat: args.repeat,
            until_fail: args.until_fail,
            fail_fast,
            suite_timeout_secs: args.suite_timeout_secs,
            script_timeout_secs: args.script_timeout_secs,
            args: script_args,
            bundle: args.bundle,
            bundle_dir: args.bundle_dir,
        }
    }
}

impl From<EvalArgs> for EvalCliOptions {
    fn from(args: EvalArgs) -> Self {
        let mut script_args = ScriptArgs::default();
        script_args.extend(args.args);
        Self {
            common: args.common.into(),
            script: args.script,
            out_dir: args.out_dir,
            script_timeout_secs: args.script_timeout_secs,
            args: script_args,
        }
    }
}

impl From<DumpArgs> for DumpCliOptions {
    fn from(args: DumpArgs) -> Self {
        Self {
            common: args.common.into(),
            fixture: args.fixture,
            params: args.params.into_iter().collect(),
            viewport: args.viewport,
            script_timeout_secs: args.script_timeout_secs,
            json: args.json,
            out: args.out,
        }
    }
}

impl From<FixturesArgs> for FixtureCliOptions {
    fn from(args: FixturesArgs) -> Self {
        Self {
            common: args.common.into(),
            name: None,
            params: BTreeMap::new(),
            json: args.json,
            markdown: args.markdown,
            dump: false,
            no_wait: false,
        }
    }
}

impl From<FixtureArgs> for FixtureCliOptions {
    fn from(args: FixtureArgs) -> Self {
        Self {
            common: args.common.into(),
            name: Some(args.name),
            params: args.params.into_iter().collect(),
            json: false,
            markdown: false,
            dump: args.dump,
            no_wait: args.no_wait,
        }
    }
}

fn render_help() -> String {
    Cli::command().render_long_help().to_string()
}

fn split_cli_args_and_app_argv<S: AsRef<OsStr>>(
    args: &[S],
) -> Result<(Vec<OsString>, Option<Vec<String>>), EdevError> {
    let Some(split_index) = args.iter().position(|arg| arg.as_ref() == OsStr::new("--")) else {
        return Ok((
            args.iter().map(|arg| arg.as_ref().to_os_string()).collect(),
            None,
        ));
    };
    let cli_args = args[..split_index]
        .iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let app_argv = args[split_index + 1..]
        .iter()
        .map(|arg| {
            arg.as_ref().to_str().map(ToOwned::to_owned).ok_or_else(|| {
                EdevError::InvalidArgs("app command after `--` must be valid UTF-8".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((cli_args, (!app_argv.is_empty()).then_some(app_argv)))
}

fn parse_script_arg_cli(raw: &str) -> Result<(String, ScriptArgValue), String> {
    parse_script_arg_flag(raw).map_err(|error| error.to_string())
}

fn parse_fixture_param_cli(raw: &str) -> Result<(String, ScriptArgValue), String> {
    parse_fixture_param_flag(raw).map_err(|error| error.to_string())
}

#[derive(Debug, Default, Deserialize, Clone)]
struct FileConfig {
    #[serde(default)]
    app: FileAppConfig,
    #[serde(default)]
    smoke: FileSmokeConfig,
    #[serde(default)]
    mcp: FileMcpConfig,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct FileAppConfig {
    cwd: Option<PathBuf>,
    command: Option<Vec<String>>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct FileSmokeConfig {
    suite_dir: Option<PathBuf>,
    #[serde(rename = "filter")]
    legacy_filter: Option<String>,
    suite_timeout_secs: Option<u64>,
    script_timeout_secs: Option<u64>,
    fail_fast: Option<bool>,
    #[serde(rename = "artifact_dir")]
    legacy_artifact_dir: Option<PathBuf>,
    bundle_dir: Option<PathBuf>,
    #[serde(default)]
    args: ScriptArgs,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct FileMcpConfig {
    verbose: Option<bool>,
    idle_shutdown_after_secs: Option<u64>,
}

#[derive(Debug, Clone)]
struct LoadedConfig {
    path: PathBuf,
    file: FileConfig,
}

fn resolve_fixture_config(
    cli: FixtureCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<FixtureConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    Ok(FixtureConfig {
        launch,
        name: cli.name,
        params: cli.params,
        json: cli.json,
        markdown: cli.markdown,
        dump: cli.dump,
        no_wait: cli.no_wait,
    })
}

fn resolve_dump_config(
    cli: DumpCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<DumpConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    let file_smoke = loaded.map(|config| &config.file.smoke);
    if cli.fixture.is_none() && !cli.params.is_empty() {
        return Err(EdevError::InvalidArgs(
            "`edev dump --param` requires `--fixture`".to_string(),
        ));
    }
    let wait_for_capture = cli.fixture.is_none();
    Ok(DumpConfig {
        launch,
        fixture: cli.fixture,
        params: cli.params,
        viewport: cli.viewport,
        wait_for_capture,
        json: cli.json,
        out: cli
            .out
            .as_ref()
            .map(|path| absolutize_path(path, current_dir)),
        timeout: Some(Duration::from_secs(
            cli.script_timeout_secs
                .or_else(|| file_smoke.and_then(|smoke| smoke.script_timeout_secs))
                .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS),
        )),
    })
}

fn resolve_eval_config(
    cli: EvalCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<EvalConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    let file_smoke = loaded.map(|config| &config.file.smoke);
    let script = absolutize_path(&cli.script, current_dir);
    let out_dir = cli
        .out_dir
        .as_ref()
        .map(|path| absolutize_path(path, current_dir))
        .unwrap_or_else(|| {
            script
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| current_dir.to_path_buf())
        });
    let mut args = file_smoke
        .map(|smoke| smoke.args.clone())
        .unwrap_or_default();
    args.extend(cli.args);
    Ok(EvalConfig {
        launch,
        script,
        out_dir,
        timeout: Some(Duration::from_secs(
            cli.script_timeout_secs
                .or_else(|| file_smoke.and_then(|smoke| smoke.script_timeout_secs))
                .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS),
        )),
        args,
    })
}

fn resolve_mcp_config(
    cli: &McpCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<McpConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    let idle_shutdown_after = Duration::from_secs(
        cli.idle_shutdown_after_secs
            .or_else(|| loaded.and_then(|config| config.file.mcp.idle_shutdown_after_secs))
            .unwrap_or(DEFAULT_IDLE_SHUTDOWN_AFTER_SECS),
    );
    Ok(McpConfig {
        launch,
        idle_shutdown_after,
    })
}

fn resolve_smoke_config(
    cli: SmokeCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<SmokeConfig, EdevError> {
    let suite_base_dir = loaded
        .and_then(|config| config.path.parent())
        .unwrap_or(current_dir);
    let file_smoke = loaded.map(|config| &config.file.smoke);
    if let Some(path) = file_smoke.and_then(|smoke| smoke.legacy_artifact_dir.as_ref()) {
        return Err(EdevError::InvalidArgs(format!(
            "smoke.artifact_dir is no longer supported (found {}); use --verbose for inline failure diagnostics",
            path.display()
        )));
    }
    if file_smoke
        .and_then(|smoke| smoke.legacy_filter.as_ref())
        .is_some()
    {
        return Err(EdevError::InvalidArgs(
            "smoke.filter is no longer supported; pass explicit script paths to `edev smoke`"
                .to_string(),
        ));
    }
    if !cli.scripts.is_empty() && !cli.only.is_empty() {
        return Err(EdevError::InvalidArgs(
            "`--only` cannot be combined with explicit smoke script paths".to_string(),
        ));
    }
    let suite_dir = resolve_path(
        cli.suite_dir.as_ref(),
        file_smoke.and_then(|smoke| smoke.suite_dir.as_ref()),
        current_dir,
        suite_base_dir,
        Path::new(DEFAULT_SUITE_DIR),
    )?;
    let mut args = file_smoke
        .map(|smoke| smoke.args.clone())
        .unwrap_or_default();
    args.extend(cli.args);
    let bundle_dir = if cli.bundle || cli.bundle_dir.is_some() {
        Some(resolve_path(
            cli.bundle_dir.as_ref(),
            file_smoke.and_then(|smoke| smoke.bundle_dir.as_ref()),
            current_dir,
            suite_base_dir,
            Path::new(DEFAULT_BUNDLE_DIR),
        )?)
    } else {
        None
    };
    let suite = SuiteConfig {
        suite_dir,
        scripts: cli.scripts,
        only: cli.only,
        suite_timeout: Duration::from_secs(
            cli.suite_timeout_secs
                .or_else(|| file_smoke.and_then(|smoke| smoke.suite_timeout_secs))
                .unwrap_or(DEFAULT_SUITE_TIMEOUT_SECS),
        ),
        script_timeout: Some(Duration::from_secs(
            cli.script_timeout_secs
                .or_else(|| file_smoke.and_then(|smoke| smoke.script_timeout_secs))
                .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS),
        )),
        fail_fast: cli
            .fail_fast
            .or_else(|| file_smoke.and_then(|smoke| smoke.fail_fast))
            .unwrap_or(false),
        run_mode: cli
            .until_fail
            .map(SuiteRunMode::UntilFail)
            .unwrap_or_else(|| SuiteRunMode::Repeat(cli.repeat.unwrap_or(1))),
        args,
    };
    let launch = if cli.list {
        None
    } else {
        Some(resolve_launch_config(&cli.common, loaded, current_dir)?)
    };
    let verbose_output = launch.as_ref().is_some_and(|launch| launch.verbose);
    Ok(SmokeConfig {
        verbose_output,
        list: cli.list,
        list_json: cli.list_json,
        launch,
        suite,
        bundle_dir,
    })
}

fn resolve_launch_config(
    cli: &CommonCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<LaunchConfig, EdevError> {
    let file_base_dir = loaded
        .and_then(|config| config.path.parent())
        .unwrap_or(current_dir);
    let file_app = loaded.map(|config| &config.file.app);
    let file_mcp = loaded.map(|config| &config.file.mcp);
    let cwd = resolve_path(
        cli.cwd.as_ref(),
        file_app.and_then(|app| app.cwd.as_ref()),
        current_dir,
        file_base_dir,
        current_dir,
    )?;
    let command = cli
        .command
        .clone()
        .or_else(|| file_app.and_then(|app| app.command.clone()))
        .ok_or_else(|| {
            EdevError::InvalidArgs(
                "no app command configured; add app.command to .edev.toml or pass one after `--`"
                    .to_string(),
            )
        })?;
    if command.is_empty() {
        return Err(EdevError::InvalidArgs(
            "app command must not be empty".to_string(),
        ));
    }
    Ok(LaunchConfig {
        cwd,
        command,
        env: file_app.map(|app| app.env.clone()).unwrap_or_default(),
        verbose: cli
            .verbose
            .or_else(|| file_mcp.and_then(|mcp| mcp.verbose))
            .unwrap_or(false),
    })
}

fn resolve_path(
    cli: Option<&PathBuf>,
    file: Option<&PathBuf>,
    current_dir: &Path,
    file_base_dir: &Path,
    default: &Path,
) -> Result<PathBuf, EdevError> {
    let path = if let Some(path) = cli {
        absolutize_path(path, current_dir)
    } else if let Some(path) = file {
        absolutize_path(path, file_base_dir)
    } else {
        absolutize_path(default, current_dir)
    };
    Ok(path.canonicalize().unwrap_or(path))
}

fn absolutize_path(path: &Path, base_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn parse_script_arg_flag(raw: &str) -> Result<(String, ScriptArgValue), EdevError> {
    parse_typed_key_value_flag(raw, "--arg")
}

fn parse_fixture_param_flag(raw: &str) -> Result<(String, ScriptArgValue), EdevError> {
    parse_typed_key_value_flag(raw, "--param")
}

fn parse_typed_key_value_flag(
    raw: &str,
    flag_name: &'static str,
) -> Result<(String, ScriptArgValue), EdevError> {
    let Some((key, raw_value)) = raw.split_once('=') else {
        return Err(EdevError::InvalidArgs(format!(
            "{flag_name} requires KEY=VALUE"
        )));
    };
    if key.is_empty() {
        return Err(EdevError::InvalidArgs(format!(
            "{flag_name} requires a non-empty key"
        )));
    }
    Ok((key.to_string(), parse_script_arg_value(raw_value)))
}

fn parse_script_arg_value(raw: &str) -> ScriptArgValue {
    match raw {
        "true" => ScriptArgValue::Bool(true),
        "false" => ScriptArgValue::Bool(false),
        _ => match raw.parse::<i64>() {
            Ok(value) => ScriptArgValue::Int(value),
            Err(_) => match raw.parse::<f64>() {
                Ok(value) => ScriptArgValue::Float(value),
                Err(_) => ScriptArgValue::String(raw.to_string()),
            },
        },
    }
}

fn load_project_config(
    explicit_config: Option<PathBuf>,
    current_dir: &Path,
) -> Result<Option<LoadedConfig>, EdevError> {
    let Some(path) = resolve_config_path(explicit_config, current_dir)? else {
        return Ok(None);
    };
    let payload = fs::read_to_string(&path)?;
    let file = toml::from_str::<FileConfig>(&payload).map_err(|error| {
        EdevError::InvalidArgs(format!("failed to parse {}: {error}", path.display()))
    })?;
    Ok(Some(LoadedConfig { path, file }))
}

fn resolve_config_path(
    explicit_config: Option<PathBuf>,
    current_dir: &Path,
) -> Result<Option<PathBuf>, EdevError> {
    if let Some(path) = explicit_config {
        let path = absolutize_path(&path, current_dir);
        if !path.is_file() {
            return Err(EdevError::InvalidArgs(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        return Ok(Some(path));
    }
    discover_config_path(current_dir)
}

fn discover_config_path(current_dir: &Path) -> Result<Option<PathBuf>, EdevError> {
    let mut dir = current_dir.canonicalize()?;
    loop {
        let candidate = dir.join(DEFAULT_CONFIG_FILE);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
        let stop_here = dir.join(".git").exists();
        let Some(parent) = dir.parent() else {
            return Ok(None);
        };
        if stop_here {
            return Ok(None);
        }
        dir = parent.to_path_buf();
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs};

    use tempfile::TempDir;

    use super::*;

    fn tempdir() -> TempDir {
        fs::create_dir_all("tmp").expect("create tmp");
        tempfile::Builder::new()
            .prefix("edev-config-test-")
            .tempdir_in("tmp")
            .expect("tempdir")
    }

    fn os_args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parse_command_defaults_to_help() {
        let args = Vec::<OsString>::new();
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Help(help) = command else {
            panic!("expected help command");
        };
        assert!(help.contains("Usage:"));
    }

    #[test]
    fn parse_command_accepts_docs_subcommand() {
        let args = os_args(&["docs"]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        assert!(matches!(command, EdevCommand::Docs));
    }

    #[test]
    fn parse_command_rejects_docs_with_extra_args() {
        let args = os_args(&["docs", "mcp"]);
        let current_dir = env::current_dir().unwrap();
        let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
            .expect_err("extra args should fail");
        assert!(matches!(error, EdevError::InvalidArgs(_)));
    }

    #[test]
    fn parse_mcp_command_accepts_explicit_argv() {
        let args = os_args(&["mcp", "--", "cargo", "run", "--", "--dev-mcp"]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Mcp(config) = command else {
            panic!("expected mcp command");
        };
        assert_eq!(config.launch.command[0], "cargo");
        assert_eq!(config.launch.command.last().expect("last arg"), "--dev-mcp");
    }

    #[test]
    fn parse_smoke_command_parses_typed_args() {
        let args = os_args(&[
            "smoke",
            "smoketest/10_basic.luau",
            "tmp/ad_hoc.luau",
            "--arg",
            "name=Sky",
            "--arg",
            "count=4",
            "--arg",
            "enabled=true",
            "--",
            "cargo",
            "run",
        ]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Smoke(config) = command else {
            panic!("expected smoke command");
        };
        assert_eq!(
            config.suite.args.get("name"),
            Some(&ScriptArgValue::String("Sky".to_string()))
        );
        assert_eq!(
            config.suite.args.get("count"),
            Some(&ScriptArgValue::Int(4))
        );
        assert_eq!(
            config.suite.args.get("enabled"),
            Some(&ScriptArgValue::Bool(true))
        );
        assert_eq!(
            config.suite.scripts,
            vec![
                PathBuf::from("smoketest/10_basic.luau"),
                PathBuf::from("tmp/ad_hoc.luau"),
            ]
        );
        assert_eq!(config.bundle_dir, None);
    }

    #[test]
    fn parse_smoke_command_accepts_authoring_flags() {
        let args = os_args(&[
            "smoke", "--list", "--json", "--only", "*layout*", "--only", "nested/*", "--repeat",
            "3", "--", "cargo", "run",
        ]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Smoke(config) = command else {
            panic!("expected smoke command");
        };

        assert!(config.list);
        assert!(config.list_json);
        assert_eq!(
            config.suite.only,
            vec!["*layout*".to_string(), "nested/*".to_string()]
        );
        assert_eq!(config.suite.run_mode, SuiteRunMode::Repeat(3));
    }

    #[test]
    fn parse_smoke_command_accepts_until_fail() {
        let args = os_args(&["smoke", "--until-fail", "5", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Smoke(config) = command else {
            panic!("expected smoke command");
        };

        assert_eq!(config.suite.run_mode, SuiteRunMode::UntilFail(5));
    }

    #[test]
    fn parse_smoke_command_rejects_only_with_explicit_scripts() {
        let args = os_args(&[
            "smoke",
            "--only",
            "*visual*",
            "smoketest/65_visual_sampling.luau",
            "--",
            "cargo",
            "run",
        ]);
        let current_dir = env::current_dir().unwrap();
        let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
            .expect_err("only with explicit script should fail");

        assert!(
            matches!(error, EdevError::InvalidArgs(message) if message.contains("cannot be combined"))
        );
    }

    #[test]
    fn parse_smoke_list_command_does_not_require_app_command() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        fs::create_dir_all(repo_root.join("smoketest")).expect("create suite dir");
        fs::write(
            repo_root.join("smoketest").join("10_bootstrap.luau"),
            "return true",
        )
        .expect("write script");

        let command = EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--list"]), &repo_root)
            .expect("parse command");
        let EdevCommand::Smoke(config) = command else {
            panic!("expected smoke command");
        };

        assert!(config.list);
        assert!(config.launch.is_none());
        assert_eq!(config.suite.suite_dir, repo_root.join("smoketest"));
    }

    #[test]
    fn parse_smoke_command_accepts_bundle_dir() {
        let args = os_args(&[
            "smoke",
            "--bundle-dir",
            "tmp/custom-bundles",
            "--",
            "cargo",
            "run",
        ]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Smoke(config) = command else {
            panic!("expected smoke command");
        };
        assert_eq!(
            config.bundle_dir,
            Some(current_dir.join("tmp/custom-bundles"))
        );
    }

    #[test]
    fn parse_eval_command_parses_script_args_and_output_dir() {
        let args = os_args(&[
            "eval",
            "tmp/probe.luau",
            "--out-dir",
            "tmp/eval-output",
            "--arg",
            "name=Sky",
            "--arg",
            "count=4",
            "--",
            "cargo",
            "run",
        ]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Eval(config) = command else {
            panic!("expected eval command");
        };
        assert_eq!(config.script, current_dir.join("tmp/probe.luau"));
        assert_eq!(config.out_dir, current_dir.join("tmp/eval-output"));
        assert_eq!(
            config.args.get("name"),
            Some(&ScriptArgValue::String("Sky".to_string()))
        );
        assert_eq!(config.args.get("count"), Some(&ScriptArgValue::Int(4)));
        assert_eq!(config.launch.command[0], "cargo");
    }

    #[test]
    fn parse_dump_command_accepts_fixture_viewport_json_out_and_timeout() {
        let args = os_args(&[
            "dump",
            "--fixture",
            "basic.default",
            "--param",
            "offset=180",
            "--param",
            "enabled=true",
            "--viewport",
            "secondary",
            "--script-timeout-secs",
            "9",
            "--json",
            "--out",
            "tmp/tree.json",
            "--",
            "cargo",
            "run",
        ]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Dump(config) = command else {
            panic!("expected dump command");
        };
        assert_eq!(config.fixture.as_deref(), Some("basic.default"));
        assert_eq!(config.params.get("offset"), Some(&ScriptArgValue::Int(180)));
        assert_eq!(
            config.params.get("enabled"),
            Some(&ScriptArgValue::Bool(true))
        );
        assert_eq!(config.viewport.as_deref(), Some("secondary"));
        assert!(!config.wait_for_capture);
        assert!(config.json);
        assert_eq!(config.out, Some(current_dir.join("tmp/tree.json")));
        assert_eq!(config.timeout, Some(Duration::from_secs(9)));
        assert_eq!(config.launch.command[0], "cargo");
    }

    #[test]
    fn parse_dump_without_fixture_waits_for_capture() {
        let args = os_args(&["dump", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Dump(config) = command else {
            panic!("expected dump command");
        };

        assert!(config.wait_for_capture);
    }

    #[test]
    fn parse_dump_rejects_param_without_fixture() {
        let args = os_args(&["dump", "--param", "offset=180", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
            .expect_err("dump param without fixture should fail");
        let EdevError::InvalidArgs(message) = error else {
            panic!("expected invalid args");
        };
        assert!(message.contains("--param"));
        assert!(message.contains("--fixture"));
    }

    #[test]
    fn eval_command_uses_smoke_timeout_and_arg_defaults() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
        fs::write(
            &config_path,
            "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
script_timeout_secs = 7
args = { name = \"File\", count = 4 }
",
        )
        .expect("write config");

        let args = os_args(&["eval", "tmp/probe.luau", "--arg", "name=Cli"]);
        let command = EdevCommand::parse_args_in_dir(&args, &repo_root).expect("parse command");
        let EdevCommand::Eval(config) = command else {
            panic!("expected eval command");
        };

        assert_eq!(config.timeout, Some(Duration::from_secs(7)));
        assert_eq!(
            config.args.get("name"),
            Some(&ScriptArgValue::String("Cli".to_string()))
        );
        assert_eq!(config.args.get("count"), Some(&ScriptArgValue::Int(4)));
    }

    #[test]
    fn discover_config_stops_at_git_root() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        let nested = repo_root.join("nested").join("deeper");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        fs::create_dir_all(&nested).expect("create nested dirs");
        fs::write(dir.path().join(DEFAULT_CONFIG_FILE), "ignored = true\n").expect("write config");

        let discovered = discover_config_path(&nested).expect("discover config");
        assert!(discovered.is_none());
    }

    #[test]
    fn discover_config_finds_nearest_ancestor_inside_git_root() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        let nested = repo_root.join("nested").join("deeper");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        fs::create_dir_all(&nested).expect("create nested dirs");
        let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
        fs::write(&config_path, "[app]\ncommand = [\"cargo\", \"run\"]\n").expect("write config");

        let discovered = discover_config_path(&nested)
            .expect("discover config")
            .expect("config path");
        assert_eq!(discovered, config_path);
    }

    #[test]
    fn file_config_resolves_relative_paths() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        fs::create_dir_all(repo_root.join("app")).expect("create app dir");
        let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
        fs::write(
            &config_path,
            "\
[app]
cwd = \"app\"
command = [\"cargo\", \"run\"]

[smoke]
suite_dir = \"suite\"
",
        )
        .expect("write config");

        let args = os_args(&["smoke"]);
        let command = EdevCommand::parse_args_in_dir(&args, &repo_root).expect("parse");
        let EdevCommand::Smoke(config) = command else {
            panic!("expected smoke command");
        };
        assert_eq!(
            config.launch.as_ref().expect("launch").cwd,
            repo_root.join("app")
        );
        assert_eq!(config.suite.suite_dir, repo_root.join("suite"));
    }

    #[test]
    fn smoke_bundle_uses_configured_or_default_dir_only_when_enabled() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
        fs::write(
            &config_path,
            "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
bundle_dir = \"configured-bundles\"
",
        )
        .expect("write config");

        let disabled =
            EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse");
        let EdevCommand::Smoke(config) = disabled else {
            panic!("expected smoke command");
        };
        assert_eq!(config.bundle_dir, None);

        let configured =
            EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--bundle"]), &repo_root)
                .expect("parse");
        let EdevCommand::Smoke(config) = configured else {
            panic!("expected smoke command");
        };
        assert_eq!(
            config.bundle_dir,
            Some(repo_root.join("configured-bundles"))
        );

        fs::write(
            &config_path,
            "\
[app]
command = [\"cargo\", \"run\"]
",
        )
        .expect("write config");
        let defaulted =
            EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--bundle"]), &repo_root)
                .expect("parse");
        let EdevCommand::Smoke(config) = defaulted else {
            panic!("expected smoke command");
        };
        assert_eq!(config.bundle_dir, Some(repo_root.join(DEFAULT_BUNDLE_DIR)));
    }

    #[test]
    fn file_config_rejects_legacy_artifact_dir() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
        fs::write(
            &config_path,
            "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
artifact_dir = \"tmp/artifacts\"
",
        )
        .expect("write config");

        let args = os_args(&["smoke"]);
        let error = EdevCommand::parse_args_in_dir(&args, &repo_root).expect_err("parse");
        let EdevError::InvalidArgs(message) = error else {
            panic!("expected invalid args");
        };
        assert!(message.contains("smoke.artifact_dir is no longer supported"));
    }

    #[test]
    fn file_config_rejects_legacy_filter() {
        let dir = tempdir();
        let repo_root = dir.path().join("repo");
        fs::create_dir_all(repo_root.join(".git")).expect("create git root");
        let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
        fs::write(
            &config_path,
            "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
filter = \"10_*\"
",
        )
        .expect("write config");

        let args = os_args(&["smoke"]);
        let error = EdevCommand::parse_args_in_dir(&args, &repo_root).expect_err("parse");
        let EdevError::InvalidArgs(message) = error else {
            panic!("expected invalid args");
        };
        assert!(message.contains("smoke.filter is no longer supported"));
    }

    #[test]
    fn parse_fixtures_command_lists_fixtures() {
        let args = os_args(&["fixtures", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Fixture(config) = command else {
            panic!("expected fixture command");
        };
        assert!(config.name.is_none());
    }

    #[test]
    fn parse_fixtures_command_accepts_json_and_markdown() {
        let current_dir = env::current_dir().unwrap();

        let command =
            EdevCommand::parse_args_in_dir(&os_args(&["fixtures", "--json"]), &current_dir)
                .expect("parse command");
        let EdevCommand::Fixture(config) = command else {
            panic!("expected fixture command");
        };
        assert!(config.json);
        assert!(!config.markdown);

        let command =
            EdevCommand::parse_args_in_dir(&os_args(&["fixtures", "--markdown"]), &current_dir)
                .expect("parse command");
        let EdevCommand::Fixture(config) = command else {
            panic!("expected fixture command");
        };
        assert!(config.markdown);
        assert!(!config.json);
    }

    #[test]
    fn parse_fixture_command_accepts_name() {
        let args = os_args(&["fixture", "basic.default", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Fixture(config) = command else {
            panic!("expected fixture command");
        };
        assert_eq!(config.name.as_deref(), Some("basic.default"));
    }

    #[test]
    fn parse_fixture_command_accepts_params_and_dump() {
        let args = os_args(&[
            "fixture",
            "basic.scrolled",
            "--param",
            "offset=180",
            "--param",
            "enabled=true",
            "--dump",
            "--no-wait",
        ]);
        let current_dir = env::current_dir().unwrap();
        let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
        let EdevCommand::Fixture(config) = command else {
            panic!("expected fixture command");
        };
        assert_eq!(config.name.as_deref(), Some("basic.scrolled"));
        assert_eq!(config.params.get("offset"), Some(&ScriptArgValue::Int(180)));
        assert_eq!(
            config.params.get("enabled"),
            Some(&ScriptArgValue::Bool(true))
        );
        assert!(config.dump);
        assert!(config.no_wait);
    }

    #[test]
    fn parse_fixture_command_rejects_missing_name() {
        let args = os_args(&["fixture", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
            .expect_err("fixture without name should fail");
        let EdevError::InvalidArgs(message) = error else {
            panic!("expected invalid args");
        };
        assert!(message.contains("required arguments were not provided"));
    }

    #[test]
    fn parse_fixtures_command_rejects_positional_arg() {
        let args = os_args(&["fixtures", "basic.default", "--", "cargo", "run"]);
        let current_dir = env::current_dir().unwrap();
        let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
            .expect_err("fixtures with name should fail");
        let EdevError::InvalidArgs(message) = error else {
            panic!("expected invalid args");
        };
        assert!(message.contains("unexpected argument"));
    }
}
