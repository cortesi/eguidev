//! Direct app launch on platforms with parent-death signaling.

use super::*;

/// No ownership endpoint is needed on platforms with parent-death signaling.
pub type OwnershipWriter = ();

/// Return whether a newly spawned app boundary exited before its MCP handshake.
pub fn spawned_process_exited(process: &mut SpawnedProcess) -> Result<bool, String> {
    process
        .child
        .as_mut()
        .map(Child::try_wait)
        .transpose()
        .map(|status| status.flatten().is_some())
        .map_err(|error| format!("inspect app process during startup: {error}"))
}

/// Fetch the process group id from a spawned direct child.
pub fn process_group_id(child: &Child) -> Option<i32> {
    child.id().and_then(|id| i32::try_from(id).ok())
}

/// Spawn the current app command directly on platforms with parent-death
/// support.
pub(super) async fn spawn_direct(
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
