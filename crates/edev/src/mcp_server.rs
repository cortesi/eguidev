//! The MCP server of the launcher: its lifecycle tools, reports, and idle
//! shutdown.

use super::*;

#[cfg(unix)]
/// Wait for shutdown signals to terminate the app process cleanly.
pub async fn shutdown_signal() {
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
pub async fn shutdown_signal() {
    let _ctrl_c = tokio::signal::ctrl_c().await;
}

/// Wait until pre-client launcher inactivity exceeds the configured timeout.
pub async fn wait_for_idle_shutdown(state: Arc<AsyncMutex<State>>, idle_after: Duration) {
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
pub enum IdleShutdownAction {
    /// Re-check idle state after this duration.
    Sleep(Duration),
    /// A client is attached, so the guard should stay pending until stdio
    /// exits.
    Suspend,
    /// No client attached before the idle budget elapsed.
    Shutdown,
}

/// MCP server implementation for launcher lifecycle tools.
pub struct EdevServer {
    /// Shared runtime state for host-side lifecycle control.
    pub state: Arc<AsyncMutex<State>>,
}

/// Start the app without holding the shared state lock during process startup.
pub async fn start_app_unlocked(state: &Arc<AsyncMutex<State>>) -> Result<StartStatus, EdevError> {
    start_app_unlocked_with(state, |config, log_state| async move {
        spawn_app(&config, log_state).await
    })
    .await
}

/// Start the app through a supplied spawn routine without locking state during
/// I/O.
pub async fn start_app_unlocked_with<F, Fut>(
    state: &Arc<AsyncMutex<State>>,
    spawn: F,
) -> Result<StartStatus, EdevError>
where
    F: FnOnce(LaunchConfig, LogState) -> Fut,
    Fut: Future<Output = Result<AppProcess, AppStartError>> + Send,
{
    let prepared = {
        let mut state = state.lock().await;
        match state.prepare_start() {
            PrepareStart::AlreadyRunning => return Ok(StartStatus::AlreadyRunning),
            PrepareStart::AppStarting => return Ok(StartStatus::AppStarting),
            PrepareStart::RestartRequired(output) => {
                return Ok(StartStatus::RestartRequired(output));
            }
            PrepareStart::Ready => (state.config.clone(), state.log_state.clone()),
        }
    };
    let (config, log_state) = prepared;
    let spawned = spawn(config, log_state).await;
    let mut state = state.lock().await;
    match state.install_spawned(LifecycleAction::Start, spawned).await {
        Ok(LifecycleStartStatus::Running) => Ok(StartStatus::Started),
        Ok(LifecycleStartStatus::StartupFailed(output)) => Ok(StartStatus::StartupFailed(output)),
        Err(error) => Err(error),
    }
}

/// Restart the app without holding state during I/O, retrying closed
/// transports.
pub async fn restart_app_unlocked(state: &Arc<AsyncMutex<State>>) -> UnlockedRestart {
    let mut attempt = 1;
    loop {
        let outcome = restart_app_unlocked_once(state).await;
        let should_retry = matches!(
            &outcome,
            UnlockedRestart::Done(result)
                if restart_result_is_transport_closed(result) && attempt < RESTART_MAX_ATTEMPTS
        );
        if should_retry {
            state.lock().await.log_edev(format!(
                "restart attempt {attempt} failed with closed transport; retrying"
            ));
            attempt += 1;
            continue;
        }
        return outcome;
    }
}

/// Perform one unlocked restart attempt.
pub async fn restart_app_unlocked_once(state: &Arc<AsyncMutex<State>>) -> UnlockedRestart {
    let old_app = {
        let mut state = state.lock().await;
        match state.prepare_restart() {
            PrepareRestart::AppStarting => return UnlockedRestart::AppStarting,
            PrepareRestart::Ready(app) => app,
        }
    };
    if let Some(app) = old_app {
        let _shutdown = (*app).shutdown().await;
    }
    let (config, log_state) = {
        let state = state.lock().await;
        (state.config.clone(), state.log_state.clone())
    };
    let spawned = spawn_app(&config, log_state).await;
    let mut state = state.lock().await;
    UnlockedRestart::Done(
        state
            .install_spawned(LifecycleAction::Restart, spawned)
            .await,
    )
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
        Ok(ListToolsResult::new().with_tools(lifecycle_tools()))
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
        let result = match name.as_str() {
            "start" => {
                let start = Instant::now();
                match start_app_unlocked(&state).await {
                    Ok(StartStatus::Started) => {
                        let connection = state.lock().await.connection();
                        lifecycle_success("started", start.elapsed(), connection)
                    }
                    Ok(StartStatus::AlreadyRunning) => {
                        let connection = state.lock().await.connection();
                        lifecycle_success("already_running", start.elapsed(), connection)
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
                    Err(EdevError::StartTimeout(message)) => lifecycle_failed(
                        ErrorKind::StartTimeout,
                        format!("App start timed out: {message}. Call start again."),
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
                    StopStatus::Stopped(shutdown) => {
                        lifecycle_stop_success("stopped", start.elapsed(), shutdown)
                    }
                    StopStatus::AlreadyStopped => {
                        lifecycle_stop_success("already_stopped", start.elapsed(), None)
                    }
                }
            }
            "restart" => {
                let start = Instant::now();
                match restart_app_unlocked(&state).await {
                    UnlockedRestart::AppStarting => tool_error(
                        ErrorKind::AppStarting,
                        "App is starting. Try again shortly.",
                    ),
                    UnlockedRestart::Done(Ok(LifecycleStartStatus::Running)) => {
                        let connection = state.lock().await.connection();
                        lifecycle_success("completed", start.elapsed(), connection)
                    }
                    UnlockedRestart::Done(Ok(LifecycleStartStatus::StartupFailed(output))) => {
                        lifecycle_startup_failed(
                            "App startup failed. Fix the issue and call restart again.",
                            &output,
                            start.elapsed(),
                        )
                    }
                    UnlockedRestart::Done(Err(EdevError::StartTimeout(message))) => {
                        lifecycle_failed(
                            ErrorKind::StartTimeout,
                            format!("App start timed out: {message}. Call restart again."),
                            start.elapsed(),
                        )
                    }
                    UnlockedRestart::Done(Err(error)) => lifecycle_failed(
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

/// Decode a proxied `script_eval` tool result back into the checked-in outcome
/// shape.
pub fn parse_script_eval_outcome(result: &CallToolResult) -> Result<ScriptEvalOutcome, String> {
    decode_tool_result(result, "script_eval", "script_eval outcome")
}

/// Serialize a `script_eval` request into an MCP arguments object.
pub fn script_eval_request_value(request: ScriptEvalRequest) -> Arguments {
    Arguments::from_struct(request).expect("script eval request should serialize")
}

/// Failure of the readiness probe of a started app.
#[derive(Debug)]
pub enum ProbeFailure {
    /// The script runtime did not answer within the request timeout.
    Timeout(String),
    /// The script runtime answered with a failure.
    Failed(String),
}

/// Probe the app's `script_eval` tool to confirm the script runtime is ready.
/// The first evaluation can wait for a cold type checker, so the probe gets
/// the whole request timeout.
pub async fn probe_script_eval_ready(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    timeout: Duration,
) -> Result<(), ProbeFailure> {
    let request = script_eval_request_value(ScriptEvalRequest {
        script: "return true".to_string(),
        timeout_ms: Some(timeout.as_millis().min(u128::from(u64::MAX)) as u64),
        options: None,
    });
    let result = {
        let client = client.lock().await;
        client
            .call_tool("script_eval".to_string(), request)
            .await
            .map_err(|error| match error {
                McpError::Timeout { .. } => ProbeFailure::Timeout(error.to_string()),
                error => ProbeFailure::Failed(error.to_string()),
            })?
    };
    let outcome = parse_script_eval_outcome(&result).map_err(ProbeFailure::Failed)?;
    if outcome.success {
        return Ok(());
    }
    let error = outcome.error.as_ref();
    let message = error
        .map(|error| error.message.clone())
        .unwrap_or_else(|| "script_eval readiness probe failed".to_string());
    if error.is_some_and(|error| {
        error.error_type == "timeout" || error.code.as_deref() == Some("timeout")
    }) {
        Err(ProbeFailure::Timeout(message))
    } else {
        Err(ProbeFailure::Failed(message))
    }
}

/// The tools of the launcher. The list does not depend on the app state.
pub fn lifecycle_tools() -> Vec<Tool> {
    vec![start_tool(), stop_tool(), restart_tool(), status_tool()]
}

/// Tool definition for starting the app process.
pub fn start_tool() -> Tool {
    Tool::new("start", ToolSchema::default())
        .with_description("Start the underlying app process if it is not already running.")
}

/// Tool definition for stopping the app process.
pub fn stop_tool() -> Tool {
    Tool::new("stop", ToolSchema::default())
        .with_description("Stop the underlying app process if it is running.")
}

/// Tool definition for restarting the app process.
pub fn restart_tool() -> Tool {
    Tool::new("restart", ToolSchema::default())
        .with_description("Restart the underlying app process.")
}

/// Tool definition for reporting launcher lifecycle state.
pub fn status_tool() -> Tool {
    Tool::new("status", ToolSchema::default())
        .with_description("Report the current launcher and app lifecycle state.")
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleReport {
    pub status: &'static str,
    pub elapsed_ms: u64,
    pub connection: Option<AppConnectionDescriptor>,
    pub shutdown: Option<ShutdownResult>,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub state: &'static str,
    pub app_present: bool,
    pub process_group_id: Option<i32>,
    pub supervisor_pid: Option<u32>,
    pub launch_id: Option<String>,
    pub connection: Option<AppConnectionDescriptor>,
    pub registry_entry_path: Option<PathBuf>,
    pub startup_output: Option<String>,
    pub mcp_client_attached: bool,
    pub idle_shutdown: IdleShutdownReport,
    pub last_shutdown: Option<ShutdownResult>,
    #[cfg(target_os = "macos")]
    pub presentation: PresentationStatus,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppConnectionDescriptor {
    pub launch_id: String,
    pub transport: &'static str,
    pub endpoint: String,
}

#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug, Clone, Serialize)]
pub struct IdleShutdownReport {
    pub state: &'static str,
    pub configured_secs: Option<u64>,
    pub remaining_secs: Option<u64>,
}

/// Build a successful lifecycle tool result.
pub fn lifecycle_success(
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
pub fn lifecycle_stop_success(
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
pub fn lifecycle_startup_failed(
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
pub fn lifecycle_failed(kind: ErrorKind, message: String, elapsed: Duration) -> CallToolResult {
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
pub enum ErrorKind {
    /// App is starting and cannot accept tool calls.
    AppStarting,
    /// Restart is required to recover.
    RestartRequired,
    /// Start failed for a non-startup reason.
    StartFailed,
    /// Restart failed for a non-startup reason.
    RestartFailed,
    /// Startup failed before the app became ready.
    StartupFailed,
    /// The script runtime did not answer the readiness probe in time.
    StartTimeout,
}

/// Build a structured tool error result.
pub fn tool_error(kind: ErrorKind, message: impl Into<String>) -> CallToolResult {
    build_tool_error(kind, message.into(), None)
}

/// Build a structured tool error result with extra data.
pub fn tool_error_with_data(
    kind: ErrorKind,
    message: impl Into<String>,
    data: &serde_json::Value,
) -> CallToolResult {
    build_tool_error(kind, message.into(), Some(data))
}

/// Build a structured tool error result with optional data payload.
pub fn build_tool_error(
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppStarting => "app_starting",
            Self::RestartRequired => "restart_required",
            Self::StartFailed => "start_failed",
            Self::RestartFailed => "restart_failed",
            Self::StartupFailed => "startup_failed",
            Self::StartTimeout => "start_timeout",
        }
    }
}

/// Return true when a restart result indicates transient transport closure.
pub fn restart_result_is_transport_closed(
    result: &Result<LifecycleStartStatus, EdevError>,
) -> bool {
    matches!(
        result,
        Ok(LifecycleStartStatus::StartupFailed(output)) if transport_message_is_closed(output)
    )
}

/// Return true when an MCP transport error string indicates closed transport.
pub fn transport_message_is_closed(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["closed", "disconnect", "broken pipe", "eof"]
        .iter()
        .any(|fragment| message.contains(fragment))
}
