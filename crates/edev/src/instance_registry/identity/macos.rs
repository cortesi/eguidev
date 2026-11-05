//! Process identity on macOS, read from the BSD process information.

use libproc::{bsd_info::BSDInfo, proc_pid::pidinfo};

use super::*;

/// Read the start-time identity of one live process.
pub fn process_start_time(pid: i32) -> Option<ProcessStartTime> {
    let info = pidinfo::<BSDInfo>(pid, 0).ok()?;
    Some(ProcessStartTime {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

/// Return true when a process with the provided pid appears alive.
pub fn is_process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // The launcher is a same-user process, so BSD info remains available
    // throughout its life.
    pidinfo::<BSDInfo>(pid, 0).is_ok_and(|info| info.pbi_status != libc::SZOMB)
}

#[cfg(feature = "test-app")]
/// Return whether `pid` is alive under the production liveness definition.
pub fn process_is_alive(pid: i32) -> bool {
    u32::try_from(pid).is_ok_and(is_process_alive)
}
