//! Script-first MCP launcher and proxy for eguidev.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Display,
    fs,
    future::{Future, pending},
    io as std_io,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitStatus,
    sync::{Arc, Mutex},
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
    ScriptEvalRequest, script_definitions,
    smoke::{ScriptRunRequest, SuiteResult, discover_suite_scripts, run_suite_with},
};
use instance_registry::{AppLaunch, AppRecord, InstanceRegistry, read_app_record_for_path};
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
#[cfg(test)]
mod observations;
mod process_lifecycle;
mod recording;
mod session;

pub use command::run;
use command::{
    call_script_eval_result, decode_tool_result, script_eval_error_message, start_app_client,
};
use config::{
    DumpConfig, EdevCommand, EvalConfig, FixtureConfig, LaunchConfig, McpConfig, RecordConfig,
    SmokeConfig,
};
use failure_bundle::{
    BundleContext, image_extension, pretty_json, safe_file_component, write_failure_bundle,
};
use fixture_projection::{FIXTURE_APPLY_SCRIPT, FIXTURE_LIST_SCRIPT, parse_fixture_list};
use session::AppSession;

/// Timeout used for app MCP request/response round-trips.
const APP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum time allowed for a launched app to build and open its direct MCP socket.
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
/// Maximum attempts for restart when the app MCP transport closes mid-handshake.
const RESTART_MAX_ATTEMPTS: usize = 3;
/// Fresh-capture attempts used while waiting for the native window to enter ScreenCaptureKit.
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
    /// Most recent completed app shutdown, retained until the next launch.
    last_shutdown: Option<ShutdownResult>,
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
            last_shutdown: None,
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
            let shutdown = app.shutdown().await;
            self.last_shutdown = Some(shutdown.clone());
            self.status = AppStatus::NotRunning;
            return Ok(StopStatus::Stopped(Some(shutdown)));
        }
        if matches!(self.status, AppStatus::StartupFailed { .. }) {
            self.status = AppStatus::NotRunning;
            return Ok(StopStatus::Stopped(None));
        }
        self.status = AppStatus::NotRunning;
        Ok(StopStatus::AlreadyStopped)
    }

    /// Stop the app process and unregister the launcher.
    async fn shutdown(&mut self) -> Result<(), EdevError> {
        let stopped = self.stop_app().await?;
        self.instance_registry.unregister()?;
        stopped.ensure_graceful()
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

    /// Resolve the current direct app client, or return a lifecycle-specific tool error.
    fn app_client(&self) -> Result<Arc<AsyncMutex<tmcp::Client<()>>>, CallToolResult> {
        match &self.status {
            AppStatus::Running => {
                let Some(app) = &self.app else {
                    self.log_edev("app client unavailable: app not running");
                    return Err(tool_error(
                        ErrorKind::AppNotRunning,
                        "App process not running. Call start.",
                    ));
                };
                Ok(Arc::clone(&app.client))
            }
            AppStatus::Starting => {
                self.log_edev("app client unavailable: app starting");
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
                self.log_edev("app client unavailable: app not running");
                Err(tool_error(
                    ErrorKind::AppNotRunning,
                    "App is not running. Call start.",
                ))
            }
        }
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
        if replace_existing && let Some(app) = self.app.take() {
            let shutdown = app.shutdown().await;
            self.last_shutdown = Some(shutdown);
        }
        self.last_shutdown = None;
        self.log_edev(format!("{} requested", action.as_str()));
        match spawn(&self.config, self.log_state.clone()).await {
            Ok(app) => {
                if let Err(output) = probe_script_eval_ready(&app.client).await {
                    let shutdown = app.shutdown().await;
                    self.last_shutdown = Some(shutdown);
                    self.status = AppStatus::StartupFailed {
                        output: output.clone(),
                    };
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
        vec![start_tool(), stop_tool(), restart_tool(), status_tool()]
    }

    /// Build a structured status snapshot of the managed app lifecycle.
    fn status_report(&self) -> StatusReport {
        let (app_present, process_group_id, supervisor_pid, launch_id, registry_entry_path) = self
            .app
            .as_ref()
            .map(|app| {
                (
                    true,
                    app.process_group_id,
                    app.supervisor_pid,
                    app.app_record
                        .as_ref()
                        .map(|record| record.launch_id.clone()),
                    app.app_launch
                        .as_ref()
                        .map(|launch| launch.entry_path.clone()),
                )
            })
            .unwrap_or((false, None, None, None, None));
        let startup_output = match &self.status {
            AppStatus::StartupFailed { output } => Some(output.clone()),
            _ => None,
        };
        StatusReport {
            state: self.status.as_str(),
            app_present,
            process_group_id,
            supervisor_pid,
            launch_id,
            connection: self
                .app
                .as_ref()
                .and_then(AppProcess::connection_descriptor),
            registry_entry_path,
            startup_output,
            mcp_client_attached: self.mcp_client_attached,
            idle_shutdown: self.idle_shutdown_report(),
            last_shutdown: self.last_shutdown.clone(),
            #[cfg(target_os = "macos")]
            presentation: PresentationStatus::requested(self.config.presentation),
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
    /// PID of the macOS supervisor, when present.
    supervisor_pid: Option<u32>,
    /// Task that owns and reaps the supervisor child.
    supervisor_exit_task: Option<JoinHandle<std_io::Result<ExitStatus>>>,
    /// Exact launch identity passed to the supervisor.
    app_launch: Option<AppLaunch>,
    /// Exact metadata written by the supervisor.
    app_record: Option<AppRecord>,
    /// Per-launch ownership writer held only by this outer AppProcess.
    ownership_writer: Option<process_lifecycle::OwnershipWriter>,
    /// Connected MCP client speaking to the app over its direct TCP endpoint.
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    /// Direct loopback MCP endpoint shared with external callers.
    mcp_endpoint: String,
    /// Background task streaming stdout.
    stdout_task: Option<JoinHandle<()>>,
    /// Background task streaming stderr.
    stderr_task: Option<JoinHandle<()>>,
    /// Captured stderr output, primarily for startup errors.
    stderr_buffer: Arc<Mutex<Vec<u8>>>,
    /// Captured stdout output when stdout is not consumed by the MCP transport.
    stdout_buffer: Arc<Mutex<Vec<u8>>>,
    /// Logger for process lifecycle messages.
    log_state: LogState,
    /// Deadline for normal app closure before forced cleanup.
    shutdown_grace: Duration,
}

impl AppProcess {
    /// Trigger immediate app termination without waiting for child process exit.
    fn start_termination(&mut self) {
        let process_group_id = self.process_group_id.take();
        process_lifecycle::terminate_process_group(process_group_id, &self.log_state);
        self.ownership_writer.take();
        let supervisor_pid = self.supervisor_pid.take();
        process_lifecycle::terminate_supervisor(supervisor_pid, &self.log_state);
        if let Some(child) = self.child.as_mut() {
            let _start_kill_result = child.start_kill();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        if let Some(task) = self.stdout_task.take() {
            task.abort();
        }
    }

    /// Request normal app closure and escalate only when the request or exit fails.
    async fn shutdown(mut self) -> ShutdownResult {
        let close_result = request_app_close(&self.client).await;
        let shutdown_grace = self.shutdown_grace;
        let result = resolve_shutdown(close_result, self.wait_for_exit(), shutdown_grace).await;
        if result.is_forced() {
            self.start_termination();
            let _wait_result = self.wait_for_exit().await;
        } else {
            self.process_group_id.take();
            self.supervisor_pid.take();
            self.ownership_writer.take();
        }
        if let Some(task) = self.stderr_task.take() {
            let _wait_result = task.await;
        }
        if let Some(task) = self.stdout_task.take() {
            let _wait_result = task.await;
        }
        let _drain_result = drain_stderr(&self.stderr_buffer).await;
        result
    }

    /// Await the existing supervisor or direct-child exit event.
    async fn wait_for_exit(&mut self) -> Result<(), String> {
        if let Some(task) = self.supervisor_exit_task.as_mut() {
            let status = task
                .await
                .map_err(|error| format!("supervisor exit task failed: {error}"))?
                .map_err(|error| format!("supervisor exit failed: {error}"))?;
            self.supervisor_exit_task.take();
            if status.success() {
                return Ok(());
            }
            return Err(format!("supervisor exited with {status}"));
        }
        if let Some(child) = self.child.as_mut() {
            let status = child
                .wait()
                .await
                .map_err(|error| format!("app exit wait failed: {error}"))?;
            self.child.take();
            if status.success() {
                return Ok(());
            }
            return Err(format!("app exited with {status}"));
        }
        Ok(())
    }

    /// Describe the exact app endpoint that direct MCP clients can connect to.
    fn connection_descriptor(&self) -> Option<AppConnectionDescriptor> {
        let launch_id = self
            .app_record
            .as_ref()
            .map(|record| record.launch_id.clone())
            .or_else(|| {
                self.app_launch
                    .as_ref()
                    .map(|launch| launch.launch_id.clone())
            })?;
        Some(AppConnectionDescriptor {
            launch_id,
            transport: "tcp",
            endpoint: self.mcp_endpoint.clone(),
        })
    }
}

/// Ask the connected app to queue normal root-viewport closure.
async fn request_app_close(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
) -> Result<(), ShutdownCause> {
    let result = {
        let client = client.lock().await;
        client
            .call_tool("app_close".to_string(), serde_json::json!({}))
            .await
            .map_err(|error| ShutdownCause::AppMcpUnavailable(error.to_string()))?
    };
    if result.is_error() {
        return Err(ShutdownCause::AppCloseFailed(
            result
                .text()
                .unwrap_or("app_close returned an error")
                .to_string(),
        ));
    }
    Ok(())
}

/// Classify one close request and event-driven exit observation.
async fn resolve_shutdown<F>(
    close_result: Result<(), ShutdownCause>,
    exit: F,
    grace: Duration,
) -> ShutdownResult
where
    F: Future<Output = Result<(), String>>,
{
    if let Err(cause) = close_result {
        return ShutdownResult::Forced { cause };
    }
    match timeout(grace, exit).await {
        Ok(Ok(())) => ShutdownResult::Graceful,
        Ok(Err(error)) => ShutdownResult::Forced {
            cause: ShutdownCause::ExitFailed(error),
        },
        Err(_) => ShutdownResult::Forced {
            cause: ShutdownCause::DeadlineExpired,
        },
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
    Stopped(Option<ShutdownResult>),
    /// No app was running.
    AlreadyStopped,
}

impl StopStatus {
    /// Convert forced cleanup into the failure contract for one-shot commands.
    fn ensure_graceful(self) -> Result<(), EdevError> {
        match self {
            Self::Stopped(Some(result)) => result.ensure_graceful(),
            Self::Stopped(None) | Self::AlreadyStopped => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
/// Result of one managed app shutdown.
enum ShutdownResult {
    /// The app accepted the close request and exited before the deadline.
    Graceful,
    /// Edev used forced process cleanup.
    Forced {
        /// Cause that required escalation.
        cause: ShutdownCause,
    },
}

impl ShutdownResult {
    /// Return whether process cleanup required escalation.
    fn is_forced(&self) -> bool {
        matches!(self, Self::Forced { .. })
    }

    /// Convert this result into the one-shot command contract.
    fn ensure_graceful(self) -> Result<(), EdevError> {
        match self {
            Self::Graceful => Ok(()),
            Self::Forced { cause } => Err(EdevError::ForcedShutdown(cause.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
/// Cause of forced managed app cleanup.
enum ShutdownCause {
    /// The app MCP transport could not accept the close request.
    #[error("app MCP unavailable: {0}")]
    AppMcpUnavailable(String),
    /// The app rejected the close request.
    #[error("app close request failed: {0}")]
    AppCloseFailed(String),
    /// The app did not exit before the configured deadline.
    #[error("app did not exit before the shutdown deadline")]
    DeadlineExpired,
    /// The supervisor or direct child exit event failed.
    #[error("app exit observation failed: {0}")]
    ExitFailed(String),
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

/// Read the supervisor's app record, tolerating the write that follows the app
/// spawn. Returns `Ok(None)` only after the record stays absent for
/// [`APP_RECORD_TIMEOUT`].
async fn await_app_record(path: &Path) -> Result<Option<AppRecord>, std_io::Error> {
    let deadline = Instant::now() + APP_RECORD_TIMEOUT;
    loop {
        if let Some(record) = read_app_record_for_path(path)? {
            return Ok(Some(record));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(APP_RECORD_POLL_INTERVAL).await;
    }
}

/// Encode the launcher's private presentation intent for the app handshake.
fn client_capabilities(presentation: Presentation) -> ClientCapabilities {
    ClientCapabilities::default().with_experimental_capability(
        EXPERIMENTAL_PRESENTATION_CAPABILITY,
        serde_json::Value::String(presentation.as_str().to_string()),
    )
}

/// Spawn the app and connect an MCP client to its direct loopback endpoint.
async fn spawn_app(
    config: &LaunchConfig,
    log_state: LogState,
) -> Result<AppProcess, AppStartError> {
    let mcp_endpoint = allocate_mcp_endpoint().map_err(|error| {
        AppStartError::Other(format!("allocate direct app MCP endpoint: {error}"))
    })?;
    let mut direct_config = config.clone();
    direct_config.env.insert(
        eguidev_runtime::MCP_ADDR_ENV.to_string(),
        mcp_endpoint.clone(),
    );
    let mut process = process_lifecycle::spawn(&direct_config, log_state.clone())
        .await
        .map_err(AppStartError::Other)?;
    let stdout = match process.stdout.take() {
        Some(stdout) => stdout,
        None => {
            process_lifecycle::shutdown_spawned(process, &log_state).await;
            return Err(AppStartError::Other(
                "failed to capture process stdout".to_string(),
            ));
        }
    };
    let stdin = match process.stdin.take() {
        Some(stdin) => stdin,
        None => {
            process_lifecycle::shutdown_spawned(process, &log_state).await;
            return Err(AppStartError::Other(
                "failed to capture process stdin".to_string(),
            ));
        }
    };
    let stderr = match process.stderr.take() {
        Some(stderr) => stderr,
        None => {
            process_lifecycle::shutdown_spawned(process, &log_state).await;
            return Err(AppStartError::Other(
                "failed to capture process stderr".to_string(),
            ));
        }
    };

    let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
    let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
    drop(stdin);
    let stdout_buffer_clone = Arc::clone(&stdout_buffer);
    let stdout_log_state = log_state.clone();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
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
            append_tail_capped(&stdout_buffer_clone, line.as_bytes());
            stdout_log_state.record_line(&line);
        }
    });
    let stderr_buffer_clone = Arc::clone(&stderr_buffer);
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
            let _write_result = tokio_io::stderr().write_all(line.as_bytes()).await;
        }
    });

    let client = match connect_app_client(
        &mcp_endpoint,
        config.presentation,
        config.request_timeout,
        &mut process,
    )
    .await
    {
        Ok(client) => client,
        Err(error) => {
            return Err(fail_startup_handshake(
                process,
                stderr_task,
                stdout_task,
                &stderr_buffer,
                "connect",
                &error,
                &log_state,
            )
            .await);
        }
    };
    log_state.record_line("edev: app MCP connected");

    let app_record = match process.app_launch.as_ref() {
        Some(launch) => match await_app_record(&launch.entry_path).await {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                drop(client);
                return Err(fail_startup_handshake(
                    process,
                    stderr_task,
                    stdout_task,
                    &stderr_buffer,
                    "record",
                    &McpError::InternalError("supervisor app record is missing".to_string()),
                    &log_state,
                )
                .await);
            }
            Err(error) => {
                drop(client);
                return Err(fail_startup_handshake(
                    process,
                    stderr_task,
                    stdout_task,
                    &stderr_buffer,
                    "record",
                    &McpError::InternalError(format!(
                        "failed to read supervisor app record: {error}"
                    )),
                    &log_state,
                )
                .await);
            }
        },
        None => None,
    };
    let process_group_id = app_record
        .as_ref()
        .map(|record| record.app_process_group_id)
        .or(process.process_group_id);
    let child = process.child.take();
    let supervisor_pid = process.supervisor_pid;
    let supervisor_exit_task = process.supervisor_exit_task.take();
    let ownership_writer = process.ownership_writer.take();
    let app_launch = process.app_launch.take();

    Ok(AppProcess {
        child,
        process_group_id,
        supervisor_pid,
        supervisor_exit_task,
        app_launch,
        app_record,
        ownership_writer,
        client: Arc::new(AsyncMutex::new(client)),
        mcp_endpoint,
        stdout_task: Some(stdout_task),
        stderr_task: Some(stderr_task),
        stderr_buffer,
        stdout_buffer,
        log_state,
        shutdown_grace: config.shutdown_grace,
    })
}

/// Finalize app startup after an MCP handshake failure.
async fn fail_startup_handshake(
    process: process_lifecycle::SpawnedProcess,
    stderr_task: JoinHandle<()>,
    stdout_task: JoinHandle<()>,
    stderr_buffer: &Arc<Mutex<Vec<u8>>>,
    stage: &str,
    error: &McpError,
    log_state: &LogState,
) -> AppStartError {
    log_state.record_line(&format!("edev: app {stage} failed: {error}"));
    process_lifecycle::shutdown_spawned(process, log_state).await;
    let _stderr_result = stderr_task.await;
    let _stdout_result = stdout_task.await;
    let output = drain_stderr(stderr_buffer).await;
    AppStartError::StartupFailed(format_startup_output(error, &output))
}

/// Reserve an unused loopback address for the next app server.
fn allocate_mcp_endpoint() -> std_io::Result<String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.to_string())
}

/// Connect to a newly launched app, including time spent in an app build command.
// Non-macOS startup probes need mutable access for `Child::try_wait`; macOS
// probes the supervisor task through the same cross-platform call boundary.
#[cfg_attr(target_os = "macos", allow(clippy::needless_pass_by_ref_mut))]
async fn connect_app_client(
    endpoint: &str,
    presentation: Presentation,
    request_timeout: Duration,
    process: &mut process_lifecycle::SpawnedProcess,
) -> Result<tmcp::Client<()>, McpError> {
    let deadline = Instant::now() + APP_CONNECT_TIMEOUT;
    loop {
        let mut client = tmcp::Client::new("edev", env!("CARGO_PKG_VERSION"))
            .with_capabilities(client_capabilities(presentation))
            .with_request_timeout(request_timeout);
        match client.connect_tcp(endpoint.to_string()).await {
            Ok(_) => return Ok(client),
            Err(error) => {
                if process_lifecycle::spawned_process_exited(process)
                    .map_err(McpError::InternalError)?
                {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(error);
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
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

/// Return buffered app stdout for failure bundles.
fn stdout_bundle_text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    snapshot_output(buffer)
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

/// MCP server implementation that proxies tool calls to the app.
struct EdevServer {
    /// Shared runtime state for proxying and host-side lifecycle control.
    state: Arc<AsyncMutex<State>>,
}

#[async_trait]
impl ServerHandler for EdevServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
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
        _arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        let state = Arc::clone(&self.state);
        {
            let mut state_guard = state.lock().await;
            state_guard.mark_activity();
        }
        if !is_host_tool(&name) {
            return Err(McpError::ToolNotFound(name));
        }
        let result = match name.as_str() {
            "start" => {
                let mut state = state.lock().await;
                let start = Instant::now();
                match state.start().await {
                    Ok(StartStatus::Started) => lifecycle_success(
                        "started",
                        start.elapsed(),
                        state
                            .app
                            .as_ref()
                            .and_then(AppProcess::connection_descriptor),
                    ),
                    Ok(StartStatus::AlreadyRunning) => lifecycle_success(
                        "already_running",
                        start.elapsed(),
                        state
                            .app
                            .as_ref()
                            .and_then(AppProcess::connection_descriptor),
                    ),
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
                    Ok(StopStatus::Stopped(shutdown)) => {
                        lifecycle_stop_success("stopped", start.elapsed(), shutdown)
                    }
                    Ok(StopStatus::AlreadyStopped) => {
                        lifecycle_stop_success("already_stopped", start.elapsed(), None)
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
                    Ok(LifecycleStartStatus::Running) => lifecycle_success(
                        "completed",
                        start.elapsed(),
                        state
                            .app
                            .as_ref()
                            .and_then(AppProcess::connection_descriptor),
                    ),
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
                let report = state.lock().await.status_report();
                CallToolResult::new()
                    .with_structured_content(serde_json::to_value(report).expect("status report"))
            }
            _ => return Err(McpError::ToolNotFound(name)),
        };
        Ok(result.into())
    }
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
            let mut outcome = parse_script_eval_outcome(&result)?;
            config.suite.apply_egui_diagnostic_policy(&mut outcome);
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
    decode_tool_result(result, "script_eval", "script_eval outcome")
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
    matches!(name, "start" | "stop" | "restart" | "status")
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

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
struct LifecycleReport {
    status: &'static str,
    elapsed_ms: u64,
    connection: Option<AppConnectionDescriptor>,
    shutdown: Option<ShutdownResult>,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
struct StatusReport {
    state: &'static str,
    app_present: bool,
    process_group_id: Option<i32>,
    supervisor_pid: Option<u32>,
    launch_id: Option<String>,
    connection: Option<AppConnectionDescriptor>,
    registry_entry_path: Option<PathBuf>,
    startup_output: Option<String>,
    mcp_client_attached: bool,
    idle_shutdown: IdleShutdownReport,
    last_shutdown: Option<ShutdownResult>,
    #[cfg(target_os = "macos")]
    presentation: PresentationStatus,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct AppConnectionDescriptor {
    launch_id: String,
    transport: &'static str,
    endpoint: String,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
struct IdleShutdownReport {
    state: &'static str,
    configured_secs: Option<u64>,
    remaining_secs: Option<u64>,
}

/// Build a successful lifecycle tool result.
fn lifecycle_success(
    status: &'static str,
    elapsed: Duration,
    connection: Option<AppConnectionDescriptor>,
) -> CallToolResult {
    CallToolResult::new().with_structured_content(serde_json::json!({
        "ok": true,
        "report": LifecycleReport {
            status,
            elapsed_ms: elapsed.as_millis() as u64,
            connection,
            shutdown: None,
        },
    }))
}

/// Build a successful stop result with its teardown outcome.
fn lifecycle_stop_success(
    status: &'static str,
    elapsed: Duration,
    shutdown: Option<ShutdownResult>,
) -> CallToolResult {
    CallToolResult::new().with_structured_content(serde_json::json!({
        "ok": true,
        "report": LifecycleReport {
            status,
            elapsed_ms: elapsed.as_millis() as u64,
            connection: None,
            shutdown,
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
                connection: None,
                shutdown: None,
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
                connection: None,
                shutdown: None,
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
        presentation: Presentation::Background,
        shutdown_grace: Duration::from_secs(30),
        verbose: false,
        request_timeout: APP_REQUEST_TIMEOUT,
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
    use crate::{
        command::{dump_script_args, eval_output_value, run_eval_script},
        failure_bundle::{
            BUNDLE_COLLECTION_SCRIPT, BUNDLE_DIAGNOSTICS_SCRIPT, bundle_meta, failure_text,
            replace_dir, stable_hash8,
        },
    };

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

    fn successful_app_close_result() -> CallToolResult {
        CallToolResult::new().with_structured_content(serde_json::json!({
            "queued": true,
        }))
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

    #[test]
    fn script_eval_outcome_accepts_json_text_with_image_sidecars() {
        let result = CallToolResult::new()
            .with_content(ContentBlock::image("AQID", "image/png"))
            .with_json_text(serde_json::json!({
                "success": true,
                "value": 42,
                "logs": [],
                "assertions": [],
                "timing": {
                    "compile_ms": 0,
                    "exec_ms": 0,
                    "total_ms": 0
                }
            }))
            .expect("script eval json")
            .with_content(ContentBlock::image("BAUG", "image/png"));

        let outcome = parse_script_eval_outcome(&result).expect("script eval outcome");

        assert_eq!(outcome.value, Some(serde_json::json!(42)));
    }

    fn dump_config(tempdir: &TempDir) -> DumpConfig {
        DumpConfig {
            launch: test_config(tempdir.path().to_path_buf()),
            fixture: None,
            params: BTreeMap::new(),
            viewport: None,
            wait_for_initial_capture: true,
            json: false,
            out: None,
            timeout: None,
        }
    }

    #[test]
    fn dump_projection_args_wait_for_initial_capture_without_fixture() {
        let tempdir = test_tempdir();
        let mut config = dump_config(&tempdir);
        config.viewport = Some("secondary".to_string());

        let args = dump_script_args(&config);
        assert_eq!(args["__dump_wait_capture"], ScriptArgValue::Bool(true));
        assert_eq!(args["__dump_json"], ScriptArgValue::Bool(false));
        assert_eq!(
            args["__dump_viewport"],
            ScriptArgValue::String("secondary".to_string())
        );
    }

    #[test]
    fn dump_projection_args_use_fixture_without_extra_capture_wait() {
        let tempdir = test_tempdir();
        let mut config = dump_config(&tempdir);
        config.fixture = Some("basic.default".to_string());
        config.wait_for_initial_capture = false;
        config.json = true;

        let args = dump_script_args(&config);
        assert_eq!(
            args["__fixture_name"],
            ScriptArgValue::String("basic.default".to_string())
        );
        assert_eq!(args["__dump_wait_capture"], ScriptArgValue::Bool(false));
        assert_eq!(args["__dump_json"], ScriptArgValue::Bool(true));
    }

    #[test]
    fn dump_projection_args_pass_fixture_params() {
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
        config.wait_for_initial_capture = false;

        let args = dump_script_args(&config);
        assert_eq!(args["enabled"], ScriptArgValue::Bool(true));
        assert_eq!(args["label"], ScriptArgValue::String("A|B".to_string()));
        assert_eq!(args["offset"], ScriptArgValue::Int(180));
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
                    "egui_diagnostics": {
                        "entries": [{
                            "kind": "id_clash",
                            "severity": "warning",
                            "message": "duplicate id",
                            "viewport_id": "root",
                            "frame": 4
                        }],
                        "dropped": 0
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
        assert_eq!(
            meta["failure"]["egui_diagnostics"]["entries"][0]["kind"],
            "id_clash"
        );

        let text = failure_text(&outcome).expect("failure text");
        assert!(text.contains("failure: expected ready"));
        assert!(text.contains("before failure"));
        assert!(text.contains("basic.default"));
        assert!(text.contains("egui diagnostics"));
        assert!(text.contains("duplicate id"));
    }

    #[test]
    fn stdout_bundle_text_preserves_captured_output() {
        let empty = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(stdout_bundle_text(&empty), "");

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
                fail_on_egui_diagnostics: true,
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
                fail_on_egui_diagnostics: true,
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
                fail_on_egui_diagnostics: true,
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
            _protocol_version: ProtocolVersion,
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
            if name == "app_close" {
                Ok(successful_app_close_result().into())
            } else if name == "script_eval" {
                Ok(successful_script_eval_result().into())
            } else if name == "script_api" {
                Ok(CallToolResult::new()
                    .with_text_content("live app script api")
                    .into())
            } else {
                Err(McpError::ToolNotFound(name))
            }
        }
    }

    struct RecordingEvalServer {
        requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
    }

    struct BundleSmokeServer {
        requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
    }

    #[async_trait]
    impl ServerHandler for RecordingEvalServer {
        async fn initialize(
            &self,
            _context: &ServerCtx,
            _protocol_version: ProtocolVersion,
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
            if name == "app_close" {
                return Ok(successful_app_close_result().into());
            }
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
            _protocol_version: ProtocolVersion,
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
            if name == "app_close" {
                return Ok(successful_app_close_result().into());
            }
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
            _protocol_version: ProtocolVersion,
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
            if name == "app_close" {
                Ok(successful_app_close_result().into())
            } else if name == "script_eval" {
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
            supervisor_pid: None,
            supervisor_exit_task: None,
            app_launch: None,
            app_record: None,
            ownership_writer: None,
            client: Arc::new(AsyncMutex::new(client)),
            mcp_endpoint: "127.0.0.1:1".to_string(),
            stdout_task: None,
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
            shutdown_grace: Duration::from_secs(30),
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
            supervisor_pid: None,
            supervisor_exit_task: None,
            app_launch: None,
            app_record: None,
            ownership_writer: None,
            client: Arc::new(AsyncMutex::new(client)),
            mcp_endpoint: "127.0.0.1:1".to_string(),
            stdout_task: None,
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
            shutdown_grace: Duration::from_secs(30),
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
            supervisor_pid: None,
            supervisor_exit_task: None,
            app_launch: None,
            app_record: None,
            ownership_writer: None,
            client: Arc::new(AsyncMutex::new(client)),
            mcp_endpoint: "127.0.0.1:1".to_string(),
            stdout_task: None,
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
            shutdown_grace: Duration::from_secs(30),
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
            supervisor_pid: None,
            supervisor_exit_task: None,
            app_launch: None,
            app_record: None,
            ownership_writer: None,
            client: Arc::new(AsyncMutex::new(client)),
            mcp_endpoint: "127.0.0.1:1".to_string(),
            stdout_task: None,
            stderr_task: None,
            stderr_buffer: Arc::new(Mutex::new(Vec::new())),
            stdout_buffer: Arc::new(Mutex::new(Vec::new())),
            log_state: LogState::new(false),
            shutdown_grace: Duration::from_secs(30),
        };

        (app, handle)
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
            ]
        );
        assert_eq!(stopped_names, running_names);
    }

    #[test]
    fn tool_error_reports_restart_failed_kind() {
        let result = tool_error(ErrorKind::RestartFailed, "restart failed");
        let payload = result.structured_content.expect("structured content");
        assert_eq!(payload["error"]["kind"], "restart_failed");
    }

    #[test]
    fn parse_fixture_list_decodes_registered_fixtures() {
        let fixtures = parse_fixture_list(successful_outcome(&serde_json::json!([
            {
                "name": "basic.default",
                "description": "baseline",
                "ready": [{
                    "widget_id": "basic.status",
                    "viewport_id": "secondary",
                    "condition": { "visible": true }
                }],
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
        assert_eq!(fixtures[0].ready.len(), 1);
        assert_eq!(fixtures[0].ready[0].widget_id, "basic.status");
        assert_eq!(
            fixtures[0].ready[0].viewport_id.as_deref(),
            Some("secondary")
        );
        assert_eq!(fixtures[0].params[0].name, "offset");
        assert_eq!(fixtures[0].tags, vec!["scroll"]);
    }

    #[test]
    fn parse_fixture_list_defaults_omitted_empty_collections() {
        let fixtures = parse_fixture_list(successful_outcome(&serde_json::json!([
            {
                "name": "basic.empty",
                "description": "empty collections"
            }
        ])))
        .expect("fixtures");

        assert_eq!(fixtures.len(), 1);
        assert!(fixtures[0].preconditions.is_empty());
        assert!(fixtures[0].ready.is_empty());
        assert!(fixtures[0].params.is_empty());
        assert!(fixtures[0].tags.is_empty());
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

        assert!(matches!(stopped, StopStatus::Stopped(_)));
        assert!(matches!(already_stopped, StopStatus::AlreadyStopped));
        assert!(matches!(state.status, AppStatus::NotRunning));
        assert_eq!(state.last_shutdown, Some(ShutdownResult::Graceful));
    }

    #[tokio::test]
    async fn shutdown_resolution_reports_normal_exit() {
        let result = resolve_shutdown(Ok(()), async { Ok(()) }, Duration::from_secs(1)).await;
        assert_eq!(result, ShutdownResult::Graceful);
    }

    #[tokio::test]
    async fn shutdown_resolution_reports_missing_mcp() {
        let result = resolve_shutdown(
            Err(ShutdownCause::AppMcpUnavailable("closed".to_string())),
            async { Ok(()) },
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(
            result,
            ShutdownResult::Forced {
                cause: ShutdownCause::AppMcpUnavailable("closed".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn shutdown_resolution_reports_close_failure() {
        let result = resolve_shutdown(
            Err(ShutdownCause::AppCloseFailed("rejected".to_string())),
            async { Ok(()) },
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(
            result,
            ShutdownResult::Forced {
                cause: ShutdownCause::AppCloseFailed("rejected".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn shutdown_resolution_reports_deadline_expiry() {
        let result =
            resolve_shutdown(Ok(()), pending::<Result<(), String>>(), Duration::ZERO).await;
        assert_eq!(
            result,
            ShutdownResult::Forced {
                cause: ShutdownCause::DeadlineExpired,
            }
        );
    }

    #[test]
    fn forced_shutdown_fails_one_shot_contract() {
        let result = ShutdownResult::Forced {
            cause: ShutdownCause::DeadlineExpired,
        }
        .ensure_graceful();
        assert!(matches!(result, Err(EdevError::ForcedShutdown(_))));
    }

    #[test]
    fn status_report_covers_all_lifecycle_states() {
        let tempdir = test_tempdir();
        let mut state = make_state(&tempdir);
        assert_eq!(state.status_report().state, "not_running");
        assert_eq!(state.status_report().idle_shutdown.state, "disabled");

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
    fn client_capabilities_encode_presentation_intent() {
        let capabilities = client_capabilities(Presentation::Foreground);
        assert_eq!(
            capabilities
                .experimental
                .as_ref()
                .and_then(|values| values.get(EXPERIMENTAL_PRESENTATION_CAPABILITY)),
            Some(&serde_json::json!("foreground"))
        );
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
