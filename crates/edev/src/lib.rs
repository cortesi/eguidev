//! Script-first MCP launcher for eguidev.

mod cargo_env;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fmt::Display,
    fs::{self, DirEntry},
    future::{Future, pending},
    io as std_io,
    path::{MAIN_SEPARATOR, Path, PathBuf},
    pin::Pin,
    process::{self, ExitStatus},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
#[cfg(target_os = "macos")]
use eguidev::internal::presentation::PresentationStatus;
use eguidev::{
    FixtureParam, FixtureSpec, ParamKind, WidgetValue,
    internal::presentation::{EXPERIMENTAL_PRESENTATION_CAPABILITY, Presentation},
};
use eguidev_runtime::{
    ScriptArgValue, ScriptArgs, ScriptErrorInfo, ScriptEvalOptions, ScriptEvalOutcome,
    ScriptEvalRequest, ScriptModules, script_definitions,
    smoke::{ScriptRunRequest, SuiteResult, discover_suite_scripts, run_suite_with},
};
use instance_registry::{
    AppLaunch, AppRecord, InstanceRegistry, read_app_record_for_path, remove_app_record_if_matches,
};
use serde::{
    Deserialize, Serialize,
    de::{DeserializeOwned, Error as SerdeDeError},
};
use tmcp::{
    Arguments, Error as McpError, Server, ServerCtx, ServerHandler,
    schema::{
        CallToolResponse, CallToolResult, ClientCapabilities, ContentBlock, Cursor, ImageContent,
        Implementation, InitializeResult, ListToolsResult, ProtocolVersion, TaskMetadata, Tool,
        ToolResultDecodeError, ToolResultExtractError, ToolResultMode, ToolSchema,
    },
};
use tokio::{
    io::{self as tokio_io, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Child,
    runtime::Handle,
    sync::Mutex as AsyncMutex,
    task::{JoinHandle, block_in_place},
    time::{sleep, timeout},
};

mod command;
mod config;
mod failure_bundle;
mod fixture_projection;
mod instance_registry;
mod lifecycle;
mod mcp_server;
mod modules;
mod process_lifecycle;
mod recording;
mod session;
mod smoke_run;
mod spawn;

pub use command::run;
use command::{call_script_eval_result, decode_tool_result, script_eval_error_message};

#[cfg(all(feature = "test-app", target_os = "macos"))]
#[doc(hidden)]
pub mod process_test_support {
    pub use crate::{
        instance_registry::process_is_alive,
        process_lifecycle::{ProcessGroupObserver, create_inherited_pipe, set_cloexec},
        recording::live_process_group_members,
    };
}
use config::{
    DumpConfig, EdevCommand, EvalConfig, FixtureConfig, LaunchConfig, McpConfig, RecordConfig,
    SmokeConfig,
};
use failure_bundle::{
    BundleContext, FailureBundleScript, image_extension, pretty_json, prune_previous_bundles,
    safe_file_component, write_failure_bundle,
};
use fixture_projection::{FIXTURE_APPLY_SCRIPT, FIXTURE_LIST_SCRIPT, parse_fixture_list};
use lifecycle::*;
use mcp_server::*;
use modules::*;
use session::AppSession;
use smoke_run::*;
use spawn::*;

/// Timeout used for app MCP request/response round-trips.
const APP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum time allowed for a launched app to build and open its direct MCP
/// socket.
const APP_CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum app stdout/stderr bytes retained for diagnostics.
const APP_LOG_TAIL_LIMIT: usize = 4 * 1024 * 1024;
/// Extra log bytes retained before trimming back to the stable tail limit.
const APP_LOG_TAIL_TRIM_SLACK: usize = 256 * 1024;
/// Maximum time allowed for the supervisor to publish its app record. The
/// supervisor writes the record after it spawns the app, so a fast app can
/// accept the MCP handshake before the record lands on disk.
const APP_RECORD_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval used while the launcher waits for the supervisor app record.
const APP_RECORD_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Maximum attempts for restart when the app MCP transport closes
/// mid-handshake.
const RESTART_MAX_ATTEMPTS: usize = 3;
/// Fresh-capture attempts used while waiting for the native window to enter
/// ScreenCaptureKit.
const RECORD_WINDOW_DISCOVERY_ATTEMPTS: usize = 3;
/// Checked-in projection used by the dump command.
const DUMP_SCRIPT: &str = include_str!("../luau/dump.luau");

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
    /// Recording failure.
    #[error("record failed: {0}")]
    RecordFailed(String),
    /// One-shot script evaluation failure.
    #[error("eval failed: {0}")]
    EvalFailed(String),
    /// Fixture operation failure.
    #[error("fixture failed: {0}")]
    FixtureFailed(String),
    /// Instance registry error.
    #[error("instance registry error: {0}")]
    InstanceRegistry(String),
    /// Managed app teardown required forced process cleanup.
    #[error("app shutdown was forced: {0}")]
    ForcedShutdown(String),
    /// The script runtime of a started app did not answer the readiness
    /// probe within the request timeout.
    #[error("app start timed out: {0}")]
    StartTimeout(String),
}

impl EdevError {
    /// Return the process exit code that reports this error, so that a caller
    /// can tell a failed script from a failed run:
    ///
    /// - 1: a smoke suite, a script evaluation, or a fixture failed.
    /// - 2: the arguments or the configuration file are invalid.
    /// - 3: the run failed outside the scripts: the app start, a forced
    ///   shutdown, MCP, I/O, the instance registry, or the recording.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::SmokeFailed(_) | Self::EvalFailed(_) | Self::FixtureFailed(_) => 1,
            Self::InvalidArgs(_) => 2,
            Self::Mcp(_)
            | Self::Io(_)
            | Self::AppStart(_)
            | Self::RecordFailed(_)
            | Self::InstanceRegistry(_)
            | Self::ForcedShutdown(_)
            | Self::StartTimeout(_) => 3,
        }
    }
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
        presentation: Presentation::Background,
        shutdown_grace: Duration::from_secs(30),
        verbose: false,
        request_timeout: APP_REQUEST_TIMEOUT,
    }
}

#[cfg(test)]
mod tests;
