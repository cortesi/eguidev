//! Process identity of the launcher, supervisor, and app records.

use super::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod other;

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(not(target_os = "macos"))]
pub use other::*;

/// Read the start-time identity of one live process by its unsigned PID.
pub(super) fn pid_start_time(pid: u32) -> Option<ProcessStartTime> {
    i32::try_from(pid).ok().and_then(process_start_time)
}

/// Return true when a recorded process is alive and, when the record holds a
/// start time, is the same process. A reused PID then reads as dead.
pub(super) fn recorded_process_is_alive(pid: u32, start_time: Option<ProcessStartTime>) -> bool {
    is_process_alive(pid) && start_time.is_none_or(|recorded| pid_start_time(pid) == Some(recorded))
}

/// Confirm that a recorded process group still belongs to the original app
/// leader.
pub fn recorded_app_group_is_current(record: &AppRecord) -> bool {
    record
        .app_group_leader_start_time
        .zip(process_start_time(record.app_process_group_id))
        .is_some_and(|(recorded, current)| recorded == current)
}
