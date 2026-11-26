//! The spawn of the app and the connection of its MCP client.

use super::*;

/// Read the supervisor's app record, tolerating the write that follows the app
/// spawn. Returns `Ok(None)` only after the record stays absent for
/// [`APP_RECORD_TIMEOUT`].
pub async fn await_app_record(path: &Path) -> Result<Option<AppRecord>, std_io::Error> {
    let deadline = Instant::now() + APP_RECORD_TIMEOUT;
    loop {
        if let Some(record) = read_app_record(path)? {
            return Ok(Some(record));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(APP_RECORD_POLL_INTERVAL).await;
    }
}

/// Encode the launcher's private presentation intent for the app handshake.
pub fn client_capabilities(presentation: Presentation) -> ClientCapabilities {
    ClientCapabilities::default().with_experimental_capability(
        EXPERIMENTAL_PRESENTATION_CAPABILITY,
        serde_json::Value::String(presentation.as_str().to_string()),
    )
}

/// Spawn the app and connect an MCP client to its Unix domain socket.
pub async fn spawn_app(
    config: &LaunchConfig,
    log_state: LogState,
) -> Result<AppProcess, AppStartError> {
    let mcp_socket = SocketFile::allocate(&config.cwd)
        .map_err(|error| AppStartError::Other(format!("allocate the app MCP socket: {error}")))?;
    let mcp_endpoint = mcp_socket.path.display().to_string();
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
        Some(launch) => {
            let record = match await_app_record(&launch.entry_path).await {
                Ok(Some(record)) => Ok(record),
                Ok(None) => Err("supervisor app record is missing".to_string()),
                Err(error) => Err(format!("failed to read supervisor app record: {error}")),
            };
            match record {
                Ok(record) => Some(record),
                Err(message) => {
                    drop(client);
                    return Err(fail_startup_handshake(
                        process,
                        stderr_task,
                        stdout_task,
                        &stderr_buffer,
                        "record",
                        &McpError::InternalError(message),
                        &log_state,
                    )
                    .await);
                }
            }
        }
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
        mcp_socket: Some(mcp_socket),
        stdout_task: Some(stdout_task),
        stderr_task: Some(stderr_task),
        stderr_buffer,
        stdout_buffer,
        log_state,
        shutdown_grace: config.shutdown_grace,
    })
}

/// Finalize app startup after an MCP handshake failure.
pub async fn fail_startup_handshake(
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

/// Longest socket path that `sun_path` of every supported platform holds,
/// with its terminating NUL. macOS bounds `sun_path` at 104 bytes.
pub const MAX_SOCKET_PATH: usize = 103;

/// The socket path of one launch. Dropping it removes the socket file, which
/// the app created.
#[derive(Debug)]
pub struct SocketFile {
    /// Path of the socket.
    pub path: PathBuf,
}

impl SocketFile {
    /// Name a new socket path for one launch below the instance registry of
    /// `cwd`, or below the temporary directory when that path would exceed
    /// the `sun_path` bound.
    pub fn allocate(cwd: &Path) -> std_io::Result<Self> {
        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let name = format!("mcp-{}-{sequence}.sock", process::id());
        let registry = cwd.join(instance_registry::REGISTRY_DIR_NAME);
        let path = registry.join(&name);
        let path = if path.as_os_str().len() <= MAX_SOCKET_PATH {
            fs::create_dir_all(&registry)?;
            path
        } else {
            let digest = failure_bundle::stable_hash8(&path.display().to_string());
            env::temp_dir().join(format!("edev-{digest}-{sequence}.sock"))
        };
        match fs::remove_file(&path) {
            Err(error) if error.kind() != std_io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        Ok(Self { path })
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _removed = fs::remove_file(&self.path);
    }
}

/// Connect to a newly launched app, including time spent in an app build
/// command.
// Non-macOS startup probes need mutable access for `Child::try_wait`; macOS
// probes the supervisor task through the same cross-platform call boundary.
#[cfg_attr(target_os = "macos", allow(clippy::needless_pass_by_ref_mut))]
pub async fn connect_app_client(
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
        match connect_endpoint(&mut client, endpoint).await {
            Ok(_) => return Ok(client),
            Err(error) => {
                if process_lifecycle::spawned_process_exited(process)
                    .map_err(McpError::InternalError)?
                {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(connect_timeout_error(endpoint, APP_CONNECT_TIMEOUT, &error));
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
}

/// Name the socket and the timeout of a start that never connected.
fn connect_timeout_error(endpoint: &str, timeout: Duration, error: &McpError) -> McpError {
    McpError::Transport(format!(
        "the app did not open its socket {endpoint} within {} seconds; last connect error: {error}",
        timeout.as_secs()
    ))
}

/// Connect a client to the socket of an app.
#[cfg(unix)]
pub async fn connect_endpoint(
    client: &mut tmcp::Client<()>,
    endpoint: &str,
) -> Result<InitializeResult, McpError> {
    client.connect_unix(endpoint).await
}

/// Report that the app endpoint needs a Unix domain socket.
#[cfg(not(unix))]
pub async fn connect_endpoint(
    _client: &mut tmcp::Client<()>,
    _endpoint: &str,
) -> Result<InitializeResult, McpError> {
    Err(McpError::InvalidConfiguration(
        "the eguidev app endpoint needs a Unix domain socket".to_string(),
    ))
}

/// Combine the handshake error with any captured stderr for diagnostics.
pub fn format_startup_output(error: impl Display, output: &str) -> String {
    let output = output.trim_end();
    if output.is_empty() {
        error.to_string()
    } else {
        format!("{error}\n{output}")
    }
}

/// Drain buffered stderr output into a string.
pub async fn drain_stderr(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    let mut output = String::new();
    if let Ok(mut data) = buffer.lock() {
        output = String::from_utf8_lossy(&data).to_string();
        data.clear();
    }
    output
}

/// Return buffered process output without clearing it.
pub fn snapshot_output(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    buffer.lock().map_or_else(
        |_| String::new(),
        |data| String::from_utf8_lossy(&data).to_string(),
    )
}

/// Append bytes to a tail-capped process-output buffer.
pub fn append_tail_capped(buffer: &Arc<Mutex<Vec<u8>>>, bytes: &[u8]) {
    let Ok(mut data) = buffer.lock() else {
        return;
    };
    data.extend_from_slice(bytes);
    if data.len() > APP_LOG_TAIL_LIMIT + APP_LOG_TAIL_TRIM_SLACK {
        let drop_len = data.len() - APP_LOG_TAIL_LIMIT;
        data.drain(..drop_len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_timeout_error_names_socket_and_timeout() {
        let error = connect_timeout_error(
            "/tmp/app.sock",
            Duration::from_secs(120),
            &McpError::Transport("IO error: No such file or directory".to_string()),
        );
        let text = error.to_string();
        assert!(text.contains("/tmp/app.sock"), "{text}");
        assert!(text.contains("120 seconds"), "{text}");
        assert!(text.contains("No such file or directory"), "{text}");
    }
}
