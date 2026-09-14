//! Input actions for injecting into egui.
#![allow(missing_docs)]

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

use crate::{
    registry::{lock, viewport_id_to_string},
    types::{Modifiers, Pos2, Vec2},
};

#[derive(Debug, Clone)]
pub enum InputAction {
    PointerMove {
        pos: Pos2,
    },
    PointerButton {
        pos: Pos2,
        button: egui::PointerButton,
        pressed: bool,
        modifiers: Modifiers,
    },
    Key {
        key: egui::Key,
        pressed: bool,
        modifiers: Modifiers,
    },
    Text {
        text: String,
    },
    Paste {
        text: String,
    },
    Scroll {
        delta: Vec2,
        modifiers: Modifiers,
    },
}

impl InputAction {
    pub fn apply(self, raw_input: &mut egui::RawInput) {
        match self {
            Self::PointerMove { pos } => {
                raw_input.events.push(egui::Event::PointerMoved(pos.into()));
            }
            Self::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                raw_input.events.push(egui::Event::PointerButton {
                    pos: pos.into(),
                    button,
                    pressed,
                    modifiers: modifiers.into(),
                });
            }
            Self::Key {
                key,
                pressed,
                modifiers,
            } => {
                raw_input.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: modifiers.into(),
                });
            }
            Self::Text { text } => {
                raw_input.events.push(egui::Event::Text(text));
            }
            Self::Paste { text } => {
                raw_input.events.push(egui::Event::Paste(text));
            }
            Self::Scroll { delta, modifiers } => {
                raw_input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: delta.into(),
                    phase: egui::TouchPhase::Move,
                    modifiers: modifiers.into(),
                });
            }
        }
    }

    fn pointer_pos(&self) -> Option<Pos2> {
        match self {
            Self::PointerMove { pos } | Self::PointerButton { pos, .. } => Some(*pos),
            Self::Key { .. } | Self::Text { .. } | Self::Paste { .. } | Self::Scroll { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ActionQueueStats {
    pub queued_actions: u64,
    pub drained_actions: u64,
    pub last_drain_frame: Option<u64>,
}

/// How many whole frames an action waits before it reaches the app.
///
/// Delayed actions start counting at their first drain. Repeated drains with
/// the same frame number do not shorten the delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTiming {
    /// Deliver at the next drain.
    Immediate,
    /// Deliver one frame after the next drain.
    AfterOneFrame,
    /// Deliver two frames after the next drain.
    AfterTwoFrames,
    /// Deliver three frames after the next drain.
    AfterThreeFrames,
}

impl ActionTiming {
    fn frames(self) -> u8 {
        match self {
            Self::Immediate => 0,
            Self::AfterOneFrame => 1,
            Self::AfterTwoFrames => 2,
            Self::AfterThreeFrames => 3,
        }
    }
}

/// An input action whose delay begins at its first drain, not an earlier batch.
struct QueuedAction {
    action: InputAction,
    frames_left: u8,
    last_frame: Option<u64>,
}

pub struct ActionQueue {
    actions: Mutex<HashMap<egui::ViewportId, Vec<QueuedAction>>>,
    commands: Mutex<HashMap<egui::ViewportId, Vec<egui::ViewportCommand>>>,
    stats: Mutex<HashMap<egui::ViewportId, ActionQueueStats>>,
    pointer_state: Mutex<PointerState>,
}

/// Recent synthetic-pointer state retained for input delivery and failure
/// diagnostics.
#[derive(Default)]
struct PointerState {
    /// Last position that the input hook consumed for each viewport.
    positions: HashMap<egui::ViewportId, Pos2>,
    /// Last frame that consumed a pointer action for each viewport.
    consumed_frames: HashMap<egui::ViewportId, u64>,
    /// Bounded event history serialized into failure diagnostics.
    trace: VecDeque<PointerTraceEvent>,
}

/// One pointer transition in the automation input path.
#[derive(serde::Serialize)]
struct PointerTraceEvent {
    /// Canonical viewport id.
    viewport_id: String,
    /// Queue, input-hook consumption, or app-reported position.
    phase: &'static str,
    /// Global Eguidev frame counter at this transition.
    frame: u64,
    /// Pointer position at this transition.
    pos: Pos2,
}

/// Maximum recent pointer transitions kept for a failure bundle.
const POINTER_TRACE_LIMIT: usize = 256;

impl Default for ActionQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionQueue {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(HashMap::new()),
            commands: Mutex::new(HashMap::new()),
            stats: Mutex::new(HashMap::new()),
            pointer_state: Mutex::new(PointerState::default()),
        }
    }

    pub fn queue_action_with_timing(
        &self,
        viewport_id: egui::ViewportId,
        timing: ActionTiming,
        action: InputAction,
    ) {
        queue_to_map(
            &self.actions,
            "actions lock",
            viewport_id,
            QueuedAction {
                action,
                frames_left: timing.frames(),
                last_frame: None,
            },
        );
        self.record_queued_action(viewport_id);
    }

    pub fn queue_command(&self, viewport_id: egui::ViewportId, command: egui::ViewportCommand) {
        queue_to_map(&self.commands, "commands lock", viewport_id, command);
    }

    pub fn drain_actions(&self, viewport_id: egui::ViewportId, frame: u64) -> Vec<InputAction> {
        let mut queue = lock(&self.actions, "actions lock");
        let mut pending = Vec::new();
        let mut current = Vec::new();
        for mut queued in queue.remove(&viewport_id).unwrap_or_default() {
            if queued.frames_left > 0 {
                if queued.last_frame.is_some_and(|previous| previous != frame) {
                    queued.frames_left -= 1;
                }
                queued.last_frame = Some(frame);
            }
            if queued.frames_left == 0 {
                current.push(queued.action);
            } else {
                pending.push(queued);
            }
        }
        if !pending.is_empty() {
            queue.insert(viewport_id, pending);
        }
        drop(queue);
        self.record_drain(viewport_id, current.len(), frame);
        self.record_pointer_actions(viewport_id, frame, "consumed", &current);
        current
    }

    pub fn drain_commands(&self, viewport_id: egui::ViewportId) -> Vec<egui::ViewportCommand> {
        let mut commands = lock(&self.commands, "commands lock");
        commands.remove(&viewport_id).unwrap_or_default()
    }

    pub fn clear_all(&self) {
        lock(&self.actions, "actions lock").clear();
        lock(&self.commands, "commands lock").clear();
        lock(&self.stats, "action stats lock").clear();
        *lock(&self.pointer_state, "pointer state lock") = PointerState::default();
    }

    pub fn clear_viewport(&self, viewport_id: egui::ViewportId) {
        lock(&self.actions, "actions lock").remove(&viewport_id);
        lock(&self.commands, "commands lock").remove(&viewport_id);
        lock(&self.stats, "action stats lock").remove(&viewport_id);
        let mut pointer = lock(&self.pointer_state, "pointer state lock");
        pointer.positions.remove(&viewport_id);
        pointer.consumed_frames.remove(&viewport_id);
        let viewport_id = viewport_id_to_string(viewport_id);
        pointer
            .trace
            .retain(|event| event.viewport_id != viewport_id);
    }

    pub fn stats(&self, viewport_id: egui::ViewportId) -> ActionQueueStats {
        lock(&self.stats, "action stats lock")
            .get(&viewport_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn has_pending_actions(&self, viewport_id: egui::ViewportId) -> bool {
        self.pending_action_count(viewport_id) > 0
    }

    pub fn pending_action_count(&self, viewport_id: egui::ViewportId) -> usize {
        pending_count(&self.actions, "actions lock", viewport_id)
    }

    pub fn has_pending_commands(&self, viewport_id: egui::ViewportId) -> bool {
        has_pending(&self.commands, "commands lock", viewport_id)
    }

    pub fn pending_command_count(&self, viewport_id: egui::ViewportId) -> usize {
        pending_count(&self.commands, "commands lock", viewport_id)
    }

    /// Record one pointer action when the runtime queues it.
    pub(crate) fn record_pointer_queued(
        &self,
        viewport_id: egui::ViewportId,
        frame: u64,
        action: &InputAction,
    ) {
        let Some(pos) = action.pointer_pos() else {
            return;
        };
        let mut state = lock(&self.pointer_state, "pointer state lock");
        push_pointer_trace(&mut state.trace, viewport_id, "queued", frame, pos);
    }

    /// Return the synthetic pointer position that must survive native input.
    pub(crate) fn pointer_pos(&self, viewport_id: egui::ViewportId) -> Option<Pos2> {
        lock(&self.pointer_state, "pointer state lock")
            .positions
            .get(&viewport_id)
            .copied()
    }

    /// Record the pointer position that the app reported after one pass.
    pub(crate) fn record_pointer_report(
        &self,
        viewport_id: egui::ViewportId,
        frame: u64,
        pos: Option<Pos2>,
    ) {
        let Some(pos) = pos else {
            return;
        };
        let mut state = lock(&self.pointer_state, "pointer state lock");
        let recent = state
            .consumed_frames
            .get(&viewport_id)
            .is_some_and(|consumed| frame <= consumed.saturating_add(4));
        if recent {
            push_pointer_trace(&mut state.trace, viewport_id, "reported", frame, pos);
        }
    }

    /// Return recent pointer delivery evidence for failure diagnostics.
    pub fn pointer_trace(&self) -> serde_json::Value {
        let state = lock(&self.pointer_state, "pointer state lock");
        serde_json::json!({
            "positions": state.positions.iter().map(|(viewport_id, pos)| {
                (viewport_id_to_string(*viewport_id), *pos)
            }).collect::<HashMap<_, _>>(),
            "events": state.trace,
        })
    }

    fn record_queued_action(&self, viewport_id: egui::ViewportId) {
        let mut stats = lock(&self.stats, "action stats lock");
        stats.entry(viewport_id).or_default().queued_actions += 1;
    }

    fn record_drain(&self, viewport_id: egui::ViewportId, count: usize, frame: u64) {
        if count == 0 {
            return;
        }
        let mut stats = lock(&self.stats, "action stats lock");
        let stats = stats.entry(viewport_id).or_default();
        stats.drained_actions += count as u64;
        stats.last_drain_frame = Some(frame);
    }

    /// Retain consumed pointer state and its bounded trace.
    fn record_pointer_actions(
        &self,
        viewport_id: egui::ViewportId,
        frame: u64,
        phase: &'static str,
        actions: &[InputAction],
    ) {
        let positions = actions
            .iter()
            .filter_map(InputAction::pointer_pos)
            .collect::<Vec<_>>();
        let Some(last) = positions.last().copied() else {
            return;
        };
        let mut state = lock(&self.pointer_state, "pointer state lock");
        state.positions.insert(viewport_id, last);
        state.consumed_frames.insert(viewport_id, frame);
        for pos in positions {
            push_pointer_trace(&mut state.trace, viewport_id, phase, frame, pos);
        }
    }
}

/// Append one bounded pointer trace event.
fn push_pointer_trace(
    trace: &mut VecDeque<PointerTraceEvent>,
    viewport_id: egui::ViewportId,
    phase: &'static str,
    frame: u64,
    pos: Pos2,
) {
    if trace.len() == POINTER_TRACE_LIMIT {
        trace.pop_front();
    }
    trace.push_back(PointerTraceEvent {
        viewport_id: viewport_id_to_string(viewport_id),
        phase,
        frame,
        pos,
    });
}

fn queue_to_map<T>(
    queue: &Mutex<HashMap<egui::ViewportId, Vec<T>>>,
    label: &'static str,
    viewport_id: egui::ViewportId,
    value: T,
) {
    let mut queue = lock(queue, label);
    queue.entry(viewport_id).or_default().push(value);
}

fn has_pending<T>(
    queue: &Mutex<HashMap<egui::ViewportId, Vec<T>>>,
    label: &'static str,
    viewport_id: egui::ViewportId,
) -> bool {
    pending_count(queue, label, viewport_id) > 0
}

fn pending_count<T>(
    queue: &Mutex<HashMap<egui::ViewportId, Vec<T>>>,
    label: &'static str,
    viewport_id: egui::ViewportId,
) -> usize {
    lock(queue, label).get(&viewport_id).map_or(0, Vec::len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_payload(action: InputAction) -> String {
        match action {
            InputAction::Text { text } => text,
            other => panic!("expected text action, got {other:?}"),
        }
    }

    #[test]
    fn drain_actions_promotes_staged_actions_one_frame_at_a_time() {
        let queue = ActionQueue::new();
        let viewport_id = egui::ViewportId::ROOT;
        // An active app has already drained input before a script queues work.
        assert!(queue.drain_actions(viewport_id, 9).is_empty());

        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::Immediate,
            InputAction::Text {
                text: "current".to_string(),
            },
        );
        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::AfterOneFrame,
            InputAction::Text {
                text: "next".to_string(),
            },
        );
        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::AfterTwoFrames,
            InputAction::Text {
                text: "later".to_string(),
            },
        );

        let current = queue
            .drain_actions(viewport_id, 10)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();
        let next = queue
            .drain_actions(viewport_id, 11)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();
        let later = queue
            .drain_actions(viewport_id, 12)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();

        assert_eq!(current, vec!["current".to_string()]);
        assert_eq!(next, vec!["next".to_string()]);
        assert_eq!(later, vec!["later".to_string()]);
        assert_eq!(queue.stats(viewport_id).queued_actions, 3);
        assert_eq!(queue.stats(viewport_id).drained_actions, 3);
        assert_eq!(queue.stats(viewport_id).last_drain_frame, Some(12));
        assert!(!queue.has_pending_actions(viewport_id));
    }

    #[test]
    fn newly_staged_actions_do_not_inherit_an_older_actions_delay() {
        let queue = ActionQueue::new();
        let viewport_id = egui::ViewportId::ROOT;
        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::AfterTwoFrames,
            InputAction::Text {
                text: "older".to_string(),
            },
        );
        assert!(queue.drain_actions(viewport_id, 10).is_empty());
        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::AfterOneFrame,
            InputAction::Text {
                text: "newer".to_string(),
            },
        );
        assert!(queue.drain_actions(viewport_id, 11).is_empty());
        assert!(queue.drain_actions(viewport_id, 11).is_empty());
        let ready = queue
            .drain_actions(viewport_id, 12)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();
        assert_eq!(ready, vec!["older", "newer"]);
        assert!(!queue.has_pending_actions(viewport_id));
    }

    #[test]
    fn empty_drains_do_not_update_last_drain_frame() {
        let queue = ActionQueue::new();
        let viewport_id = egui::ViewportId::ROOT;

        assert!(queue.drain_actions(viewport_id, 10).is_empty());
        assert_eq!(queue.stats(viewport_id).last_drain_frame, None);

        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::Immediate,
            InputAction::Text {
                text: "typed".to_string(),
            },
        );
        assert_eq!(queue.drain_actions(viewport_id, 11).len(), 1);
        assert_eq!(queue.stats(viewport_id).last_drain_frame, Some(11));

        assert!(queue.drain_actions(viewport_id, 12).is_empty());
        assert_eq!(queue.stats(viewport_id).last_drain_frame, Some(11));
    }

    #[test]
    fn drain_actions_promotes_once_per_frame_number() {
        let queue = ActionQueue::new();
        let viewport_id = egui::ViewportId::ROOT;
        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::Immediate,
            InputAction::Text {
                text: "now".to_string(),
            },
        );
        queue.queue_action_with_timing(
            viewport_id,
            ActionTiming::AfterOneFrame,
            InputAction::Text {
                text: "next".to_string(),
            },
        );

        let first = queue
            .drain_actions(viewport_id, 10)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();
        let second = queue
            .drain_actions(viewport_id, 10)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();
        let third = queue
            .drain_actions(viewport_id, 11)
            .into_iter()
            .map(text_payload)
            .collect::<Vec<_>>();

        assert_eq!(first, vec!["now".to_string()]);
        assert!(second.is_empty());
        assert_eq!(third, vec!["next".to_string()]);
    }
}
