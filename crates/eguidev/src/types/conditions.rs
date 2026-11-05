//! The shared conditions of waits, assertions, actions, and fixtures.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::*;

/// A scroll position required by a [`WidgetCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScrollAtCondition {
    /// Target logical scroll offset.
    pub offset: Vec2,
    /// Allowed absolute error per axis.
    #[serde(default = "default_scroll_tolerance")]
    pub tolerance: f32,
}

/// A JSON value required by a [`WidgetCondition`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DataCondition {
    /// RFC 6901 pointer into the widget's `data` value. Empty selects all.
    pub pointer: String,
    /// Value the pointer must resolve to.
    pub equals: serde_json::Value,
}

/// Shared declarative condition used by waits, assertions, actions, and
/// fixtures.
///
/// Every populated field must match. An empty condition means `present = true`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetCondition {
    /// Whether the widget must be present or absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,
    /// Whether the widget must satisfy standard click actionability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionable: Option<bool>,
    /// Required visibility state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Required coverage state: whether another egui layer covers the
    /// widget's action point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered: Option<bool>,
    /// Required enabled state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Required keyboard-focus state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// Required selection state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// Required semantic widget role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<WidgetRole>,
    /// Required exact label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Required label substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_contains: Option<String>,
    /// Required exact widget value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<WidgetValue>,
    /// Required substring in the value's text projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_text_contains: Option<String>,
    /// Whether scroll state must be initialized and stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_ready: Option<bool>,
    /// Required stable scroll position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_at: Option<ScrollAtCondition>,
    /// Required value inside widget data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<DataCondition>,
}

/// Shared declarative condition for viewport waits and assertions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewportCondition {
    /// Whether the viewport must be present or absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,
    /// Required semantic viewport name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Required exact window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Required window-title substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    /// Required focus state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// Required minimized state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimized: Option<bool>,
    /// Required occlusion state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occluded: Option<bool>,
    /// Required observed native title visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_title_visible: Option<bool>,
    /// Required cursor icon requested by the viewport's latest frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_icon: Option<String>,
    /// Required maximized state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<bool>,
    /// Required fullscreen state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    /// Minimum captured viewport frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_at_least: Option<u64>,
}

const fn default_scroll_tolerance() -> f32 {
    0.5
}

/// Whether a string is a well-formed RFC 6901 JSON pointer.
///
/// An empty pointer selects the whole document. Any other pointer starts with
/// `/`, and `~` only introduces the `~0` and `~1` escapes.
///
/// Keep aligned with `isValidJsonPointer` in `eguidev.luau`.
fn is_valid_json_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(current) = chars.next() {
        if current == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

impl WidgetCondition {
    /// Require the widget to exist.
    pub fn present() -> Self {
        Self {
            present: Some(true),
            ..Self::default()
        }
    }

    /// Require the widget to exist and be visible.
    pub fn visible() -> Self {
        Self {
            visible: Some(true),
            ..Self::default()
        }
    }

    /// Require an exact widget label.
    pub fn label(label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            ..Self::default()
        }
    }

    /// Require an exact widget value.
    pub fn value(value: WidgetValue) -> Self {
        Self {
            value: Some(value),
            ..Self::default()
        }
    }

    /// Require stable, initialized scroll metadata.
    pub fn scroll_ready() -> Self {
        Self {
            scroll_ready: Some(true),
            ..Self::default()
        }
    }

    /// Require stable scroll metadata at the requested offset.
    pub fn scroll_at(offset: impl Into<Vec2>, tolerance: f32) -> Self {
        Self {
            scroll_at: Some(ScrollAtCondition {
                offset: offset.into(),
                tolerance,
            }),
            ..Self::default()
        }
    }

    /// Require a widget-data value at an RFC 6901 pointer.
    pub fn data(pointer: impl Into<String>, equals: impl Into<serde_json::Value>) -> Self {
        Self {
            data: Some(DataCondition {
                pointer: pointer.into(),
                equals: equals.into(),
            }),
            ..Self::default()
        }
    }

    /// Validate combinations and nested condition values.
    pub fn validate(&self) -> Result<(), String> {
        let has_state_field = self.actionable.is_some()
            || self.visible.is_some()
            || self.covered.is_some()
            || self.enabled.is_some()
            || self.focused.is_some()
            || self.selected.is_some()
            || self.role.is_some()
            || self.label.is_some()
            || self.label_contains.is_some()
            || self.value.is_some()
            || self.value_text_contains.is_some()
            || self.scroll_ready.is_some()
            || self.scroll_at.is_some()
            || self.data.is_some();
        if self.present == Some(false) && has_state_field {
            return Err("present = false cannot be combined with a state condition".to_string());
        }
        if self.label.as_ref().is_some_and(String::is_empty) {
            return Err("label must not be empty".to_string());
        }
        if self.label_contains.as_ref().is_some_and(String::is_empty) {
            return Err("label_contains must not be empty".to_string());
        }
        if self
            .value_text_contains
            .as_ref()
            .is_some_and(String::is_empty)
        {
            return Err("value_text_contains must not be empty".to_string());
        }
        if let Some(scroll_at) = self.scroll_at
            && (!scroll_at.tolerance.is_finite() || scroll_at.tolerance <= 0.0)
        {
            return Err("scroll_at tolerance must be finite and greater than 0".to_string());
        }
        if let Some(data) = &self.data
            && !is_valid_json_pointer(&data.pointer)
        {
            return Err(format!(
                "data pointer {:?} is not an RFC 6901 JSON pointer",
                data.pointer
            ));
        }
        Ok(())
    }
}

impl ViewportCondition {
    /// Validate combinations and string conditions.
    pub fn validate(&self) -> Result<(), String> {
        let has_state_field = self.name.is_some()
            || self.title.is_some()
            || self.title_contains.is_some()
            || self.focused.is_some()
            || self.minimized.is_some()
            || self.occluded.is_some()
            || self.os_title_visible.is_some()
            || self.cursor_icon.is_some()
            || self.maximized.is_some()
            || self.fullscreen.is_some()
            || self.frame_at_least.is_some();
        if self.present == Some(false) && has_state_field {
            return Err("present = false cannot be combined with a state condition".to_string());
        }
        if self.name.as_ref().is_some_and(String::is_empty) {
            return Err("name must not be empty".to_string());
        }
        if self.title.as_ref().is_some_and(String::is_empty) {
            return Err("title must not be empty".to_string());
        }
        if self.title_contains.as_ref().is_some_and(String::is_empty) {
            return Err("title_contains must not be empty".to_string());
        }
        if self.cursor_icon.as_ref().is_some_and(String::is_empty) {
            return Err("cursor_icon must not be empty".to_string());
        }
        Ok(())
    }
}

impl fmt::Display for WidgetCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = serde_json::to_string(self).map_err(|_| fmt::Error)?;
        f.write_str(&value)
    }
}
