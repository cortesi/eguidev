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

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum, error::ErrorKind};
use eguidev::internal::presentation;
use eguidev_runtime::{
    ScriptArgValue, ScriptArgs,
    smoke::{SuiteConfig, SuiteRunMode},
};
use serde::Deserialize;
#[cfg(not(target_os = "macos"))]
use tokio::process::Command;

use crate::EdevError;
#[cfg(not(target_os = "macos"))]
use crate::cargo_env;

mod cli;
mod file;
mod resolve;

use cli::*;
use file::*;
use resolve::*;

const DEFAULT_CONFIG_FILE: &str = ".edev.toml";
const DEFAULT_SUITE_DIR: &str = "smoketest";
const DEFAULT_BUNDLE_DIR: &str = "tmp/edev-bundles";
const DEFAULT_SUITE_TIMEOUT_SECS: u64 = 600;
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_IDLE_SHUTDOWN_AFTER_SECS: u64 = 20 * 60;
const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 30;

#[derive(Debug, Clone)]
/// Fully resolved app launch configuration.
pub struct LaunchConfig {
    /// Canonical working directory used for app execution.
    pub(crate) cwd: PathBuf,
    /// Full argv used to launch the app with DevMCP enabled.
    pub(crate) command: Vec<String>,
    /// Extra environment variables injected into the app process.
    pub(crate) env: BTreeMap<String, String>,
    /// Presentation policy requested from the embedded app MCP server.
    pub(crate) presentation: presentation::Presentation,
    /// Deadline for normal app closure before forced process cleanup.
    pub(crate) shutdown_grace: Duration,
    /// Whether launcher lifecycle logs are enabled.
    pub(crate) verbose: bool,
    /// MCP client request timeout used after the app handshake.
    pub(crate) request_timeout: Duration,
}

impl LaunchConfig {
    /// Build the app command from the resolved argv and process settings.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn app_command(&self) -> Command {
        let mut command = Command::from(cargo_env::app_command(&self.command[0]));
        command.args(&self.command[1..]);
        command.current_dir(&self.cwd);
        command.envs(&self.env);
        command
    }

    /// Size one-shot clients from the command script timeout without dropping
    /// below the lifecycle handshake bound.
    fn with_script_timeout(mut self, script_timeout: Option<Duration>) -> Self {
        self.request_timeout = script_timeout
            .unwrap_or(crate::APP_REQUEST_TIMEOUT)
            .max(crate::APP_REQUEST_TIMEOUT);
        self
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
    /// Optional root directory whose Luau files are available through
    /// `require`.
    pub(crate) module_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
/// Resolved configuration for `edev record`.
pub struct RecordConfig {
    /// Absolute output movie path.
    pub(crate) outfile: PathBuf,
    /// Optional native window title override.
    pub(crate) window_title: Option<String>,
    /// Smoke suite configuration to execute while recording.
    pub(crate) smoke: SmokeConfig,
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
    /// Optional Luau instruction backstop for this evaluation.
    pub(crate) max_instructions: Option<u64>,
    /// Args passed to the script, merged over `[smoke].args`.
    pub(crate) args: ScriptArgs,
    /// Optional root directory whose Luau files are available through
    /// `require`.
    pub(crate) module_dir: Option<PathBuf>,
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
    /// Whether to wait for a fresh capture before dumping when no fixture is
    /// set.
    pub(crate) wait_for_initial_capture: bool,
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
    /// Apply without waiting for ready conditions.
    pub(crate) no_wait: bool,
}

#[derive(Debug, Clone)]
/// Parsed top-level command for the edev binary.
pub enum EdevCommand {
    /// Run the launcher MCP server.
    Mcp(McpConfig),
    /// Run the checked-in smoke suite through the launcher.
    Smoke(SmokeConfig),
    /// Run the checked-in smoke suite while recording a native app window.
    Record(RecordConfig),
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
            CliCommand::Record(cli) => {
                let mut options = RecordCliOptions::from(cli);
                options.smoke.common.command = app_argv;
                Ok(Self::Record(resolve_record_config(
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

#[cfg(test)]
mod tests;
