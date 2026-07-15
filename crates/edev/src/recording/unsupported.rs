//! Unsupported-platform recording backend.

use super::{RecordingRequest, RecordingSummary};
use crate::EdevError;

/// Placeholder native recording handle for unsupported platforms.
pub struct NativeRecording;

impl NativeRecording {
    /// Stop recording. This is unreachable because startup always fails.
    pub(crate) fn stop(self) -> Result<RecordingSummary, EdevError> {
        Err(EdevError::RecordFailed(
            "`edev record` is only supported on macOS 15.0 or newer".to_string(),
        ))
    }
}

/// Reject recording before any app launch happens.
pub fn ensure_supported() -> Result<(), EdevError> {
    Err(EdevError::RecordFailed(
        "`edev record` is only supported on macOS 15.0 or newer".to_string(),
    ))
}

/// Return no process-tree members on unsupported platforms.
pub fn process_group_members(_process_group_id: Option<i32>) -> Vec<i32> {
    Vec::new()
}

/// Return no live process-group members on unsupported platforms.
#[cfg(not(all(test, target_os = "macos")))]
pub fn live_process_group_members(_process_group_id: i32) -> Vec<i32> {
    Vec::new()
}

/// Reject recording before any app launch happens.
pub fn start(_request: &RecordingRequest) -> Result<NativeRecording, EdevError> {
    ensure_supported()?;
    unreachable!("unsupported recording startup should fail in ensure_supported")
}
