//! Native recording support for the `edev record` command.

use std::{collections::BTreeSet, path::PathBuf};

use crate::EdevError;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(all(test, target_os = "macos"))]
#[path = "unsupported.rs"]
mod unsupported_for_tests;

#[cfg(target_os = "macos")]
pub use macos::live_process_group_members;
#[cfg(target_os = "macos")]
pub use macos::{NativeRecording, ensure_supported, process_group_members, start};
#[cfg(not(target_os = "macos"))]
pub(crate) use unsupported::{NativeRecording, ensure_supported, process_group_members, start};

/// Request needed to start a native recording session.
#[derive(Debug, Clone)]
pub struct RecordingRequest {
    /// Output `.mov` path.
    pub(crate) outfile: PathBuf,
    /// Native window title to capture.
    pub(crate) title: String,
    /// Process ids belonging to the app launch tree.
    pub(crate) app_process_ids: BTreeSet<i32>,
}

/// Summary returned after recording finalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSummary {
    /// Final output path.
    pub(crate) outfile: PathBuf,
    /// Bytes reported by the native recorder.
    pub(crate) file_size: u64,
}

/// One native window that could be selected for recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCandidate {
    /// ScreenCaptureKit window id.
    pub(crate) window_id: u32,
    /// Native window title, if available.
    pub(crate) title: Option<String>,
    /// Owning app name, if available.
    pub(crate) owner_name: Option<String>,
    /// Owning process id, if available.
    pub(crate) process_id: Option<i32>,
}

impl WindowCandidate {
    /// Whether this candidate belongs to the launched app process set.
    fn is_owned_by(&self, app_process_ids: &BTreeSet<i32>) -> bool {
        self.process_id
            .is_some_and(|process_id| app_process_ids.contains(&process_id))
    }

    /// Render one candidate for user-facing diagnostics.
    fn describe(&self, app_process_ids: &BTreeSet<i32>) -> String {
        let title = self.title.as_deref().unwrap_or("<untitled>");
        let owner = self.owner_name.as_deref().unwrap_or("<unknown>");
        let pid = self
            .process_id
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let ownership = if self.is_owned_by(app_process_ids) {
            "process-tree"
        } else {
            "title-only"
        };
        format!(
            "window {} title={title:?} app={owner:?} pid={pid} match={ownership}",
            self.window_id
        )
    }
}

/// Result of selecting a native window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSelection {
    /// Selected ScreenCaptureKit window id.
    pub(crate) window_id: u32,
    /// Whether ownership was established through the app process set.
    pub(crate) owner_match: bool,
}

/// Select one window by preferring app-owned title matches over title-only
/// fallback.
pub fn select_window(
    candidates: &[WindowCandidate],
    title: &str,
    app_process_ids: &BTreeSet<i32>,
) -> Result<WindowSelection, EdevError> {
    if title.trim().is_empty() {
        return Err(EdevError::RecordFailed(
            "root viewport has no title; pass --window-title <TITLE>".to_string(),
        ));
    }

    let title_matches = candidates
        .iter()
        .filter(|candidate| candidate.title.as_deref() == Some(title))
        .collect::<Vec<_>>();
    if title_matches.is_empty() {
        return Err(EdevError::RecordFailed(not_found_message(
            candidates,
            title,
            app_process_ids,
        )));
    }

    let owned_matches = title_matches
        .iter()
        .copied()
        .filter(|candidate| candidate.is_owned_by(app_process_ids))
        .collect::<Vec<_>>();
    match owned_matches.as_slice() {
        [candidate] => {
            return Ok(WindowSelection {
                window_id: candidate.window_id,
                owner_match: true,
            });
        }
        [] => {}
        matches => {
            return Err(EdevError::RecordFailed(ambiguity_message(
                matches,
                title,
                app_process_ids,
            )));
        }
    }

    match title_matches.as_slice() {
        [candidate] => Ok(WindowSelection {
            window_id: candidate.window_id,
            owner_match: false,
        }),
        matches => Err(EdevError::RecordFailed(ambiguity_message(
            matches,
            title,
            app_process_ids,
        ))),
    }
}

/// Format a native-window not-found error.
fn not_found_message(
    candidates: &[WindowCandidate],
    title: &str,
    app_process_ids: &BTreeSet<i32>,
) -> String {
    let mut message = format!(
        "could not find a visible native window titled {title:?}; likely causes include a hidden window, missing root viewport title, or Screen Recording permission denial",
    );
    let titled = candidates
        .iter()
        .filter(|candidate| candidate.title.is_some())
        .take(8)
        .map(|candidate| candidate.describe(app_process_ids))
        .collect::<Vec<_>>();
    if !titled.is_empty() {
        message.push_str("; visible titled windows included: ");
        message.push_str(&titled.join("; "));
    }
    message
}

/// Format a native-window ambiguity error.
fn ambiguity_message(
    candidates: &[&WindowCandidate],
    title: &str,
    app_process_ids: &BTreeSet<i32>,
) -> String {
    let matches = candidates
        .iter()
        .map(|candidate| candidate.describe(app_process_ids))
        .collect::<Vec<_>>()
        .join("; ");
    format!("ambiguous native windows titled {title:?}: {matches}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        window_id: u32,
        title: Option<&str>,
        owner_name: &str,
        process_id: Option<i32>,
    ) -> WindowCandidate {
        WindowCandidate {
            window_id,
            title: title.map(str::to_string),
            owner_name: Some(owner_name.to_string()),
            process_id,
        }
    }

    fn process_set(process_ids: &[i32]) -> BTreeSet<i32> {
        process_ids.iter().copied().collect()
    }

    #[test]
    fn selection_prefers_owned_title_match() {
        let candidates = vec![
            candidate(1, Some("Demo"), "Other", Some(11)),
            candidate(2, Some("Demo"), "App", Some(22)),
        ];

        let selection = select_window(&candidates, "Demo", &process_set(&[22])).expect("selection");

        assert_eq!(selection.window_id, 2);
        assert!(selection.owner_match);
    }

    #[test]
    fn selection_uses_title_only_when_process_tree_misses() {
        let candidates = vec![candidate(1, Some("Detached"), "App", Some(33))];

        let selection =
            select_window(&candidates, "Detached", &process_set(&[22])).expect("selection");

        assert_eq!(selection.window_id, 1);
        assert!(!selection.owner_match);
    }

    #[test]
    fn selection_uses_the_requested_override_title() {
        let candidates = vec![
            candidate(1, Some("Root"), "App", Some(22)),
            candidate(2, Some("Override"), "App", Some(22)),
        ];

        let selection =
            select_window(&candidates, "Override", &process_set(&[22])).expect("selection");

        assert_eq!(selection.window_id, 2);
        assert!(selection.owner_match);
    }

    #[test]
    fn selection_rejects_duplicate_owned_titles() {
        let candidates = vec![
            candidate(1, Some("Demo"), "App", Some(22)),
            candidate(2, Some("Demo"), "App", Some(23)),
        ];

        let error =
            select_window(&candidates, "Demo", &process_set(&[22, 23])).expect_err("ambiguous");

        assert!(matches!(error, EdevError::RecordFailed(message) if message.contains("ambiguous")));
    }

    #[test]
    fn selection_rejects_duplicate_title_only_matches() {
        let candidates = vec![
            candidate(1, Some("Demo"), "Other", Some(11)),
            candidate(2, Some("Demo"), "Another", Some(12)),
        ];

        let error = select_window(&candidates, "Demo", &process_set(&[22])).expect_err("ambiguous");

        assert!(
            matches!(error, EdevError::RecordFailed(message) if message.contains("title-only"))
        );
    }

    #[test]
    fn selection_ignores_missing_candidate_titles() {
        let candidates = vec![
            candidate(1, None, "Other", Some(11)),
            candidate(2, Some("Demo"), "App", Some(22)),
        ];

        let selection = select_window(&candidates, "Demo", &process_set(&[22])).expect("selection");

        assert_eq!(selection.window_id, 2);
    }

    #[test]
    fn selection_rejects_missing_query_title() {
        let candidates = vec![candidate(1, Some("Demo"), "App", Some(22))];

        let error = select_window(&candidates, "", &process_set(&[22])).expect_err("missing");

        assert!(
            matches!(error, EdevError::RecordFailed(message) if message.contains("--window-title"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unsupported_backend_rejects_recording() {
        let error = unsupported_for_tests::ensure_supported().expect_err("unsupported platform");
        assert!(
            matches!(error, EdevError::RecordFailed(message) if message.contains("only supported on macOS 15.0 or newer"))
        );

        let request = RecordingRequest {
            outfile: "tmp/unsupported.mov".into(),
            title: "Unsupported".to_string(),
            app_process_ids: BTreeSet::new(),
        };
        let start_result = unsupported_for_tests::start(&request);
        assert!(
            matches!(start_result, Err(EdevError::RecordFailed(message)) if message.contains("only supported on macOS 15.0 or newer"))
        );
        let stop_result = unsupported_for_tests::NativeRecording.stop();
        assert!(
            matches!(stop_result, Err(EdevError::RecordFailed(message)) if message.contains("only supported on macOS 15.0 or newer"))
        );
        assert!(unsupported_for_tests::process_group_members(Some(1)).is_empty());
    }
}
