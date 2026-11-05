//! Process identity on platforms other than macOS.

use super::*;

/// Process start-time identity is not used by non-macOS launchers.
pub fn process_start_time(_pid: i32) -> Option<ProcessStartTime> {
    None
}

#[cfg(unix)]
/// Return true when a process with the provided pid appears alive.
pub(super) fn is_process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    !matches!(error.raw_os_error(), Some(libc::ESRCH))
}

#[cfg(not(unix))]
/// Process liveness checks are conservative on non-unix platforms.
pub(super) fn is_process_alive(_pid: u32) -> bool {
    true
}
