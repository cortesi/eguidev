//! The lifecycle state of the launcher and of its managed app.

use super::*;

#[derive(Clone, Debug, Default)]
/// Lightweight logger for app and launcher messages.
pub struct LogState {
    /// Whether launcher lifecycle logs should be emitted.
    pub verbose: bool,
}

impl LogState {
    /// Build a logger that conditionally emits messages.
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    /// Record a single log line, preserving newlines when present.
    pub fn record_line(&self, line: &str) {
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
pub struct State {
    /// Launcher configuration.
    pub config: LaunchConfig,
    /// Instance registry entry for this launcher.
    pub instance_registry: InstanceRegistry,
    /// Active app process, if running.
    pub app: Option<AppProcess>,
    /// Current lifecycle status.
    pub status: AppStatus,
    /// Timestamp of the last MCP interaction handled by this launcher.
    pub last_activity: Instant,
    /// Configured idle guard for the launcher, if this is an MCP session.
    pub idle_shutdown_after: Option<Duration>,
    /// Whether the stdio MCP client completed initialization.
    pub mcp_client_attached: bool,
    /// Most recent completed app shutdown, retained until the next launch.
    pub last_shutdown: Option<ShutdownResult>,
    /// Logger for launcher lifecycle messages.
    pub log_state: LogState,
}

/// Future returned by the app spawn helper.
pub type SpawnFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AppProcess, AppStartError>> + Send + 'a>>;

impl State {
    /// Create a new runtime state from the provided configuration.
    pub fn new(config: LaunchConfig, instance_registry: InstanceRegistry) -> Self {
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
    pub fn enable_idle_shutdown(&mut self, idle_after: Duration) {
        self.idle_shutdown_after = Some(idle_after);
    }

    /// Record an internal launcher log line into the shared log buffer.
    pub fn log_edev(&self, line: impl AsRef<str>) {
        let line = line.as_ref();
        self.log_state.record_line(&format!("edev: {line}"));
    }

    /// Mark launcher activity from user-visible tool execution.
    pub fn mark_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Mark the stdio MCP client as initialized and attached.
    pub fn mark_client_attached(&mut self) {
        self.mcp_client_attached = true;
        self.mark_activity();
    }

    /// Stop the managed app process and reset launcher state without
    /// unregistering the launcher.
    pub async fn stop_app(&mut self) -> Result<StopStatus, EdevError> {
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
    pub async fn shutdown(&mut self) -> Result<(), EdevError> {
        let stopped = self.stop_app().await?;
        self.instance_registry.unregister()?;
        stopped.ensure_graceful()
    }

    /// Start the app process unless it is already running.
    #[cfg(test)]
    pub async fn start(&mut self) -> Result<StartStatus, EdevError> {
        match self.prepare_start() {
            PrepareStart::AlreadyRunning => Ok(StartStatus::AlreadyRunning),
            PrepareStart::AppStarting => Ok(StartStatus::AppStarting),
            PrepareStart::RestartRequired(output) => Ok(StartStatus::RestartRequired(output)),
            PrepareStart::Ready => {
                self.start_with(|config, log_state| Box::pin(spawn_app(config, log_state)))
                    .await
            }
        }
    }

    /// Mark an idle app as starting, or report why a start cannot proceed.
    pub fn prepare_start(&mut self) -> PrepareStart {
        match &self.status {
            AppStatus::Running => PrepareStart::AlreadyRunning,
            AppStatus::StartupFailed { output } => PrepareStart::RestartRequired(output.clone()),
            AppStatus::Starting => PrepareStart::AppStarting,
            AppStatus::NotRunning => {
                self.status = AppStatus::Starting;
                self.last_shutdown = None;
                self.log_edev("start requested");
                PrepareStart::Ready
            }
        }
    }

    /// Mark the app as starting and detach the process that restart must stop.
    pub fn prepare_restart(&mut self) -> PrepareRestart {
        match &self.status {
            AppStatus::Starting => PrepareRestart::AppStarting,
            _ => {
                self.status = AppStatus::Starting;
                self.last_shutdown = None;
                self.log_edev("restart requested");
                PrepareRestart::Ready(self.app.take().map(Box::new))
            }
        }
    }

    /// Restart the app process using the default spawn behavior.
    pub async fn restart(&mut self) -> Result<LifecycleStartStatus, EdevError> {
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

    /// Resolve the current direct app client, or return a lifecycle-specific
    /// tool error.
    pub fn app_client(&self) -> Result<Arc<AsyncMutex<tmcp::Client<()>>>, CallToolResult> {
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
    #[cfg(test)]
    pub async fn start_with<F>(&mut self, spawn: F) -> Result<StartStatus, EdevError>
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
    pub async fn restart_with<F>(&mut self, spawn: F) -> Result<LifecycleStartStatus, EdevError>
    where
        F: for<'a> FnOnce(&'a LaunchConfig, LogState) -> SpawnFuture<'a>,
    {
        self.spawn_with(LifecycleAction::Restart, true, spawn).await
    }

    /// Spawn and attach an app process for either a start or restart
    /// transition.
    pub async fn spawn_with<F>(
        &mut self,
        action: LifecycleAction,
        replace_existing: bool,
        spawn: F,
    ) -> Result<LifecycleStartStatus, EdevError>
    where
        F: for<'a> FnOnce(&'a LaunchConfig, LogState) -> SpawnFuture<'a>,
    {
        if !matches!(self.status, AppStatus::Starting) {
            self.status = AppStatus::Starting;
            self.last_shutdown = None;
            self.log_edev(format!("{} requested", action.as_str()));
        }
        if replace_existing && let Some(app) = self.app.take() {
            let _shutdown = app.shutdown().await;
        }
        let spawned = spawn(&self.config, self.log_state.clone()).await;
        self.install_spawned(action, spawned).await
    }

    /// Install a completed spawn result unless another action cancelled it.
    pub async fn install_spawned(
        &mut self,
        action: LifecycleAction,
        spawned: Result<AppProcess, AppStartError>,
    ) -> Result<LifecycleStartStatus, EdevError> {
        if !matches!(self.status, AppStatus::Starting) {
            if let Ok(app) = spawned {
                let shutdown = app.shutdown().await;
                self.last_shutdown = Some(shutdown);
            }
            self.log_edev(format!(
                "{} cancelled because the app was stopped",
                action.as_str()
            ));
            return Err(EdevError::AppStart(format!(
                "{} cancelled because the app was stopped",
                action.as_str()
            )));
        }
        match spawned {
            Ok(app) => {
                match probe_script_eval_ready(&app.client, self.config.request_timeout).await {
                    Ok(()) => {
                        self.status = AppStatus::Running;
                        self.app = Some(app);
                        self.log_edev(format!("{} completed", action.as_str()));
                        Ok(LifecycleStartStatus::Running)
                    }
                    Err(ProbeFailure::Failed(output)) => {
                        let shutdown = app.shutdown().await;
                        self.last_shutdown = Some(shutdown);
                        self.status = AppStatus::StartupFailed {
                            output: output.clone(),
                        };
                        self.log_edev(format!("{} failed during app startup", action.as_str()));
                        Ok(LifecycleStartStatus::StartupFailed(output))
                    }
                    Err(ProbeFailure::Timeout(message)) => {
                        // A slow script runtime is not a defect of the app, so
                        // a later start needs no
                        // restart.
                        let shutdown = app.shutdown().await;
                        self.last_shutdown = Some(shutdown);
                        self.status = AppStatus::NotRunning;
                        self.log_edev(format!(
                            "{} timed out during the readiness probe",
                            action.as_str()
                        ));
                        Err(EdevError::StartTimeout(message))
                    }
                }
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
    pub fn tools_list(&self) -> Vec<Tool> {
        vec![start_tool(), stop_tool(), restart_tool(), status_tool()]
    }

    /// Build a structured status snapshot of the managed app lifecycle.
    pub fn status_report(&self) -> StatusReport {
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
    pub fn idle_shutdown_report(&self) -> IdleShutdownReport {
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
pub struct AppProcess {
    /// Child process handle for `cargo run`.
    pub child: Option<Child>,
    /// Process group id for the running app process tree.
    pub process_group_id: Option<i32>,
    /// PID of the macOS supervisor, when present.
    pub supervisor_pid: Option<u32>,
    /// Task that owns and reaps the supervisor child.
    pub supervisor_exit_task: Option<JoinHandle<std_io::Result<ExitStatus>>>,
    /// Exact launch identity passed to the supervisor.
    pub app_launch: Option<AppLaunch>,
    /// Exact metadata written by the supervisor.
    pub app_record: Option<AppRecord>,
    /// Per-launch ownership writer held only by this outer AppProcess.
    pub ownership_writer: Option<process_lifecycle::OwnershipWriter>,
    /// Connected MCP client speaking to the app over its socket.
    pub client: Arc<AsyncMutex<tmcp::Client<()>>>,
    /// Path of the Unix domain socket of the app, shared with external
    /// callers.
    pub mcp_endpoint: String,
    /// The socket file, which the launcher removes when the app ends.
    pub mcp_socket: Option<SocketFile>,
    /// Background task streaming stdout.
    pub stdout_task: Option<JoinHandle<()>>,
    /// Background task streaming stderr.
    pub stderr_task: Option<JoinHandle<()>>,
    /// Captured stderr output, primarily for startup errors.
    pub stderr_buffer: Arc<Mutex<Vec<u8>>>,
    /// Captured stdout output when stdout is not consumed by the MCP transport.
    pub stdout_buffer: Arc<Mutex<Vec<u8>>>,
    /// Logger for process lifecycle messages.
    pub log_state: LogState,
    /// Deadline for normal app closure before forced cleanup.
    pub shutdown_grace: Duration,
}

impl AppProcess {
    /// Trigger immediate app termination without waiting for child process
    /// exit.
    pub fn start_termination(&mut self) {
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

    /// Request normal app closure and escalate only when the request or exit
    /// fails.
    pub async fn shutdown(mut self) -> ShutdownResult {
        let client = Arc::clone(&self.client);
        let shutdown_grace = self.shutdown_grace;
        let result = resolve_shutdown(
            request_app_close(&client),
            self.wait_for_exit(),
            shutdown_grace,
        )
        .await;
        if result.is_forced() {
            self.start_termination();
            let _wait_result = self.wait_for_exit().await;
        } else {
            self.process_group_id.take();
            self.supervisor_pid.take();
            self.ownership_writer.take();
        }
        self.remove_app_record();
        self.mcp_socket.take();
        if let Some(task) = self.stderr_task.take() {
            let _wait_result = task.await;
        }
        if let Some(task) = self.stdout_task.take() {
            let _wait_result = task.await;
        }
        let _drain_result = drain_stderr(&self.stderr_buffer).await;
        result
    }

    /// Remove this exact app record after the managed process tree has exited.
    pub fn remove_app_record(&self) {
        let (Some(launch), Some(record)) = (&self.app_launch, &self.app_record) else {
            return;
        };
        if let Err(error) = remove_app_record_if_matches(&launch.entry_path, record) {
            eprintln!(
                "edev: could not remove app record {}: {error}",
                launch.entry_path.display()
            );
        }
    }

    /// Await the existing supervisor or direct-child exit event, retaining its
    /// handle if this wait is cancelled before completion.
    pub async fn wait_for_exit(&mut self) -> Result<(), String> {
        if let Some(task) = self.supervisor_exit_task.as_mut() {
            let result = task.await;
            self.supervisor_exit_task.take();
            let status = result
                .map_err(|error| format!("supervisor exit task failed: {error}"))?
                .map_err(|error| format!("supervisor exit failed: {error}"))?;
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
    pub fn connection_descriptor(&self) -> Option<AppConnectionDescriptor> {
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
            transport: "unix",
            endpoint: self.mcp_endpoint.clone(),
        })
    }
}

/// Ask the connected app to queue normal root-viewport closure.
pub async fn request_app_close(
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

/// Bound the close request and event-driven exit observation by one deadline.
pub async fn resolve_shutdown<C, F>(close: C, exit: F, grace: Duration) -> ShutdownResult
where
    C: Future<Output = Result<(), ShutdownCause>>,
    F: Future<Output = Result<(), String>>,
{
    let shutdown = async {
        close.await?;
        exit.await.map_err(ShutdownCause::ExitFailed)
    };
    match timeout(grace, shutdown).await {
        Ok(Ok(())) => ShutdownResult::Graceful,
        Ok(Err(cause)) => ShutdownResult::Forced { cause },
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
pub enum AppStatus {
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
    pub fn as_str(&self) -> &'static str {
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
pub enum AppStartError {
    /// App startup failed before the MCP handshake completed.
    StartupFailed(String),
    /// Other startup failure.
    Other(String),
}

#[derive(Debug)]
/// Outcome of a completed start or restart path.
pub enum LifecycleStartStatus {
    /// Startup completed successfully.
    Running,
    /// Startup failed before the app became ready.
    StartupFailed(String),
}

#[derive(Debug)]
/// State transition selected before starting an app without holding the state
/// lock.
pub enum PrepareStart {
    /// The app is already running.
    AlreadyRunning,
    /// Another lifecycle action is starting the app.
    AppStarting,
    /// The previous startup failed and requires an explicit restart.
    RestartRequired(String),
    /// The caller can start the app.
    Ready,
}

/// State transition selected before restarting without holding the state lock.
pub enum PrepareRestart {
    /// Another lifecycle action is starting the app.
    AppStarting,
    /// The caller can restart after stopping the detached app, if present.
    Ready(Option<Box<AppProcess>>),
}

/// Outcome of a restart performed without holding the state lock during I/O.
pub enum UnlockedRestart {
    /// Another lifecycle action is starting the app.
    AppStarting,
    /// The restart attempt completed.
    Done(Result<LifecycleStartStatus, EdevError>),
}

#[derive(Debug)]
/// Outcome of a start attempt.
pub enum StartStatus {
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
pub enum StopStatus {
    /// A running app was stopped or a failed startup state was cleared.
    Stopped(Option<ShutdownResult>),
    /// No app was running.
    AlreadyStopped,
}

impl StopStatus {
    /// Convert forced cleanup into the failure contract for one-shot commands.
    pub fn ensure_graceful(self) -> Result<(), EdevError> {
        match self {
            Self::Stopped(Some(result)) => result.ensure_graceful(),
            Self::Stopped(None) | Self::AlreadyStopped => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
/// Result of one managed app shutdown.
pub enum ShutdownResult {
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
    pub fn is_forced(&self) -> bool {
        matches!(self, Self::Forced { .. })
    }

    /// Convert this result into the one-shot command contract.
    pub fn ensure_graceful(self) -> Result<(), EdevError> {
        match self {
            Self::Graceful => Ok(()),
            Self::Forced { cause } => Err(EdevError::ForcedShutdown(cause.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
/// Cause of forced managed app cleanup.
pub enum ShutdownCause {
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
pub enum LifecycleAction {
    /// Start the app without replacing a running process.
    Start,
    /// Replace any existing app process with a fresh one.
    Restart,
}

impl LifecycleAction {
    /// Lowercase label for logging.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Restart => "restart",
        }
    }
}
