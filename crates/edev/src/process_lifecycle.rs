//! Private platform launch and cleanup abstraction for edev.

#[cfg(target_os = "macos")]
use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    path::PathBuf,
    ptr,
};
use std::{
    ffi::{OsStr, OsString},
    io,
    process::{self, ExitStatus, Stdio},
};

#[cfg(target_os = "macos")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use tokio::{
    io::{AsyncRead, AsyncWrite, Interest, copy, stderr, stdout, unix::AsyncFd},
    task::{JoinError, spawn_blocking},
};
use tokio::{
    process::{Child, ChildStderr, ChildStdout, Command},
    task::JoinHandle,
};

use super::{LaunchConfig, LogState, instance_registry::AppLaunch};
#[cfg(target_os = "macos")]
use super::{
    instance_registry::{self, AppRecord, app_launch_for, read_app_record_for_path},
    recording,
};

#[cfg(target_os = "macos")]
/// Byte sent before a deliberate outer-launcher shutdown.
const NORMAL_SHUTDOWN_MARKER: u8 = b'N';

/// Process resources returned by one platform launch.
pub struct SpawnedProcess {
    /// App or supervisor stdout, captured by the outer launcher.
    pub stdout: Option<ChildStdout>,
    /// App or supervisor stderr, captured by the outer launcher.
    pub stderr: Option<ChildStderr>,
    /// Direct app child on platforms without a supervisor.
    pub child: Option<Child>,
    /// App process group id, excluding the supervisor.
    pub process_group_id: Option<i32>,
    /// Supervisor PID when the platform launch uses one.
    pub supervisor_pid: Option<u32>,
    /// Task that owns and reaps the supervisor child.
    pub supervisor_exit_task: Option<JoinHandle<io::Result<ExitStatus>>>,
    /// Write endpoint held only by the outer AppProcess.
    pub ownership_writer: Option<OwnershipWriter>,
    /// Exact launch identity used by the supervisor registry record.
    pub app_launch: Option<AppLaunch>,
}

/// Per-launch ownership writer.
#[cfg(target_os = "macos")]
pub type OwnershipWriter = OwnedFd;

/// No ownership endpoint is needed on platforms with parent-death signaling.
#[cfg(not(target_os = "macos"))]
pub type OwnershipWriter = ();

/// Start one managed app through the platform-specific process boundary.
pub async fn spawn(config: &LaunchConfig, log_state: LogState) -> Result<SpawnedProcess, String> {
    #[cfg(target_os = "macos")]
    {
        spawn_supervised(config, log_state).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        spawn_direct(config, log_state).await
    }
}

/// Shut down a process whose MCP handshake failed before AppProcess was built.
pub async fn shutdown_spawned(mut process: SpawnedProcess, log_state: &LogState) {
    #[cfg(target_os = "macos")]
    {
        close_ownership_writer(&mut process.ownership_writer, log_state);
        if let Some(task) = process.supervisor_exit_task.take() {
            let _result = task.await;
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        terminate_process_group(process.process_group_id, log_state);
        if let Some(mut child) = process.child.take() {
            let _result = child.wait().await;
        }
    }
}

/// Return whether a newly spawned app boundary exited before its MCP handshake.
#[cfg(target_os = "macos")]
pub fn spawned_process_exited(process: &SpawnedProcess) -> Result<bool, String> {
    Ok(process
        .supervisor_exit_task
        .as_ref()
        .is_some_and(JoinHandle::is_finished))
}

/// Return whether a newly spawned app boundary exited before its MCP handshake.
#[cfg(not(target_os = "macos"))]
pub fn spawned_process_exited(process: &mut SpawnedProcess) -> Result<bool, String> {
    process
        .child
        .as_mut()
        .map(Child::try_wait)
        .transpose()
        .map(|status| status.flatten().is_some())
        .map_err(|error| format!("inspect app process during startup: {error}"))
}

/// Close the outer ownership endpoint, marking a deliberate shutdown first.
pub fn close_ownership_writer(writer: &mut Option<OwnershipWriter>, log_state: &LogState) {
    #[cfg(target_os = "macos")]
    {
        let Some(writer) = writer.take() else {
            return;
        };
        if let Err(error) = write_normal_shutdown_marker(&writer) {
            log_state.record_line(&format!("edev: failed to mark normal shutdown: {error}"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = log_state;
        writer.take();
    }
}

/// Fetch the process group id from a spawned direct child.
#[cfg(not(target_os = "macos"))]
pub fn process_group_id(child: &Child) -> Option<i32> {
    child.id().and_then(|id| i32::try_from(id).ok())
}

/// Configure child process behavior for cleanup and termination.
pub fn configure_child_process(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(all(unix, target_os = "linux"))]
    {
        let parent_pid = libc::pid_t::try_from(process::id()).ok();
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some(parent_pid) = parent_pid
                    && libc::getppid() != parent_pid
                {
                    return Err(io::Error::other("parent process changed before exec"));
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

/// Terminate an app process group for a last-resort cleanup path.
pub fn terminate_process_group(process_group_id: Option<i32>, log_state: &LogState) {
    #[cfg(unix)]
    {
        if let Some(pgid) = process_group_id
            && let Err(error) = kill_process_group(pgid)
        {
            log_state.record_line(&format!(
                "edev: failed to kill process group {pgid}: {error}"
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (process_group_id, log_state);
    }
}

/// Send SIGKILL to one process group, tolerating an already-empty group.
#[cfg(unix)]
pub fn kill_process_group(process_group_id: i32) -> io::Result<()> {
    if unsafe { libc::killpg(process_group_id, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Kill one supervisor process for the last-resort outer teardown path.
pub fn terminate_supervisor(supervisor_pid: Option<u32>, log_state: &LogState) {
    #[cfg(unix)]
    {
        if let Some(pid) = supervisor_pid.and_then(|pid| i32::try_from(pid).ok()) {
            let result = unsafe { libc::kill(pid, libc::SIGKILL) };
            if result != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    log_state
                        .record_line(&format!("edev: failed to kill supervisor {pid}: {error}"));
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (supervisor_pid, log_state);
    }
}

/// Detect the hidden supervisor invocation without exposing it through clap help.
pub fn is_supervisor_invocation(args: &[OsString]) -> bool {
    args.get(1)
        .is_some_and(|arg| arg == OsStr::new("__edev_supervisor"))
}

/// Run the hidden supervisor entry point.
pub async fn run_hidden_supervisor(args: &[OsString]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let fd = args
            .get(3)
            .filter(|_| args.get(2).is_some_and(|flag| flag == "--config-fd"))
            .ok_or_else(|| "supervisor requires --config-fd <fd>".to_string())?;
        let fd = fd
            .to_str()
            .ok_or_else(|| "supervisor config fd is not UTF-8".to_string())?
            .parse::<RawFd>()
            .map_err(|error| format!("invalid supervisor config fd: {error}"))?;
        let config = read_supervisor_config(fd).await?;
        run_supervisor(config).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err("the edev supervisor is only available on macOS".to_string())
    }
}

/// Spawn the current app command directly on platforms with parent-death support.
#[cfg(not(target_os = "macos"))]
async fn spawn_direct(
    config: &LaunchConfig,
    log_state: LogState,
) -> Result<SpawnedProcess, String> {
    log_state.record_line("edev: spawning app");
    let mut command = config.app_command();
    command.kill_on_drop(true);
    configure_child_process(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn app command: {error}"))?;
    log_state.record_line("edev: app process spawned");
    let process_group_id = process_group_id(&child);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture app stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture app stderr".to_string())?;

    Ok(SpawnedProcess {
        stdout: Some(stdout),
        stderr: Some(stderr),
        child: Some(child),
        process_group_id,
        supervisor_pid: None,
        supervisor_exit_task: None,
        ownership_writer: None,
        app_launch: None,
    })
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize, Deserialize)]
/// Serialized launch data passed only to the hidden supervisor child.
struct SupervisorConfig {
    /// Working directory for the app command.
    working_dir: PathBuf,
    /// Full app argv.
    command: Vec<String>,
    /// Extra app environment.
    env: BTreeMap<String, String>,
    /// Outer launcher PID.
    launcher_pid: u32,
    /// Collision-safe outer launcher identity.
    launcher_token: String,
    /// Collision-safe app launch identity.
    launch_id: String,
    /// Exact app record path.
    app_record_path: PathBuf,
    /// Inherited read endpoint for the outer ownership pipe.
    ownership_fd: RawFd,
}

#[cfg(target_os = "macos")]
/// App resources owned by one running supervisor.
struct ManagedApp {
    /// Direct app child reaped by the supervisor.
    child: Child,
    /// Process group containing the app and all descendants.
    process_group_id: i32,
    /// Exact metadata written for this app launch.
    record: AppRecord,
    /// App stdout used by the app-to-outer relay.
    stdout: ChildStdout,
    /// App stderr used by the app-to-outer relay.
    stderr: ChildStderr,
}

#[cfg(target_os = "macos")]
/// Read supervisor configuration from the private inherited pipe.
async fn read_supervisor_config(fd: RawFd) -> Result<SupervisorConfig, String> {
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

#[cfg(target_os = "macos")]
/// Spawn the supervisor and retain only its outer-owned lifecycle endpoints.
async fn spawn_supervised(
    config: &LaunchConfig,
    log_state: LogState,
) -> Result<SpawnedProcess, String> {
    let executable =
        env::current_exe().map_err(|error| format!("resolve edev executable: {error}"))?;
    spawn_supervised_with_executable(config, log_state, executable).await
}

#[cfg(target_os = "macos")]
/// Spawn one supervisor using an explicit executable.
async fn spawn_supervised_with_executable(
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

#[cfg(target_os = "macos")]
/// Kill and reap a supervisor before its lifecycle task is available.
async fn terminate_unobserved_supervisor(supervisor: &mut Child) {
    let _start_kill_result = supervisor.start_kill();
    let _wait_result = supervisor.wait().await;
}

#[cfg(target_os = "macos")]
/// Wait for the supervisor and recover an exact record if it died unexpectedly.
async fn monitor_supervisor_exit(mut child: Child, record_path: PathBuf) -> io::Result<ExitStatus> {
    let status = child.wait().await?;
    let Some(record) = read_app_record_for_path(&record_path)? else {
        return Ok(status);
    };
    recover_after_supervisor_exit(&record_path, &record).await?;
    Ok(status)
}

#[cfg(target_os = "macos")]
/// Recover a group left behind by an abruptly dead supervisor.
async fn recover_after_supervisor_exit(
    record_path: &std::path::Path,
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

#[cfg(target_os = "macos")]
/// Spawn the app group, capture stdio, and install its exact registry record.
async fn spawn_managed_app(config: &SupervisorConfig) -> Result<ManagedApp, String> {
    let executable = config
        .command
        .first()
        .ok_or_else(|| "app command is empty".to_string())?;
    let mut command = Command::new(executable);
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
        launcher_token: config.launcher_token.clone(),
        working_dir: config.working_dir.clone(),
    };
    let record = launch.record(
        process::id(),
        process_group_id,
        instance_registry::process_start_time(process_group_id),
    );
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

#[cfg(target_os = "macos")]
/// Execute the app command inside a new group and relay stdout and stderr.
async fn run_supervisor(config: SupervisorConfig) -> Result<(), String> {
    let ownership = unsafe { OwnedFd::from_raw_fd(config.ownership_fd) };
    set_nonblocking(ownership.as_raw_fd())
        .map_err(|error| format!("configure ownership pipe: {error}"))?;
    set_cloexec(ownership.as_raw_fd(), true)
        .map_err(|error| format!("protect ownership pipe: {error}"))?;
    let ownership =
        AsyncFd::new(ownership).map_err(|error| format!("register ownership pipe: {error}"))?;

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

#[cfg(target_os = "macos")]
/// Kill and reap an app before its process-group observer is available.
async fn terminate_unobserved_app(app: &mut Child, process_group_id: Option<i32>) {
    if process_group_id.is_some() {
        terminate_process_group_without_logging(process_group_id);
    } else {
        let _start_kill_result = app.start_kill();
    }
    let _result = app.wait().await;
}

#[cfg(target_os = "macos")]
/// Relay one asynchronous byte stream.
async fn relay<R, W>(mut reader: R, mut writer: W) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    copy(&mut reader, &mut writer).await
}

#[cfg(target_os = "macos")]
/// Decode one relay task result.
fn relay_join_result(result: Result<io::Result<u64>, JoinError>) -> Result<(), String> {
    result
        .map_err(|error| format!("relay task failed: {error}"))?
        .map(|_| ())
        .map_err(|error| format!("stdio relay failed: {error}"))
}

#[cfg(target_os = "macos")]
/// Decode and await the direct app child wait task.
async fn wait_for_child(
    task: &mut JoinHandle<io::Result<ExitStatus>>,
) -> Result<ExitStatus, String> {
    task.await
        .map_err(|error| format!("app wait task failed: {error}"))?
        .map_err(|error| format!("app wait failed: {error}"))
}

#[cfg(target_os = "macos")]
/// Await one relay task when it is still active.
async fn await_relay(task: &mut JoinHandle<io::Result<u64>>, active: &mut bool) {
    if *active {
        let _result = task.await;
        *active = false;
    }
}

#[cfg(target_os = "macos")]
/// Stop relay tasks after a group-level cleanup path.
async fn abort_relays(
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

#[cfg(target_os = "macos")]
/// Stdio relay tasks and their completion state for one supervisor.
struct RelayTasks {
    /// App stdout to outer stdout relay.
    stdout: JoinHandle<io::Result<u64>>,
    /// App stderr to outer stderr relay.
    stderr: JoinHandle<io::Result<u64>>,
    /// Whether the stdout relay is still pending.
    stdout_active: bool,
    /// Whether the stderr relay is still pending.
    stderr_active: bool,
}

#[cfg(target_os = "macos")]
impl RelayTasks {
    /// Start stdout and stderr supervisor relay tasks.
    fn new(stdout_reader: ChildStdout, stderr_reader: ChildStderr) -> Self {
        Self {
            stdout: tokio::spawn(relay(stdout_reader, stdout())),
            stderr: tokio::spawn(relay(stderr_reader, stderr())),
            stdout_active: true,
            stderr_active: true,
        }
    }

    /// Wait until ownership, the app child, or a relay requires cleanup.
    async fn wait_for_trigger(
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
    async fn finish(&mut self, app_exited_naturally: bool) {
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

#[cfg(target_os = "macos")]
/// Event emitted by the supervisor's owner, child, or relay paths.
enum SupervisorTrigger {
    /// Outer ownership writer reached EOF or failed.
    Owner(io::Result<OwnerExit>),
    /// Direct app child exited.
    App(Result<ExitStatus, String>),
    /// A stdio relay failed.
    Relay(String),
}

#[cfg(target_os = "macos")]
/// Classification of an ownership channel close.
#[derive(Debug, Clone, Copy)]
enum OwnerExit {
    /// The outer launcher explicitly marked a deliberate shutdown.
    Normal,
    /// The channel closed without the deliberate-shutdown marker.
    Abnormal,
}

#[cfg(target_os = "macos")]
/// Wait for the normal marker or bare EOF on the dedicated owner pipe.
async fn wait_for_owner_eof(owner: &AsyncFd<OwnedFd>) -> io::Result<OwnerExit> {
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

#[cfg(target_os = "macos")]
/// Read one owner message byte from the pipe without blocking.
fn read_owner_fd(fd: RawFd) -> io::Result<Option<u8>> {
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

#[cfg(target_os = "macos")]
/// Write the deliberate-shutdown marker before closing the owner endpoint.
fn write_normal_shutdown_marker(writer: &OwnedFd) -> io::Result<()> {
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

#[cfg(target_os = "macos")]
/// Create a pipe whose ends remain close-on-exec until a child clears its reader.
fn create_inherited_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    // macOS has no atomic pipe-with-CLOEXEC API, so protect both ends before returning them.
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

#[cfg(target_os = "macos")]
/// Set or clear close-on-exec for one file descriptor.
fn set_cloexec(fd: RawFd, enabled: bool) -> io::Result<()> {
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

#[cfg(target_os = "macos")]
/// Configure one inherited pipe for readiness-based EOF detection.
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
/// Terminate an app process group without requiring a launcher log state.
fn terminate_process_group_without_logging(process_group_id: Option<i32>) {
    if let Some(pgid) = process_group_id {
        let _result = kill_process_group(pgid);
    }
}

#[cfg(target_os = "macos")]
/// Observe process exits using kqueue rather than polling.
struct ProcessGroupObserver {
    /// Tokio-registered kqueue descriptor.
    queue: AsyncFd<OwnedFd>,
}

#[cfg(target_os = "macos")]
impl ProcessGroupObserver {
    /// Create an empty process-exit observer.
    fn new() -> io::Result<Self> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let queue = AsyncFd::with_interest(fd, Interest::READABLE)?;
        Ok(Self { queue })
    }

    /// Register current members of one process group for exit notifications.
    fn watch_group(&self, process_group_id: i32) -> io::Result<()> {
        for pid in recording::live_process_group_members(process_group_id) {
            let _exists = self.watch_pid(pid)?;
        }
        Ok(())
    }

    /// Register one process for a one-shot NOTE_EXIT event.
    fn watch_pid(&self, pid: i32) -> io::Result<bool> {
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
    async fn wait_until_group_empty(&self, process_group_id: i32) -> io::Result<()> {
        loop {
            let members = recording::live_process_group_members(process_group_id);
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
    async fn next_event(&self) -> io::Result<()> {
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

#[cfg(target_os = "macos")]
/// Read and consume one or more pending kqueue events.
fn read_kqueue(fd: RawFd) -> io::Result<usize> {
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

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::{
        ffi::CString,
        fs::{self, File},
        io::{BufRead, BufReader},
        os::{fd::FromRawFd, unix::fs::PermissionsExt},
        path::PathBuf,
        sync::{Arc, Barrier, mpsc},
        thread,
        time::Duration,
    };

    use eguidev::internal::presentation::Presentation;
    use tempfile::Builder;
    use tokio::runtime;

    use super::*;

    fn test_tempdir() -> tempfile::TempDir {
        fs::create_dir_all("tmp").expect("create tmp");
        Builder::new()
            .prefix("edev-lifecycle-test-")
            .tempdir_in("tmp")
            .expect("tempdir")
    }

    fn test_config(cwd: PathBuf) -> LaunchConfig {
        LaunchConfig {
            cwd,
            command: vec!["/usr/bin/true".to_string()],
            env: Default::default(),
            presentation: Presentation::Background,
            shutdown_grace: Duration::from_secs(30),
            verbose: false,
            request_timeout: crate::APP_REQUEST_TIMEOUT,
        }
    }

    fn is_cloexec(fd: RawFd) -> bool {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0, "read fd flags");
        flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn inherited_pipe_ends_remain_cloexec_in_the_parent() {
        let (reader, writer) = create_inherited_pipe().expect("pipe");
        assert!(is_cloexec(reader.as_raw_fd()));
        assert!(is_cloexec(writer.as_raw_fd()));
    }

    #[test]
    fn concurrent_supervisor_launches_do_not_retain_private_pipe_endpoints() {
        const LAUNCHES: usize = 4;

        let tempdir = test_tempdir();
        let _registry =
            instance_registry::InstanceRegistry::register(&test_config(tempdir.path().into()))
                .expect("register launcher");
        let script = tempdir.path().join("config-reader.sh");
        fs::write(
            &script,
            concat!(
                "#!/bin/sh\nset -eu\nfd=\"$3\"\n",
                "payload=$(eval \"cat <&$fd\")\n",
                "owner_fd=$(printf '%s\\n' \"$payload\" | ",
                "sed -n 's/.*\"ownership_fd\":\\([0-9][0-9]*\\).*/\\1/p')\n",
                "test -n \"$owner_fd\"\n",
                "printf '%s\\n' \"$$\" > \"$PWD/ready\"\n",
                "eval \"cat <&$owner_fd\" >/dev/null\n",
            ),
        )
        .expect("write helper");
        let mut permissions = fs::metadata(&script)
            .expect("helper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script, permissions).expect("make helper executable");

        let ready_path = tempdir.path().join("ready");
        let ready_c_path =
            CString::new(ready_path.as_os_str().as_encoded_bytes()).expect("ready path");
        assert_eq!(
            unsafe { libc::mkfifo(ready_c_path.as_ptr(), 0o600) },
            0,
            "create ready fifo"
        );
        let ready_fd = unsafe { libc::open(ready_c_path.as_ptr(), libc::O_RDWR) };
        assert!(ready_fd >= 0, "open ready fifo");
        let ready_file = unsafe { File::from_raw_fd(ready_fd) };

        let barrier = Arc::new(Barrier::new(LAUNCHES + 1));
        thread::scope(|scope| {
            let mut handles = Vec::new();
            let mut shutdown_senders = Vec::new();
            let mut completion_receivers = Vec::new();
            for _ in 0..LAUNCHES {
                let barrier = Arc::clone(&barrier);
                let cwd = tempdir.path().to_path_buf();
                let executable = script.clone();
                let (shutdown_sender, shutdown_receiver) = mpsc::channel();
                let (completion_sender, completion_receiver) = mpsc::channel();
                shutdown_senders.push(shutdown_sender);
                completion_receivers.push(completion_receiver);
                handles.push(scope.spawn(move || {
                    let runtime = runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("runtime");
                    runtime.block_on(async move {
                        barrier.wait();
                        let config = test_config(cwd);
                        let mut process = spawn_supervised_with_executable(
                            &config,
                            LogState::new(false),
                            executable,
                        )
                        .await
                        .expect("spawn supervisor");
                        process.stdout.take();
                        process.stderr.take();
                        shutdown_receiver.recv().expect("shutdown signal");
                        shutdown_spawned(process, &LogState::new(false)).await;
                        completion_sender.send(()).expect("completion receiver");
                    });
                }));
            }

            barrier.wait();
            let (ready_sender, ready_receiver) = mpsc::channel();
            let ready_handle = scope.spawn(move || {
                let mut reader = BufReader::new(ready_file);
                let mut lines = Vec::new();
                let result = (|| {
                    for _ in 0..LAUNCHES {
                        let mut line = String::new();
                        reader
                            .read_line(&mut line)
                            .map_err(|error| error.to_string())?;
                        if line.is_empty() {
                            return Err(
                                "ready fifo closed before all launches reported".to_string()
                            );
                        }
                        lines.push(line);
                    }
                    Ok::<_, String>(lines)
                })();
                ready_sender.send(result).expect("ready receiver");
            });
            let ready_result = ready_receiver
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "concurrent launches retained a config reader".to_string())
                .expect("concurrent launches should read private config pipes")
                .expect("all supervisors should read their config");
            assert_eq!(ready_result.len(), LAUNCHES);

            for (shutdown_sender, completion_receiver) in
                shutdown_senders.into_iter().zip(completion_receivers)
            {
                shutdown_sender.send(()).expect("launch worker");
                completion_receiver
                    .recv_timeout(Duration::from_secs(5))
                    .expect("a sibling launch retained the ownership writer");
            }

            for handle in handles {
                handle.join().expect("launch worker");
            }
            ready_handle.join().expect("ready reader");
        });
    }
}
