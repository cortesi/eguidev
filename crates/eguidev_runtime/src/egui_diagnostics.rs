//! Capture and retain egui identity diagnostics from completed passes.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Mutex,
};

use egui::{Color32, FullOutput, Shape, StrokeKind};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::Rect;

const JOURNAL_CAPACITY: usize = 1_024;
const RECT_CHANGED_MESSAGE: &str = "Widget rectangle changed identity between completed passes";

/// Kind of identity warning emitted by egui.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EguiDiagnosticKind {
    /// One egui ID was used for different rectangles in one pass.
    IdClash,
    /// One rectangle changed egui ID between completed passes.
    RectChangedId,
}

/// Severity assigned to an egui diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EguiDiagnosticSeverity {
    /// The diagnostic reports suspicious identity behavior.
    Warning,
}

/// One egui identity diagnostic from a completed viewport pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EguiDiagnostic {
    /// Stable diagnostic kind.
    pub kind: EguiDiagnosticKind,
    /// Diagnostic severity.
    pub severity: EguiDiagnosticSeverity,
    /// Human-readable warning text.
    pub message: String,
    /// Canonical viewport selector that produced the diagnostic.
    pub viewport_id: String,
    /// Eguidev app-wide frame counter at output completion.
    pub frame: u64,
    /// Affected rectangle when egui supplies one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
}

/// Diagnostics retained for one script evaluation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EguiDiagnosticBatch {
    /// Retained diagnostics in journal sequence order.
    pub entries: Vec<EguiDiagnostic>,
    /// Undismissed diagnostics overwritten before they could be returned.
    pub dropped: u64,
}

impl EguiDiagnosticBatch {
    /// Return whether the batch contains no retained or dropped diagnostics.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.dropped == 0
    }
}

#[derive(Debug, Clone)]
struct JournalEntry {
    sequence: u64,
    diagnostic: EguiDiagnostic,
}

#[derive(Debug, Clone, Copy)]
pub struct OutputCompletion {
    pub sequence: u64,
    pub frame: u64,
}

#[derive(Debug)]
struct JournalState {
    capacity: usize,
    next_entry_sequence: u64,
    next_completion_sequence: u64,
    entries: VecDeque<JournalEntry>,
    completions: HashMap<String, OutputCompletion>,
}

impl JournalState {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next_entry_sequence: 0,
            next_completion_sequence: 0,
            entries: VecDeque::with_capacity(capacity),
            completions: HashMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct DiagnosticSelection {
    pub batch: EguiDiagnosticBatch,
    pub sequences: Vec<u64>,
}

/// Process-wide bounded journal of egui identity diagnostics.
#[derive(Debug)]
pub struct EguiDiagnosticJournal {
    state: Mutex<JournalState>,
    completion_notify: Notify,
}

impl Default for EguiDiagnosticJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl EguiDiagnosticJournal {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(JournalState::new(JOURNAL_CAPACITY)),
            completion_notify: Notify::new(),
        }
    }

    #[cfg(test)]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            state: Mutex::new(JournalState::new(capacity)),
            completion_notify: Notify::new(),
        }
    }

    pub fn record_output(&self, viewport_id: String, frame: u64, output: &FullOutput) {
        let diagnostics = collect_output_diagnostics(&viewport_id, frame, output);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for diagnostic in diagnostics {
            let sequence = state.next_entry_sequence;
            state.next_entry_sequence += 1;
            state.entries.push_back(JournalEntry {
                sequence,
                diagnostic,
            });
            if state.entries.len() > state.capacity {
                state.entries.pop_front();
            }
        }
        let sequence = state.next_completion_sequence;
        state.next_completion_sequence += 1;
        state
            .completions
            .insert(viewport_id, OutputCompletion { sequence, frame });
        drop(state);
        self.completion_notify.notify_waiters();
    }

    pub fn tail_sequence(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .next_entry_sequence
    }

    pub fn output_completion(&self, viewport_id: &str) -> Option<OutputCompletion> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .completions
            .get(viewport_id)
            .copied()
    }

    pub fn completion_notify(&self) -> &Notify {
        &self.completion_notify
    }

    pub fn select(
        &self,
        start_sequence: u64,
        dismissed: &BTreeSet<u64>,
        viewport_id: Option<&str>,
    ) -> DiagnosticSelection {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let retained_start = state
            .entries
            .front()
            .map_or(state.next_entry_sequence, |entry| entry.sequence);
        let lost_end = retained_start.min(state.next_entry_sequence);
        let lost_total = lost_end.saturating_sub(start_sequence);
        let dismissed_lost = if start_sequence < lost_end {
            dismissed
                .range(start_sequence..lost_end)
                .count()
                .try_into()
                .unwrap_or(u64::MAX)
        } else {
            0
        };
        let dropped = lost_total.saturating_sub(dismissed_lost);
        let retained = state
            .entries
            .iter()
            .filter(|entry| entry.sequence >= start_sequence)
            .filter(|entry| !dismissed.contains(&entry.sequence))
            .filter(|entry| {
                viewport_id.is_none_or(|viewport_id| entry.diagnostic.viewport_id == viewport_id)
            })
            .collect::<Vec<_>>();
        DiagnosticSelection {
            batch: EguiDiagnosticBatch {
                entries: retained
                    .iter()
                    .map(|entry| entry.diagnostic.clone())
                    .collect(),
                dropped,
            },
            sequences: retained.iter().map(|entry| entry.sequence).collect(),
        }
    }
}

fn collect_output_diagnostics(
    viewport_id: &str,
    frame: u64,
    output: &FullOutput,
) -> Vec<EguiDiagnostic> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| diagnostic_from_shape(viewport_id, frame, &clipped.shape))
        .collect()
}

fn diagnostic_from_shape(viewport_id: &str, frame: u64, shape: &Shape) -> Option<EguiDiagnostic> {
    match shape {
        Shape::Text(text) if text.galley.text().starts_with('🔥') => Some(EguiDiagnostic {
            kind: EguiDiagnosticKind::IdClash,
            severity: EguiDiagnosticSeverity::Warning,
            message: text.galley.text().to_string(),
            viewport_id: viewport_id.to_string(),
            frame,
            rect: None,
        }),
        Shape::Rect(rect)
            if rect.fill == Color32::TRANSPARENT
                && rect.corner_radius == egui::CornerRadius::ZERO
                && rect.stroke.width == 2.0
                && rect.stroke.color == Color32::RED
                && rect.stroke_kind == StrokeKind::Outside
                && rect.round_to_pixels.is_none()
                && rect.blur_width == 0.0
                && rect.brush.is_none()
                && rect.angle == 0.0 =>
        {
            Some(EguiDiagnostic {
                kind: EguiDiagnosticKind::RectChangedId,
                severity: EguiDiagnosticSeverity::Warning,
                message: RECT_CHANGED_MESSAGE.to_string(),
                viewport_id: viewport_id.to_string(),
                frame,
                rect: Some(rect.rect.into()),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_clash_output() -> FullOutput {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.warn_on_id_clash = true);
        ctx.all_styles_mut(|style| style.debug.warn_if_rect_changes_id = false);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let id = egui::Id::new("duplicate");
            ui.ctx().check_for_id_clash(
                id,
                egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(20.0, 20.0)),
                "test widget",
            );
            ui.ctx().check_for_id_clash(
                id,
                egui::Rect::from_min_size(egui::pos2(60.0, 10.0), egui::vec2(20.0, 20.0)),
                "test widget",
            );
        });
        output.textures_delta.clear();
        output
    }

    fn rect_changed_output() -> FullOutput {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.warn_on_id_clash = false);
        ctx.all_styles_mut(|style| style.debug.warn_if_rect_changes_id = true);
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(20.0, 20.0));
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = ui.interact(rect, egui::Id::new("first"), egui::Sense::click());
        })
        .drop_without_applying_deltas();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = ui.interact(rect, egui::Id::new("second"), egui::Sense::click());
        });
        output.textures_delta.clear();
        output
    }

    #[test]
    fn collects_real_id_clash_text() {
        let diagnostics = collect_output_diagnostics("root", 7, &id_clash_output());
        assert!(
            diagnostics
                .iter()
                .any(|entry| entry.kind == EguiDiagnosticKind::IdClash)
        );
        let diagnostic = diagnostics
            .iter()
            .find(|entry| entry.kind == EguiDiagnosticKind::IdClash)
            .expect("id clash diagnostic");
        assert!(diagnostic.message.starts_with('🔥'));
        assert_eq!(diagnostic.viewport_id, "root");
        assert_eq!(diagnostic.frame, 7);
        assert_eq!(diagnostic.rect, None);
    }

    #[test]
    fn collects_real_rect_changed_marker() {
        let diagnostics = collect_output_diagnostics("vp:2", 11, &rect_changed_output());
        let diagnostic = diagnostics
            .iter()
            .find(|entry| entry.kind == EguiDiagnosticKind::RectChangedId)
            .expect("rectangle identity diagnostic");
        assert_eq!(diagnostic.message, RECT_CHANGED_MESSAGE);
        assert_eq!(diagnostic.viewport_id, "vp:2");
        assert_eq!(diagnostic.frame, 11);
        assert!(diagnostic.rect.is_some());
    }

    #[test]
    fn ignores_near_match_rectangles() {
        let marker = Shape::rect_stroke(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
            0,
            (2.0, Color32::RED),
            StrokeKind::Inside,
        );
        assert!(diagnostic_from_shape("root", 1, &marker).is_none());
    }

    #[test]
    fn journal_preserves_order_and_reports_only_undismissed_loss() {
        let journal = EguiDiagnosticJournal::with_capacity(2);
        let output = id_clash_output();
        journal.record_output("root".to_string(), 1, &output);
        let start = 0;
        let first = journal.select(start, &BTreeSet::new(), None);
        assert!(!first.sequences.is_empty());
        let first_sequence = first.sequences[0];

        journal.record_output("root".to_string(), 2, &output);
        journal.record_output("root".to_string(), 3, &output);

        let lost = journal.select(start, &BTreeSet::new(), None);
        assert!(lost.batch.dropped > 0);
        assert!(
            lost.batch
                .entries
                .windows(2)
                .all(|pair| pair[0].frame <= pair[1].frame)
        );

        let dismissed = BTreeSet::from([first_sequence]);
        let after_dismissal = journal.select(start, &dismissed, None);
        assert_eq!(after_dismissal.batch.dropped + 1, lost.batch.dropped);
        assert_eq!(
            journal
                .select(start, &dismissed, Some("vp:missing"))
                .batch
                .dropped,
            after_dismissal.batch.dropped,
            "dropped count is evaluation-wide"
        );
    }

    #[test]
    fn viewport_selection_dismisses_only_matching_entries() {
        let journal = EguiDiagnosticJournal::new();
        let output = id_clash_output();
        journal.record_output("root".to_string(), 1, &output);
        journal.record_output("vp:2".to_string(), 2, &output);

        let root = journal.select(0, &BTreeSet::new(), Some("root"));
        assert!(
            root.batch
                .entries
                .iter()
                .all(|entry| entry.viewport_id == "root")
        );
        let dismissed = root.sequences.into_iter().collect::<BTreeSet<_>>();
        let remaining = journal.select(0, &dismissed, None);
        assert!(
            remaining
                .batch
                .entries
                .iter()
                .all(|entry| entry.viewport_id == "vp:2")
        );
    }
}
