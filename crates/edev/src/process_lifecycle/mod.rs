//! Private platform launch and cleanup abstraction for edev.

#[cfg(target_os = "macos")]
use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    path::{Path, PathBuf},
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
use crate::cargo_env;

#[cfg(not(target_os = "macos"))]
mod direct;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
pub use direct::*;
#[cfg(target_os = "macos")]
pub use macos::*;

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

/// Detect the hidden supervisor invocation without exposing it through clap
/// help.
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

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;
