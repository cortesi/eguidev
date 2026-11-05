//! The command line of edev: its arguments, its options, and its help.

use super::*;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliPresentation {
    Background,
    Foreground,
}

impl From<CliPresentation> for presentation::Presentation {
    fn from(value: CliPresentation) -> Self {
        match value {
            CliPresentation::Background => Self::Background,
            CliPresentation::Foreground => Self::Foreground,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct CommonCliOptions {
    pub(super) cwd: Option<PathBuf>,
    pub(super) presentation: Option<presentation::Presentation>,
    pub(super) verbose: Option<bool>,
    pub(super) command: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct McpCliOptions {
    pub(super) common: CommonCliOptions,
    pub(super) idle_shutdown_after_secs: Option<u64>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct SmokeCliOptions {
    pub(super) common: CommonCliOptions,
    pub(super) suite_dir: Option<PathBuf>,
    pub(super) scripts: Vec<PathBuf>,
    pub(super) only: Vec<String>,
    pub(super) list: bool,
    pub(super) list_json: bool,
    pub(super) repeat: Option<u32>,
    pub(super) until_fail: Option<u32>,
    pub(super) fail_fast: Option<bool>,
    pub(super) suite_timeout_secs: Option<u64>,
    pub(super) script_timeout_secs: Option<u64>,
    pub(super) script_max_instructions: Option<u64>,
    pub(super) args: ScriptArgs,
    pub(super) bundle: bool,
    pub(super) bundle_dir: Option<PathBuf>,
    pub(super) module_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct RecordCliOptions {
    pub(super) outfile: PathBuf,
    pub(super) window_title: Option<String>,
    pub(super) smoke: SmokeCliOptions,
}

#[derive(Debug, Default, Clone)]
pub(super) struct EvalCliOptions {
    pub(super) common: CommonCliOptions,
    pub(super) script: PathBuf,
    pub(super) out_dir: Option<PathBuf>,
    pub(super) script_timeout_secs: Option<u64>,
    pub(super) script_max_instructions: Option<u64>,
    pub(super) args: ScriptArgs,
    pub(super) module_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct DumpCliOptions {
    pub(super) common: CommonCliOptions,
    pub(super) fixture: Option<String>,
    pub(super) params: BTreeMap<String, ScriptArgValue>,
    pub(super) viewport: Option<String>,
    pub(super) script_timeout_secs: Option<u64>,
    pub(super) json: bool,
    pub(super) out: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct FixtureCliOptions {
    pub(super) common: CommonCliOptions,
    pub(super) name: Option<String>,
    pub(super) params: BTreeMap<String, ScriptArgValue>,
    pub(super) json: bool,
    pub(super) markdown: bool,
    pub(super) dump: bool,
    pub(super) no_wait: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "edev",
    about = "eguidev MCP launcher, smoke runner, and fixture tool",
    disable_version_flag = true
)]
pub(super) struct Cli {
    #[arg(long)]
    pub(super) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(super) command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
pub(super) enum CliCommand {
    /// Print Luau API definitions and exit.
    Docs,
    /// Run as an MCP server for agent automation.
    Mcp(McpArgs),
    /// Run the smoketest suite and exit.
    Smoke(SmokeArgs),
    /// Run the smoketest suite while recording the app window.
    Record(RecordArgs),
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
pub(super) struct CommonArgs {
    /// Override the app working directory.
    #[arg(long)]
    pub(super) cwd: Option<PathBuf>,
    /// Choose whether the app remains in the background while connected.
    #[arg(long, value_enum)]
    pub(super) presentation: Option<CliPresentation>,
    /// Enable verbose launcher output.
    #[arg(long, short = 'v', action = ArgAction::SetTrue, conflicts_with = "quiet")]
    pub(super) verbose: bool,
    /// Disable verbose launcher output.
    #[arg(long, action = ArgAction::SetTrue)]
    pub(super) quiet: bool,
}

#[derive(Debug, Args)]
pub(super) struct McpArgs {
    #[command(flatten)]
    pub(super) common: CommonArgs,
    /// Override the pre-client MCP idle guard.
    #[arg(long = "idle-shutdown-after-secs")]
    pub(super) idle_shutdown_after_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub(super) struct SmokeArgs {
    #[command(flatten)]
    pub(super) common: CommonArgs,
    #[command(flatten)]
    pub(super) smoke: SmokeSuiteArgs,
}

#[derive(Debug, Args, Clone)]
pub(super) struct RecordArgs {
    /// Output `.mov` file to write.
    pub(super) outfile: PathBuf,
    #[command(flatten)]
    pub(super) common: CommonArgs,
    #[command(flatten)]
    pub(super) smoke: SmokeSuiteArgs,
    /// Native window title to record instead of the root viewport title.
    #[arg(long = "window-title", value_name = "TITLE")]
    pub(super) window_title: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub(super) struct SmokeSuiteArgs {
    /// Override the smoke suite directory.
    #[arg(long = "suite")]
    pub(super) suite_dir: Option<PathBuf>,
    /// Run only these smoke scripts, in the order provided.
    #[arg(value_name = "SCRIPT")]
    pub(super) scripts: Vec<PathBuf>,
    /// Print discovered smoke scripts without launching the app.
    #[arg(long = "list", action = ArgAction::SetTrue)]
    pub(super) list: bool,
    /// Emit list output as JSON.
    #[arg(long = "json", action = ArgAction::SetTrue, requires = "list")]
    pub(super) list_json: bool,
    /// Filter discovered smoke scripts by display-path glob. Repeat to select
    /// more scripts (union).
    #[arg(long = "only", value_name = "GLOB")]
    pub(super) only: Vec<String>,
    /// Run the selected smoke scripts this many times.
    #[arg(
        long = "repeat",
        value_parser = clap::value_parser!(u32).range(1..),
        conflicts_with = "until_fail"
    )]
    pub(super) repeat: Option<u32>,
    /// Repeat until the first failure, stopping after at most this many rounds.
    #[arg(
        long = "until-fail",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub(super) until_fail: Option<u32>,
    /// Stop the smoke suite after the first failure.
    #[arg(long = "fail-fast", action = ArgAction::SetTrue, conflicts_with = "no_fail_fast")]
    pub(super) fail_fast: bool,
    /// Keep running after failures.
    #[arg(long = "no-fail-fast", action = ArgAction::SetTrue)]
    pub(super) no_fail_fast: bool,
    /// Override the suite wall-clock timeout.
    #[arg(long = "suite-timeout-secs")]
    pub(super) suite_timeout_secs: Option<u64>,
    /// Override the per-script timeout.
    #[arg(long = "script-timeout-secs")]
    pub(super) script_timeout_secs: Option<u64>,
    /// Override the per-script Luau instruction backstop.
    #[arg(long = "script-max-instructions")]
    pub(super) script_max_instructions: Option<u64>,
    /// Pass a typed suite-wide script arg.
    #[arg(long = "arg", value_parser = parse_script_arg_cli)]
    pub(super) args: Vec<(String, ScriptArgValue)>,
    /// Write failure bundles to the configured/default bundle directory.
    #[arg(long = "bundle", action = ArgAction::SetTrue)]
    pub(super) bundle: bool,
    /// Write failure bundles to this directory.
    #[arg(long = "bundle-dir")]
    pub(super) bundle_dir: Option<PathBuf>,
    /// Make every `.luau` file below this directory available through
    /// `require`.
    #[arg(long = "module-dir")]
    pub(super) module_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct EvalArgs {
    /// Script file to evaluate.
    pub(super) script: PathBuf,
    #[command(flatten)]
    pub(super) common: CommonArgs,
    /// Directory for returned image files. Defaults to the script directory.
    #[arg(long = "out-dir")]
    pub(super) out_dir: Option<PathBuf>,
    /// Override the per-script timeout.
    #[arg(long = "script-timeout-secs")]
    pub(super) script_timeout_secs: Option<u64>,
    /// Override the Luau instruction backstop.
    #[arg(long = "script-max-instructions")]
    pub(super) script_max_instructions: Option<u64>,
    /// Pass a typed script arg.
    #[arg(long = "arg", value_parser = parse_script_arg_cli)]
    pub(super) args: Vec<(String, ScriptArgValue)>,
    /// Make every `.luau` file below this directory available through
    /// `require`.
    #[arg(long = "module-dir")]
    pub(super) module_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct DumpArgs {
    #[command(flatten)]
    pub(super) common: CommonArgs,
    /// Apply this fixture before dumping.
    #[arg(long)]
    pub(super) fixture: Option<String>,
    /// Pass a typed fixture param as KEY=VALUE.
    #[arg(long = "param", value_parser = parse_fixture_param_cli)]
    pub(super) params: Vec<(String, ScriptArgValue)>,
    /// Restrict the dump to one viewport selector.
    #[arg(long)]
    pub(super) viewport: Option<String>,
    /// Override the dump script timeout.
    #[arg(long = "script-timeout-secs")]
    pub(super) script_timeout_secs: Option<u64>,
    /// Emit structured JSON instead of text.
    #[arg(long, action = ArgAction::SetTrue)]
    pub(super) json: bool,
    /// Write the dump to a file instead of stdout.
    #[arg(long)]
    pub(super) out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct FixturesArgs {
    #[command(flatten)]
    pub(super) common: CommonArgs,
    /// Emit structured JSON.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "markdown")]
    pub(super) json: bool,
    /// Emit Markdown.
    #[arg(long, action = ArgAction::SetTrue)]
    pub(super) markdown: bool,
}

#[derive(Debug, Args)]
pub(super) struct FixtureArgs {
    /// Fixture name to apply.
    pub(super) name: String,
    #[command(flatten)]
    pub(super) common: CommonArgs,
    /// Pass a typed fixture param as KEY=VALUE.
    #[arg(long = "param", value_parser = parse_fixture_param_cli)]
    pub(super) params: Vec<(String, ScriptArgValue)>,
    /// Print `dump_text()` after applying the fixture.
    #[arg(long, action = ArgAction::SetTrue)]
    pub(super) dump: bool,
    /// Apply the fixture without waiting for ready conditions.
    #[arg(long = "no-wait", action = ArgAction::SetTrue)]
    pub(super) no_wait: bool,
}

impl From<CommonArgs> for CommonCliOptions {
    fn from(args: CommonArgs) -> Self {
        Self {
            cwd: args.cwd,
            presentation: args.presentation.map(Into::into),
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
        smoke_cli_options(args.common, args.smoke)
    }
}

impl From<RecordArgs> for RecordCliOptions {
    fn from(args: RecordArgs) -> Self {
        Self {
            outfile: args.outfile,
            window_title: args.window_title,
            smoke: smoke_cli_options(args.common, args.smoke),
        }
    }
}

pub(super) fn smoke_cli_options(common: CommonArgs, args: SmokeSuiteArgs) -> SmokeCliOptions {
    let mut script_args = ScriptArgs::default();
    script_args.extend(args.args);
    let fail_fast = if args.fail_fast {
        Some(true)
    } else if args.no_fail_fast {
        Some(false)
    } else {
        None
    };
    SmokeCliOptions {
        common: common.into(),
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
        script_max_instructions: args.script_max_instructions,
        args: script_args,
        bundle: args.bundle,
        bundle_dir: args.bundle_dir,
        module_dir: args.module_dir,
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
            script_max_instructions: args.script_max_instructions,
            args: script_args,
            module_dir: args.module_dir,
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

pub(super) fn render_help() -> String {
    Cli::command().render_long_help().to_string()
}

pub(super) fn split_cli_args_and_app_argv<S: AsRef<OsStr>>(
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

pub(super) fn parse_script_arg_cli(raw: &str) -> Result<(String, ScriptArgValue), String> {
    parse_script_arg_flag(raw).map_err(|error| error.to_string())
}

pub(super) fn parse_fixture_param_cli(raw: &str) -> Result<(String, ScriptArgValue), String> {
    parse_fixture_param_flag(raw).map_err(|error| error.to_string())
}
