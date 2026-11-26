//! The macOS supervisor, which owns the app process group of one launch.

use super::*;

/// Byte sent before a deliberate outer-launcher shutdown.
pub(super) const NORMAL_SHUTDOWN_MARKER: u8 = b'N';

/// Per-launch ownership writer.
pub type OwnershipWriter = OwnedFd;

/// Return whether a newly spawned app boundary exited before its MCP handshake.
pub fn spawned_process_exited(process: &SpawnedProcess) -> Result<bool, String> {
    Ok(process
        .supervisor_exit_task
        .as_ref()
        .is_some_and(JoinHandle::is_finished))
}

#[derive(Debug, Serialize, Deserialize)]
/// Serialized launch data passed only to the hidden supervisor child.
pub(super) struct SupervisorConfig {
    /// Working directory for the app command.
    pub(super) working_dir: PathBuf,
    /// Full app argv.
    pub(super) command: Vec<String>,
    /// Extra app environment.
    pub(super) env: BTreeMap<String, String>,
    /// Outer launcher PID.
    pub(super) launcher_pid: u32,
    /// Start time of the outer launcher.
    pub(super) launcher_start_time: Option<instance_registry::ProcessStartTime>,
    /// Collision-safe outer launcher identity.
    pub(super) launcher_token: String,
    /// Collision-safe app launch identity.
    pub(super) launch_id: String,
    /// Exact app record path.
    pub(super) app_record_path: PathBuf,
    /// Inherited read endpoint for the outer ownership pipe.
    pub(super) ownership_fd: RawFd,
}

/// App resources owned by one running supervisor.
pub(super) struct ManagedApp {
    /// Direct app child reaped by the supervisor.
    pub(super) child: Child,
    /// Process group containing the app and all descendants.
    pub(super) process_group_id: i32,
    /// Exact metadata written for this app launch.
    pub(super) record: AppRecord,
    /// App stdout used by the app-to-outer relay.
    pub(super) stdout: ChildStdout,
    /// App stderr used by the app-to-outer relay.
    pub(super) stderr: ChildStderr,
}

/// Read supervisor configuration from the private inherited pipe.
pub(super) async fn read_supervisor_config(fd: RawFd) -> Result<SupervisorConfig, String> {
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_cloexec(fd.as_raw_fd(), true)
        .map_err(|error| format!("protect supervisor config pipe: {error}"))?;
    let file = fs::File::from(fd);
    let payload = spawn_blocking(move || {
        let mut file = file;
        let mut payload = Vec::new();
        file.read_to_end(&mut payload)
            .map_err(|error| format!("read supervisor configuration: {error}"))?;
        Ok::<_, String>(payload)
    })
    .await
    .map_err(|error| format!("supervisor config read task failed: {error}"))??;
    serde_json::from_slice(&payload)
        .map_err(|error| format!("decode supervisor configuration: {error}"))
}

/// Spawn the supervisor and retain only its outer-owned lifecycle endpoints.
pub(super) async fn spawn_supervised(
    config: &LaunchConfig,
    log_state: LogState,
) -> Result<SpawnedProcess, String> {
    let executable =
        env::current_exe().map_err(|error| format!("resolve edev executable: {error}"))?;
    spawn_supervised_with_executable(config, log_state, executable).await
}

/// Spawn one supervisor using an explicit executable.
pub(super) async fn spawn_supervised_with_executable(
    config: &LaunchConfig,
    log_state: LogState,
    executable: PathBuf,
) -> Result<SpawnedProcess, String> {
    let launch = app_launch_for(&config.cwd, process::id())
        .map_err(|error| format!("resolve launcher identity: {error}"))?;
    let (ownership_reader, ownership_writer) =
        create_inherited_pipe().map_err(|error| format!("ownership pipe: {error}"))?;
    let (config_reader, config_writer) =
        create_inherited_pipe().map_err(|error| format!("config pipe: {error}"))?;
    let supervisor_config = SupervisorConfig {
        working_dir: config.cwd.clone(),
        command: config.command.clone(),
        env: config.env.clone(),
        launcher_pid: process::id(),
        launcher_start_time: launch.launcher_start_time,
        launcher_token: launch.launcher_token.clone(),
        launch_id: launch.launch_id.clone(),
        app_record_path: launch.entry_path.clone(),
        ownership_fd: ownership_reader.as_raw_fd(),
    };
    let payload = serde_json::to_vec(&supervisor_config)
        .map_err(|error| format!("serialize supervisor configuration: {error}"))?;
    let mut command = Command::new(executable);
    let config_fd_arg = config_reader.as_raw_fd().to_string();
    let ownership_fd = ownership_reader.as_raw_fd();
    let config_fd = config_reader.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            set_cloexec(ownership_fd, false)?;
            set_cloexec(config_fd, false)
        });
    }
    command
        .args(["__edev_supervisor", "--config-fd", &config_fd_arg])
        .current_dir(&config.cwd)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            drop(ownership_reader);
            drop(ownership_writer);
            drop(config_reader);
            drop(config_writer);
            return Err(format!("failed to spawn edev supervisor: {error}"));
        }
    };
    drop(ownership_reader);
    drop(config_reader);
    let supervisor_pid = match child.id() {
        Some(pid) => pid,
        None => {
            terminate_unobserved_supervisor(&mut child).await;
            drop(ownership_writer);
            drop(config_writer);
            return Err("supervisor did not expose a PID".to_string());
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_unobserved_supervisor(&mut child).await;
            drop(ownership_writer);
            drop(config_writer);
            return Err("failed to capture supervisor stdout".to_string());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_unobserved_supervisor(&mut child).await;
            drop(ownership_writer);
            drop(config_writer);
            return Err("failed to capture supervisor stderr".to_string());
        }
    };
    let record_path = launch.entry_path.clone();
    let supervisor_exit_task =
        tokio::spawn(async move { monitor_supervisor_exit(child, record_path).await });
    let mut config_writer = fs::File::from(config_writer);
    if let Err(error) = config_writer.write_all(&payload) {
        drop(config_writer);
        drop(ownership_writer);
        terminate_supervisor(Some(supervisor_pid), &log_state);
        let _result = supervisor_exit_task.await;
        return Err(format!("write supervisor configuration: {error}"));
    }
    drop(config_writer);
    log_state.record_line(&format!("edev: supervisor {supervisor_pid} spawned"));

    Ok(SpawnedProcess {
        stdout: Some(stdout),
        stderr: Some(stderr),
        child: None,
        process_group_id: None,
        supervisor_pid: Some(supervisor_pid),
        supervisor_exit_task: Some(supervisor_exit_task),
        ownership_writer: Some(ownership_writer),
        app_launch: Some(launch),
    })
}

/// Kill and reap a supervisor before its lifecycle task is available.
pub(super) async fn terminate_unobserved_supervisor(supervisor: &mut Child) {
    let _start_kill_result = supervisor.start_kill();
    let _wait_result = supervisor.wait().await;
}

/// Wait for the supervisor and recover an exact record if it died unexpectedly.
pub(super) async fn monitor_supervisor_exit(
    mut child: Child,
    record_path: PathBuf,
) -> io::Result<ExitStatus> {
    let status = child.wait().await?;
    let Some(record) = read_app_record(&record_path)? else {
        return Ok(status);
    };
    recover_after_supervisor_exit(&record_path, &record).await?;
    Ok(status)
}

/// Recover a group left behind by an abruptly dead supervisor.
pub(super) async fn recover_after_supervisor_exit(
    record_path: &Path,
    record: &AppRecord,
) -> io::Result<()> {
    let observer = ProcessGroupObserver::new()?;
    if instance_registry::recorded_app_group_is_current(record)
        && !recording::live_process_group_members(record.app_process_group_id).is_empty()
    {
        observer.watch_group(record.app_process_group_id)?;
        if instance_registry::recorded_app_group_is_current(record) {
            terminate_process_group_without_logging(Some(record.app_process_group_id));
            observer
                .wait_until_group_empty(record.app_process_group_id)
                .await?;
        }
    }
    instance_registry::remove_app_record_if_matches(record_path, record)?;
    let _removed_launcher = instance_registry::remove_launcher_record_if_dead(
        &instance_registry::launcher_record_path(&record.working_dir, record.launcher_pid),
        record,
    )?;
    Ok(())
}

/// Spawn the app group, capture stdio, and install its exact registry record.
pub(super) async fn spawn_managed_app(config: &SupervisorConfig) -> Result<ManagedApp, String> {
    let executable = config
        .command
        .first()
        .ok_or_else(|| "app command is empty".to_string())?;
    let mut command = if Path::new(executable).file_stem().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("cargo" | "ncode" | "edev" | "canopyctl")
        )
    }) {
        Command::from(cargo_env::command(executable))
    } else {
        Command::new(executable)
    };
    command
        .args(&config.command[1..])
        .current_dir(&config.working_dir)
        .envs(&config.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_child_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn managed app: {error}"))?;
    let process_group_id = match child.id().and_then(|pid| i32::try_from(pid).ok()) {
        Some(pid) => pid,
        None => {
            terminate_unobserved_app(&mut child, None).await;
            return Err("managed app did not expose a supported PID".to_string());
        }
    };
    let streams = (child.stdout.take(), child.stderr.take());
    let (Some(stdout), Some(stderr)) = streams else {
        terminate_unobserved_app(&mut child, Some(process_group_id)).await;
        return Err("failed to capture managed app stdio".to_string());
    };
    let launch = AppLaunch {
        launch_id: config.launch_id.clone(),
        entry_path: config.app_record_path.clone(),
        launcher_pid: config.launcher_pid,
        launcher_start_time: config.launcher_start_time,
        launcher_token: config.launcher_token.clone(),
        working_dir: config.working_dir.clone(),
    };
    let mut record = launch.record(
        process::id(),
        process_group_id,
        instance_registry::process_start_time(process_group_id),
    );
    record.mcp_socket = config
        .env
        .get(eguidev_runtime::MCP_ADDR_ENV)
        .map(PathBuf::from);
    if let Err(error) = instance_registry::write_app_record(&config.app_record_path, &record) {
        terminate_unobserved_app(&mut child, Some(process_group_id)).await;
        return Err(format!("write app record: {error}"));
    }
    Ok(ManagedApp {
        child,
        process_group_id,
        record,
        stdout,
        stderr,
    })
}

/// Execute the app command inside a new group and relay stdout and stderr.
pub(super) async fn run_supervisor(config: SupervisorConfig) -> Result<(), String> {
    let ownership = unsafe { OwnedFd::from_raw_fd(config.ownership_fd) };
    set_nonblocking(ownership.as_raw_fd())
        .map_err(|error| format!("configure ownership pipe: {error}"))?;
    set_cloexec(ownership.as_raw_fd(), true)
        .map_err(|error| format!("protect ownership pipe: {error}"))?;
    // SAFETY: the `OwnedFd` keeps the pipe open for as long as the `AsyncFd`
    // owns it, and always returns the same descriptor.
    let ownership = unsafe { AsyncFd::register(ownership) }
        .map_err(|error| format!("register ownership pipe: {}", error.into_parts().1))?;

    let managed = spawn_managed_app(&config).await?;
    let ManagedApp {
        child: mut app,
        process_group_id: app_process_group_id,
        record,
        stdout: app_stdout,
        stderr: app_stderr,
    } = managed;

    let observer = match ProcessGroupObserver::new() {
        Ok(observer) => observer,
        Err(error) => {
            terminate_unobserved_app(&mut app, Some(app_process_group_id)).await;
            let _remove_result =
                instance_registry::remove_app_record_if_matches(&config.app_record_path, &record);
            return Err(format!("observe app process group: {error}"));
        }
    };
    if let Err(error) = observer.watch_group(app_process_group_id) {
        terminate_unobserved_app(&mut app, Some(app_process_group_id)).await;
        let _remove_result =
            instance_registry::remove_app_record_if_matches(&config.app_record_path, &record);
        return Err(format!("watch app process group: {error}"));
    }

    let mut child_wait_task = tokio::spawn(async move { app.wait().await });
    let mut relays = RelayTasks::new(app_stdout, app_stderr);
    let trigger = relays
        .wait_for_trigger(&ownership, &mut child_wait_task)
        .await;

    let app_exited_naturally = matches!(&trigger, SupervisorTrigger::App(_));
    let owner_normal = matches!(&trigger, SupervisorTrigger::Owner(Ok(OwnerExit::Normal)));
    let owner_abnormal = matches!(
        &trigger,
        SupervisorTrigger::Owner(Ok(OwnerExit::Abnormal)) | SupervisorTrigger::Owner(Err(_))
    );
    let (app_status, trigger_error) = match trigger {
        SupervisorTrigger::App(result) => match result {
            Ok(status) => (Some(status), None),
            Err(error) => (None, Some(error)),
        },
        SupervisorTrigger::Owner(result) => {
            terminate_process_group_without_logging(Some(app_process_group_id));
            let child_result = wait_for_child(&mut child_wait_task).await;
            (
                child_result.ok(),
                result
                    .err()
                    .map(|error| format!("ownership channel failed: {error}")),
            )
        }
        SupervisorTrigger::Relay(error) => {
            terminate_process_group_without_logging(Some(app_process_group_id));
            let child_result = wait_for_child(&mut child_wait_task).await;
            (child_result.ok(), Some(error))
        }
    };

    if !recording::live_process_group_members(app_process_group_id).is_empty() {
        terminate_process_group_without_logging(Some(app_process_group_id));
    }
    let group_exit = observer
        .wait_until_group_empty(app_process_group_id)
        .await
        .map_err(|error| format!("confirm app process-group exit: {error}"));
    let record_removal = if group_exit.is_ok() {
        Some(
            instance_registry::remove_app_record_if_matches(&config.app_record_path, &record)
                .map(|_| ())
                .map_err(|error| format!("remove app record: {error}")),
        )
    } else {
        None
    };
    let launcher_removal = if owner_abnormal && group_exit.is_ok() {
        Some(
            instance_registry::remove_launcher_record_if_dead(
                &instance_registry::launcher_record_path(&record.working_dir, record.launcher_pid),
                &record,
            )
            .map(|_| ())
            .map_err(|error| format!("remove launcher record: {error}")),
        )
    } else {
        None
    };

    relays.finish(app_exited_naturally).await;

    group_exit?;
    if let Some(Err(error)) = record_removal {
        return Err(error);
    }
    if let Some(Err(error)) = launcher_removal {
        return Err(error);
    }
    if let Some(error) = trigger_error {
        return Err(error);
    }
    if owner_normal {
        return Ok(());
    }
    let app_status =
        app_status.ok_or_else(|| "managed app exit status was unavailable".to_string())?;
    if app_status.success() {
        Ok(())
    } else {
        Err(format!("managed app exited with {app_status}"))
    }
}

/// Kill and reap an app before its process-group observer is available.
pub(super) async fn terminate_unobserved_app(app: &mut Child, process_group_id: Option<i32>) {
    if process_group_id.is_some() {
        terminate_process_group_without_logging(process_group_id);
    } else {
        let _start_kill_result = app.start_kill();
    }
    let _result = app.wait().await;
}

/// Relay one asynchronous byte stream.
pub(super) async fn relay<R, W>(mut reader: R, mut writer: W) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    copy(&mut reader, &mut writer).await
}

/// Decode one relay task result.
pub(super) fn relay_join_result(result: Result<io::Result<u64>, JoinError>) -> Result<(), String> {
    result
        .map_err(|error| format!("relay task failed: {error}"))?
        .map(|_| ())
        .map_err(|error| format!("stdio relay failed: {error}"))
}

/// Decode and await the direct app child wait task.
pub(super) async fn wait_for_child(
    task: &mut JoinHandle<io::Result<ExitStatus>>,
) -> Result<ExitStatus, String> {
    task.await
        .map_err(|error| format!("app wait task failed: {error}"))?
        .map_err(|error| format!("app wait failed: {error}"))
}

/// Await one relay task when it is still active.
pub(super) async fn await_relay(task: &mut JoinHandle<io::Result<u64>>, active: &mut bool) {
    if *active {
        let _result = task.await;
        *active = false;
    }
}

/// Stop relay tasks after a group-level cleanup path.
pub(super) async fn abort_relays(
    stdout: &mut JoinHandle<io::Result<u64>>,
    stderr: &mut JoinHandle<io::Result<u64>>,
    stdout_active: bool,
    stderr_active: bool,
) {
    if stdout_active {
        stdout.abort();
        let _result = stdout.await;
    }
    if stderr_active {
        stderr.abort();
        let _result = stderr.await;
    }
}

/// Stdio relay tasks and their completion state for one supervisor.
pub(super) struct RelayTasks {
    /// App stdout to outer stdout relay.
    pub(super) stdout: JoinHandle<io::Result<u64>>,
    /// App stderr to outer stderr relay.
    pub(super) stderr: JoinHandle<io::Result<u64>>,
    /// Whether the stdout relay is still pending.
    pub(super) stdout_active: bool,
    /// Whether the stderr relay is still pending.
    pub(super) stderr_active: bool,
}

impl RelayTasks {
    /// Start stdout and stderr supervisor relay tasks.
    pub(super) fn new(stdout_reader: ChildStdout, stderr_reader: ChildStderr) -> Self {
        Self {
            stdout: tokio::spawn(relay(stdout_reader, stdout())),
            stderr: tokio::spawn(relay(stderr_reader, stderr())),
            stdout_active: true,
            stderr_active: true,
        }
    }

    /// Wait until ownership, the app child, or a relay requires cleanup.
    pub(super) async fn wait_for_trigger(
        &mut self,
        ownership: &AsyncFd<OwnedFd>,
        child: &mut JoinHandle<io::Result<ExitStatus>>,
    ) -> SupervisorTrigger {
        let owner = wait_for_owner_eof(ownership);
        tokio::pin!(owner);
        loop {
            tokio::select! {
                owner_result = &mut owner => {
                    return SupervisorTrigger::Owner(owner_result);
                }
                child_result = &mut *child => {
                    return SupervisorTrigger::App(child_result
                        .map_err(|error| format!("app wait task failed: {error}"))
                        .and_then(|result| {
                            result.map_err(|error| format!("app wait failed: {error}"))
                        }));
                }
                relay_result = &mut self.stdout, if self.stdout_active => {
                    self.stdout_active = false;
                    if let Err(error) = relay_join_result(relay_result) {
                        return SupervisorTrigger::Relay(error);
                    }
                }
                relay_result = &mut self.stderr, if self.stderr_active => {
                    self.stderr_active = false;
                    if let Err(error) = relay_join_result(relay_result) {
                        return SupervisorTrigger::Relay(error);
                    }
                }
            }
        }
    }

    /// Drain natural output or abort all remaining relays after forced cleanup.
    pub(super) async fn finish(&mut self, app_exited_naturally: bool) {
        if app_exited_naturally {
            await_relay(&mut self.stdout, &mut self.stdout_active).await;
            await_relay(&mut self.stderr, &mut self.stderr_active).await;
        } else {
            abort_relays(
                &mut self.stdout,
                &mut self.stderr,
                self.stdout_active,
                self.stderr_active,
            )
            .await;
        }
    }
}

/// Event emitted by the supervisor's owner, child, or relay paths.
pub(super) enum SupervisorTrigger {
    /// Outer ownership writer reached EOF or failed.
    Owner(io::Result<OwnerExit>),
    /// Direct app child exited.
    App(Result<ExitStatus, String>),
    /// A stdio relay failed.
    Relay(String),
}

/// Classification of an ownership channel close.
#[derive(Debug, Clone, Copy)]
pub(super) enum OwnerExit {
    /// The outer launcher explicitly marked a deliberate shutdown.
    Normal,
    /// The channel closed without the deliberate-shutdown marker.
    Abnormal,
}

/// Wait for the normal marker or bare EOF on the dedicated owner pipe.
pub(super) async fn wait_for_owner_eof(owner: &AsyncFd<OwnedFd>) -> io::Result<OwnerExit> {
    let mut saw_marker = false;
    let mut saw_unexpected_data = false;
    loop {
        let mut readiness = owner.readable().await?;
        match readiness.try_io(|inner| read_owner_fd(inner.get_ref().as_raw_fd())) {
            Ok(result) => match result? {
                None => {
                    return Ok(if saw_marker && !saw_unexpected_data {
                        OwnerExit::Normal
                    } else {
                        OwnerExit::Abnormal
                    });
                }
                Some(byte) if byte == NORMAL_SHUTDOWN_MARKER && !saw_marker => {
                    saw_marker = true;
                }
                Some(_) => saw_unexpected_data = true,
            },
            Err(_would_block) => {}
        }
    }
}

/// Read one owner message byte from the pipe without blocking.
pub(super) fn read_owner_fd(fd: RawFd) -> io::Result<Option<u8>> {
    let mut byte = [0_u8; 1];
    loop {
        let result = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), byte.len()) };
        if result == 0 {
            return Ok(None);
        }
        if result == 1 {
            return Ok(Some(byte[0]));
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner pipe returned more than one byte",
        ));
    }
}

/// Write the deliberate-shutdown marker before closing the owner endpoint.
pub(super) fn write_normal_shutdown_marker(writer: &OwnedFd) -> io::Result<()> {
    loop {
        let result = unsafe {
            libc::write(
                writer.as_raw_fd(),
                (&NORMAL_SHUTDOWN_MARKER as *const u8).cast(),
                1,
            )
        };
        if result == 1 {
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "owner pipe accepted no shutdown marker",
            ));
        }
        return Err(io::Error::last_os_error());
    }
}

/// Create a pipe whose ends remain close-on-exec until a child clears its
/// reader.
pub fn create_inherited_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    // macOS has no atomic pipe-with-CLOEXEC API, so protect both ends before
    // returning them.
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    set_cloexec(writer.as_raw_fd(), true)?;
    set_cloexec(reader.as_raw_fd(), true)?;
    Ok((reader, writer))
}

/// Set or clear close-on-exec for one file descriptor.
pub fn set_cloexec(fd: RawFd, enabled: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Configure one inherited pipe for readiness-based EOF detection.
pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Observe process exits using kqueue rather than polling.
pub struct ProcessGroupObserver {
    /// Tokio-registered kqueue descriptor.
    pub(super) queue: AsyncFd<OwnedFd>,
}

impl ProcessGroupObserver {
    /// Create an empty process-exit observer.
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: the `OwnedFd` keeps the queue open for as long as the
        // `AsyncFd` owns it, and always returns the same descriptor.
        let queue = unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }
            .map_err(|error| error.into_parts().1)?;
        Ok(Self { queue })
    }

    /// Register current members of one process group for exit notifications.
    pub(super) fn watch_group(&self, process_group_id: i32) -> io::Result<()> {
        for pid in recording::live_process_group_members(process_group_id) {
            let _exists = self.watch_pid(pid)?;
        }
        Ok(())
    }

    /// Register one process for a one-shot NOTE_EXIT event.
    pub fn watch_pid(&self, pid: i32) -> io::Result<bool> {
        if pid <= 0 {
            return Ok(false);
        }
        let event = libc::kevent {
            ident: usize::try_from(pid).unwrap_or_default(),
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: ptr::null_mut(),
        };
        let result = unsafe {
            libc::kevent(
                self.queue.get_ref().as_raw_fd(),
                &event,
                1,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ESRCH)) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    /// Wait for all observed process-group members to disappear.
    pub(super) async fn wait_until_group_empty(&self, process_group_id: i32) -> io::Result<()> {
        self.wait_until_pids_exit(|| recording::live_process_group_members(process_group_id))
            .await
    }

    /// Wait until `current_pids` reports no remaining processes.
    pub async fn wait_until_pids_exit<F>(&self, current_pids: F) -> io::Result<()>
    where
        F: Fn() -> Vec<i32>,
    {
        loop {
            let members = current_pids();
            if members.is_empty() {
                return Ok(());
            }
            let mut member_vanished = false;
            for pid in members {
                member_vanished |= !self.watch_pid(pid)?;
            }
            if member_vanished {
                continue;
            }
            self.next_event().await?;
        }
    }

    /// Wait for one kqueue event.
    pub async fn next_event(&self) -> io::Result<()> {
        loop {
            let mut readiness = self.queue.readable().await?;
            match readiness.try_io(|inner| read_kqueue(inner.get_ref().as_raw_fd())) {
                Ok(result) => {
                    result?;
                    return Ok(());
                }
                Err(_would_block) => {}
            }
        }
    }
}

/// Read and consume one or more pending kqueue events.
pub(super) fn read_kqueue(fd: RawFd) -> io::Result<usize> {
    let mut events = [unsafe { mem::zeroed::<libc::kevent>() }; 32];
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe {
        libc::kevent(
            fd,
            ptr::null(),
            0,
            events.as_mut_ptr(),
            i32::try_from(events.len()).unwrap_or(i32::MAX),
            &timeout,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result == 0 {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    Ok(result as usize)
}
