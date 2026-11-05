//! Input events, modifiers, and the options of actions and waits.

use serde::{Deserialize, Serialize};

use super::*;

/// Keyboard modifier state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct Modifiers {
    /// Ctrl key pressed.
    #[serde(default)]
    pub ctrl: bool,
    /// Shift key pressed.
    #[serde(default)]
    pub shift: bool,
    /// Alt key pressed.
    #[serde(default)]
    pub alt: bool,
    /// Command key pressed.
    #[serde(default)]
    pub command: bool,
}

impl From<egui::Modifiers> for Modifiers {
    fn from(modifiers: egui::Modifiers) -> Self {
        Self {
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
            command: modifiers.command,
        }
    }
}

/// Timeout and polling controls shared by waits and high-level actions.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct WaitOptions {
    /// Optional operation timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Optional interval between fresh-capture polls in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_ms: Option<u64>,
}

/// Controls shared by high-level actions.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ActionOptions {
    /// Wait controls used before and after the action.
    #[serde(flatten)]
    pub wait: WaitOptions,
    /// Whether to wait for deterministic settlement after queuing the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<bool>,
}

/// Pointer button used by clicks and raw pointer events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    /// Primary pointer button.
    Primary,
    /// Secondary pointer button.
    Secondary,
    /// Middle pointer button.
    Middle,
}

/// Options for a drag action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DragOptions {
    /// Shared action controls.
    #[serde(flatten)]
    pub action: ActionOptions,
    /// Optional normalized starting position within the source widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Vec2>,
}

/// Coarse alignment used by scroll-to actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScrollAlign {
    /// Align to the top.
    Top,
    /// Align to the center.
    Center,
    /// Align to the bottom.
    Bottom,
}

/// One resize command applied atomically before settlement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResizeOptions {
    /// Shared action controls.
    #[serde(flatten)]
    pub action: ActionOptions,
    /// Requested inner viewport size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_size: Option<Vec2>,
    /// Requested minimum inner size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<Vec2>,
    /// Requested maximum inner size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<Vec2>,
    /// Requested resize increments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub increments: Option<Vec2>,
    /// Whether the viewport is user-resizable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resizable: Option<bool>,
}

/// Press or release action used by raw input events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RawInputAction {
    /// Press the key or pointer button.
    Press,
    /// Release the key or pointer button.
    Release,
}

/// One raw input event. Raw input queues exactly one event and does not settle.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RawInputEvent {
    /// Move the pointer to an absolute egui position.
    PointerMove {
        /// Absolute egui position.
        position: Pos2,
    },
    /// Press or release one pointer button.
    PointerButton {
        /// Absolute egui position.
        position: Pos2,
        /// Pointer button.
        button: PointerButton,
        /// Press or release action.
        action: RawInputAction,
        /// Optional modifier state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifiers: Option<Modifiers>,
    },
    /// Press or release one key.
    Key {
        /// Egui key name.
        key: String,
        /// Press or release action.
        action: RawInputAction,
        /// Optional modifier state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifiers: Option<Modifiers>,
    },
    /// Insert one raw text event.
    Text {
        /// Text payload.
        text: String,
    },
    /// Insert one raw scroll event.
    Scroll {
        /// Scroll delta in egui points.
        delta: Vec2,
        /// Optional modifier state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifiers: Option<Modifiers>,
    },
}

impl From<Modifiers> for egui::Modifiers {
    fn from(modifiers: Modifiers) -> Self {
        Self {
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
            command: modifiers.command,
            mac_cmd: modifiers.command,
        }
    }
}
