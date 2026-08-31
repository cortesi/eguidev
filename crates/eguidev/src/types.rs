//! Data types used by DevMCP tooling.

use std::{
    any::Any,
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use egui::{Rect as EguiRect, Vec2 as EguiVec2};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{
    Deserialize, Serialize,
    de::{self, Deserializer},
    ser::Serializer,
};

use crate::registry::viewport_id_to_string;

/// Error returned when a semantic viewport name is reserved or invalid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewportNameError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

impl ViewportNameError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ViewportNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ViewportNameError {}

/// Error returned when parsing a viewport selector string fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewportSelParseError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

impl ViewportSelParseError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ViewportSelParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ViewportSelParseError {}

/// Explicit selector for a viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportSel {
    kind: ViewportSelKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ViewportSelKind {
    Root,
    Id(egui::ViewportId),
    RawId(u64),
    Name(String),
}

impl ViewportSel {
    /// Select the root viewport.
    pub fn root() -> Self {
        Self {
            kind: ViewportSelKind::Root,
        }
    }

    /// Select a concrete egui viewport id.
    pub fn id(id: egui::ViewportId) -> Self {
        Self {
            kind: ViewportSelKind::Id(id),
        }
    }

    /// Select a semantic viewport name.
    pub fn name(name: impl Into<String>) -> Result<Self, ViewportNameError> {
        let name = name.into();
        validate_viewport_name(&name)?;
        Ok(Self {
            kind: ViewportSelKind::Name(name),
        })
    }

    /// Parse the Luau/tool selector grammar: `root`, a semantic name, or
    /// `vp:<hex>`.
    pub fn parse(selector: impl AsRef<str>) -> Result<Self, ViewportSelParseError> {
        let selector = selector.as_ref();
        if selector.trim().is_empty() {
            return Err(ViewportSelParseError::new(
                "empty_viewport_selector",
                "viewport selector must not be empty",
            ));
        }
        if selector == "root" {
            return Ok(Self::root());
        }
        if let Some(raw) = selector.strip_prefix("vp:") {
            if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(ViewportSelParseError::new(
                    "invalid_viewport_id",
                    format!("viewport id selector `{selector}` must be `vp:<hex>`"),
                ));
            }
            let raw_id = u64::from_str_radix(raw, 16).map_err(|_| {
                ViewportSelParseError::new(
                    "invalid_viewport_id",
                    format!("viewport id selector `{selector}` must be `vp:<hex>`"),
                )
            })?;
            return Ok(Self {
                kind: ViewportSelKind::RawId(raw_id),
            });
        }
        validate_viewport_name(selector).map_err(|error| {
            ViewportSelParseError::new(
                error.code,
                format!("invalid viewport name: {}", error.message),
            )
        })?;
        Ok(Self {
            kind: ViewportSelKind::Name(selector.to_string()),
        })
    }

    /// Return the canonical string selector used in fixtures and scripts.
    pub fn to_selector_string(&self) -> String {
        match &self.kind {
            ViewportSelKind::Root => "root".to_string(),
            ViewportSelKind::Id(id) => viewport_id_to_string(*id),
            ViewportSelKind::RawId(raw_id) => format!("vp:{raw_id:x}"),
            ViewportSelKind::Name(name) => name.clone(),
        }
    }
}

impl From<egui::ViewportId> for ViewportSel {
    fn from(value: egui::ViewportId) -> Self {
        Self::id(value)
    }
}

pub fn validate_viewport_name(name: &str) -> Result<(), ViewportNameError> {
    if name.trim().is_empty() {
        return Err(ViewportNameError::new(
            "empty_viewport_name",
            "viewport name must not be empty",
        ));
    }
    if name == "root" || name.starts_with("vp:") {
        return Err(ViewportNameError::new(
            "reserved_viewport_name",
            format!("viewport name `{name}` is reserved"),
        ));
    }
    Ok(())
}

fn sanitize_f32(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// A logical point in egui coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Pos2 {
    /// X coordinate in points.
    pub x: f32,
    /// Y coordinate in points.
    pub y: f32,
}

impl From<egui::Pos2> for Pos2 {
    fn from(pos: egui::Pos2) -> Self {
        Self {
            x: sanitize_f32(pos.x),
            y: sanitize_f32(pos.y),
        }
    }
}

impl From<Pos2> for egui::Pos2 {
    fn from(pos: Pos2) -> Self {
        egui::pos2(pos.x, pos.y)
    }
}

/// A 2D vector in egui coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Vec2 {
    /// X component in points.
    pub x: f32,
    /// Y component in points.
    pub y: f32,
}

impl From<EguiVec2> for Vec2 {
    fn from(vec: EguiVec2) -> Self {
        Self {
            x: sanitize_f32(vec.x),
            y: sanitize_f32(vec.y),
        }
    }
}

impl From<Vec2> for EguiVec2 {
    fn from(vec: Vec2) -> Self {
        egui::vec2(vec.x, vec.y)
    }
}

/// Axis-aligned rectangle in egui coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Rect {
    /// Minimum point.
    pub min: Pos2,
    /// Maximum point.
    pub max: Pos2,
}

impl Rect {
    /// Return the center point of the rectangle in egui coordinates.
    pub fn center(self) -> Pos2 {
        Pos2 {
            x: (self.min.x + self.max.x) * 0.5,
            y: (self.min.y + self.max.y) * 0.5,
        }
    }
}

impl From<EguiRect> for Rect {
    fn from(rect: EguiRect) -> Self {
        Self {
            min: Pos2::from(rect.min),
            max: Pos2::from(rect.max),
        }
    }
}

impl From<Rect> for EguiRect {
    fn from(rect: Rect) -> Self {
        Self::from_min_max(rect.min.into(), rect.max.into())
    }
}

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

/// Fixture metadata advertised by an app.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureSpec {
    /// Fixture name.
    pub name: String,
    /// Fixture description.
    pub description: String,
    /// Declarative conditions that must be satisfied before fixture
    /// application.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<FixtureTargetSpec>,
    /// Declarative ready conditions for the fixture baseline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ready: Vec<FixtureTargetSpec>,
    /// Typed scalar params accepted by this fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<FixtureParam>,
    /// Searchable fixture categories used by docs and CLI output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// One widget target and condition in a fixture readiness contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureTargetSpec {
    /// Widget id to resolve from the registry.
    pub widget_id: String,
    /// Optional viewport selector (`root`, semantic name, or `vp:...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_id: Option<String>,
    /// Readiness condition to evaluate against the widget state.
    pub condition: WidgetCondition,
}

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

/// Supported scalar kinds for fixture parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// Boolean fixture parameter.
    Bool,
    /// Signed integer fixture parameter.
    Int,
    /// Floating-point fixture parameter.
    Float,
    /// String fixture parameter.
    Text,
}

/// One typed fixture parameter in a fixture catalog entry.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureParam {
    /// Parameter name.
    pub name: String,
    /// Parameter scalar kind.
    pub kind: ParamKind,
    /// Human-readable parameter description.
    pub description: String,
    /// Optional default. Missing default means the caller must supply the
    /// param.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<WidgetValue>,
    /// Optional exact allowed values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<WidgetValue>,
    /// Optional inclusive minimum for int/float params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Optional inclusive maximum for int/float params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// Validated fixture params passed to a handler.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureParams(BTreeMap<String, WidgetValue>);

/// Fixture call passed to a registered handler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureCall {
    /// Fixture name.
    pub name: String,
    /// Validated params with defaults filled in.
    pub params: FixtureParams,
}

/// Successful fixture handler response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureResponse {
    /// Handler-returned values exposed to scripts and CLI output.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, WidgetValue>,
    /// Handler-returned dynamic ready waited on together with the spec ready.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ready: Vec<FixtureTargetSpec>,
}

/// Result returned by a fixture handler.
pub type FixtureResult = Result<FixtureResponse, FixtureError>;

/// Structured fixture handler failure.
///
/// A handler chooses its own `code` values, such as `unknown_fixture`.
/// `eguidev` produces these itself before or around the handler:
///
/// - `no_handler`: the app registered no fixture handler.
/// - `unknown_param`, `missing_param`, `invalid_param_type`,
///   `invalid_param_choice`, `param_below_min`, `param_above_max`: the call did
///   not satisfy the fixture's declared params, and `details` names the param.
/// - `timeout`: a UI-thread handler did not run before the deadline.
/// - `panic`: the handler panicked, and `message` carries the payload.
/// - `internal`: the handler was dropped without returning a result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Optional machine-readable error details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl FixtureSpec {
    /// Create a new fixture specification.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            preconditions: Vec::new(),
            ready: Vec::new(),
            params: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// Add a visible-widget precondition checked before fixture application.
    pub fn precondition(self, widget_id: impl Into<String>) -> Self {
        self.push_precondition(widget_id.into(), None, WidgetCondition::visible())
    }

    /// Add a visible-widget precondition scoped to a viewport.
    pub fn precondition_in(
        self,
        widget_id: impl Into<String>,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_precondition(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::visible(),
        )
    }

    /// Add an exact-value precondition checked before fixture application.
    pub fn precondition_value(self, widget_id: impl Into<String>, value: WidgetValue) -> Self {
        self.push_precondition(widget_id.into(), None, WidgetCondition::value(value))
    }

    /// Add an exact-value precondition scoped to a viewport.
    pub fn precondition_value_in(
        self,
        widget_id: impl Into<String>,
        value: WidgetValue,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_precondition(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::value(value),
        )
    }

    /// Add a visible-widget ready condition.
    pub fn ready(self, widget_id: impl Into<String>) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::visible())
    }

    /// Add a visible-widget ready condition scoped to a viewport.
    pub fn ready_in(self, widget_id: impl Into<String>, viewport: impl Into<ViewportSel>) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::visible(),
        )
    }

    /// Add an exact-label ready condition.
    pub fn ready_label(self, widget_id: impl Into<String>, text: impl Into<String>) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::label(text))
    }

    /// Add an exact-label ready condition scoped to a viewport.
    pub fn ready_label_in(
        self,
        widget_id: impl Into<String>,
        text: impl Into<String>,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::label(text),
        )
    }

    /// Add an exact-value ready condition.
    pub fn ready_value(self, widget_id: impl Into<String>, value: WidgetValue) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::value(value))
    }

    /// Add an exact-value ready condition scoped to a viewport.
    pub fn ready_value_in(
        self,
        widget_id: impl Into<String>,
        value: WidgetValue,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::value(value),
        )
    }

    /// Add a scroll-readiness condition.
    pub fn ready_scroll(self, widget_id: impl Into<String>) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::scroll_ready())
    }

    /// Add a scroll-readiness condition scoped to a viewport.
    pub fn ready_scroll_in(
        self,
        widget_id: impl Into<String>,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::scroll_ready(),
        )
    }

    /// Add a scroll-position ready condition.
    pub fn ready_scroll_at(
        self,
        widget_id: impl Into<String>,
        offset: impl Into<Vec2>,
        tolerance: f32,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            None,
            WidgetCondition::scroll_at(offset, tolerance),
        )
    }

    /// Add a scroll-position ready condition scoped to a viewport.
    pub fn ready_scroll_at_in(
        self,
        widget_id: impl Into<String>,
        offset: impl Into<Vec2>,
        tolerance: f32,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::scroll_at(offset, tolerance),
        )
    }

    /// Add a widget-data ready condition.
    pub fn ready_data(
        self,
        widget_id: impl Into<String>,
        pointer: impl Into<String>,
        equals: impl Into<serde_json::Value>,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            None,
            WidgetCondition::data(pointer, equals),
        )
    }

    /// Add a widget-data ready condition scoped to a viewport.
    pub fn ready_data_in(
        self,
        widget_id: impl Into<String>,
        pointer: impl Into<String>,
        equals: impl Into<serde_json::Value>,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::data(pointer, equals),
        )
    }

    /// Add a typed fixture parameter.
    pub fn param(mut self, param: FixtureParam) -> Self {
        self.params.push(param);
        self
    }

    /// Add a fixture tag used by CLI/docs output.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Return whether this fixture is a transition with entry requirements.
    pub fn is_transition(&self) -> bool {
        !self.preconditions.is_empty()
    }

    /// Validate fixture metadata and readiness conditions.
    pub fn validate(&self, require_ready: bool) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("fixture name must not be empty".to_string());
        }
        if self.description.trim().is_empty() {
            return Err(format!(
                "fixture {} description must not be empty",
                self.name
            ));
        }
        let mut param_names = BTreeSet::new();
        for (index, param) in self.params.iter().enumerate() {
            param
                .validate()
                .map_err(|error| format!("fixture {} param {}: {error}", self.name, index + 1))?;
            if !param_names.insert(param.name.as_str()) {
                return Err(format!(
                    "fixture {} duplicate param name: {}",
                    self.name, param.name
                ));
            }
        }
        let mut tags = BTreeSet::new();
        for tag in &self.tags {
            if tag.trim().is_empty() {
                return Err(format!("fixture {} tag must not be empty", self.name));
            }
            if !tags.insert(tag.as_str()) {
                return Err(format!("fixture {} duplicate tag: {tag}", self.name));
            }
        }
        for (index, ready) in self.preconditions.iter().enumerate() {
            ready.validate().map_err(|error| {
                format!("fixture {} precondition {}: {error}", self.name, index + 1)
            })?;
        }
        for (index, ready) in self.ready.iter().enumerate() {
            ready
                .validate()
                .map_err(|error| format!("fixture {} ready {}: {error}", self.name, index + 1))?;
        }
        if require_ready && self.ready.is_empty() {
            return Err(format!(
                "fixture {} must declare at least one ready condition",
                self.name
            ));
        }
        Ok(())
    }

    /// Validate and normalize a caller-supplied param map.
    pub fn validate_params(
        &self,
        mut supplied: BTreeMap<String, WidgetValue>,
    ) -> Result<FixtureParams, FixtureError> {
        let mut values = BTreeMap::new();
        if let Some(name) = supplied
            .keys()
            .find(|name| !self.params.iter().any(|param| param.name == **name))
            .cloned()
        {
            return Err(FixtureError::new(
                "unknown_param",
                format!("unknown param {name:?} for fixture {}", self.name),
            )
            .details(serde_json::json!({
                "fixture": self.name,
                "param": name,
                "allowed": self.params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
            })));
        }
        for param in &self.params {
            let value = match supplied.remove(&param.name) {
                Some(value) => value,
                None => match &param.default {
                    Some(value) => value.clone(),
                    None => {
                        return Err(FixtureError::new(
                            "missing_param",
                            format!(
                                "missing required param {:?} for fixture {}",
                                param.name, self.name
                            ),
                        )
                        .details(serde_json::json!({
                            "fixture": self.name,
                            "param": param.name,
                            "kind": param.kind.as_str(),
                        })));
                    }
                },
            };
            let value = param.normalize_value(value)?;
            values.insert(param.name.clone(), value);
        }
        Ok(FixtureParams(values))
    }

    /// Return a human-readable summary of the readiness contract.
    pub fn describe_readiness(&self) -> String {
        let preconditions = self
            .preconditions
            .iter()
            .map(FixtureTargetSpec::describe)
            .collect::<Vec<_>>();
        let ready = self
            .ready
            .iter()
            .map(FixtureTargetSpec::describe)
            .collect::<Vec<_>>();
        match (preconditions.is_empty(), ready.is_empty()) {
            (true, true) => "No readiness conditions declared.".to_string(),
            (true, false) => ready.join("; "),
            (false, true) => format!("preconditions: {}", preconditions.join("; ")),
            (false, false) => format!(
                "preconditions: {}; ready: {}",
                preconditions.join("; "),
                ready.join("; ")
            ),
        }
    }

    fn push_precondition(
        mut self,
        widget_id: String,
        viewport_id: Option<String>,
        condition: WidgetCondition,
    ) -> Self {
        self.preconditions.push(FixtureTargetSpec {
            widget_id,
            viewport_id,
            condition,
        });
        self
    }

    fn push_ready(
        mut self,
        widget_id: String,
        viewport_id: Option<String>,
        condition: WidgetCondition,
    ) -> Self {
        self.ready.push(FixtureTargetSpec {
            widget_id,
            viewport_id,
            condition,
        });
        self
    }
}

impl FixtureParam {
    /// Create a boolean fixture parameter.
    pub fn bool(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(name, ParamKind::Bool, description)
    }

    /// Create an integer fixture parameter.
    pub fn int(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(name, ParamKind::Int, description)
    }

    /// Create a floating-point fixture parameter.
    pub fn float(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(name, ParamKind::Float, description)
    }

    /// Create a text fixture parameter.
    pub fn text(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(name, ParamKind::Text, description)
    }

    fn new(name: impl Into<String>, kind: ParamKind, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind,
            description: description.into(),
            default: None,
            choices: Vec::new(),
            min: None,
            max: None,
        }
    }

    /// Set this parameter's default value.
    pub fn default(mut self, value: impl Into<WidgetValue>) -> Self {
        self.default = Some(self.normalize_literal(value.into()));
        self
    }

    /// Restrict this parameter to exact choices.
    pub fn choices<I, V>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Into<WidgetValue>,
    {
        self.choices = values
            .into_iter()
            .map(Into::into)
            .map(|value| self.normalize_literal(value))
            .collect();
        self
    }

    /// Set an inclusive numeric range.
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name must not be empty".to_string());
        }
        if self.description.trim().is_empty() {
            return Err(format!("param {} description must not be empty", self.name));
        }
        if (self.min.is_some() || self.max.is_some())
            && !matches!(self.kind, ParamKind::Int | ParamKind::Float)
        {
            return Err(format!(
                "param {} has a range but is not numeric",
                self.name
            ));
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            if !min.is_finite() || !max.is_finite() {
                return Err(format!("param {} range must be finite", self.name));
            }
            if min > max {
                return Err(format!("param {} range min exceeds max", self.name));
            }
        }
        if let Some(default) = &self.default {
            self.normalize_value(default.clone())
                .map_err(|error| error.message)?;
        }
        for choice in &self.choices {
            self.normalize_value(choice.clone())
                .map_err(|error| error.message)?;
        }
        Ok(())
    }

    fn normalize_literal(&self, value: WidgetValue) -> WidgetValue {
        match (self.kind, value) {
            (ParamKind::Float, WidgetValue::Int(value)) => WidgetValue::Float(value as f64),
            (_, value) => value,
        }
    }

    fn validate_value(&self, value: &WidgetValue) -> Result<(), FixtureError> {
        if !self.kind.matches(value) {
            return Err(FixtureError::new(
                "invalid_param_type",
                format!(
                    "param {:?} expected {}, got {}",
                    self.name,
                    self.kind.as_str(),
                    value.kind_name()
                ),
            )
            .details(serde_json::json!({
                "param": self.name,
                "expected": self.kind.as_str(),
                "actual": value.kind_name(),
            })));
        }
        if !self.choices.is_empty() && !self.choices.iter().any(|choice| choice == value) {
            return Err(FixtureError::new(
                "invalid_param_choice",
                format!(
                    "param {:?} value is not one of its allowed choices",
                    self.name
                ),
            )
            .details(serde_json::json!({
                "param": self.name,
                "value": value,
                "choices": self.choices,
            })));
        }
        if let Some(number) = value.as_f64() {
            if let Some(min) = self.min
                && number < min
            {
                return Err(FixtureError::new(
                    "param_below_min",
                    format!("param {:?} must be >= {min}", self.name),
                )
                .details(serde_json::json!({
                    "param": self.name,
                    "value": value,
                    "min": min,
                })));
            }
            if let Some(max) = self.max
                && number > max
            {
                return Err(FixtureError::new(
                    "param_above_max",
                    format!("param {:?} must be <= {max}", self.name),
                )
                .details(serde_json::json!({
                    "param": self.name,
                    "value": value,
                    "max": max,
                })));
            }
        }
        Ok(())
    }

    fn normalize_value(&self, value: WidgetValue) -> Result<WidgetValue, FixtureError> {
        let value = self.normalize_literal(value);
        self.validate_value(&value)?;
        Ok(value)
    }
}

impl ParamKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Text => "text",
        }
    }

    fn matches(self, value: &WidgetValue) -> bool {
        matches!(
            (self, value),
            (Self::Bool, WidgetValue::Bool(_))
                | (Self::Int, WidgetValue::Int(_))
                | (Self::Float, WidgetValue::Float(_))
                | (Self::Text, WidgetValue::Text(_))
        )
    }
}

impl FixtureParams {
    /// Get a bool param by name.
    pub fn bool(&self, name: &str) -> bool {
        match self.0.get(name) {
            Some(WidgetValue::Bool(value)) => *value,
            Some(_) => panic!("fixture param {name:?} is not a bool"),
            None => panic!("fixture param {name:?} is not declared"),
        }
    }

    /// Get an int param by name.
    pub fn int(&self, name: &str) -> i64 {
        match self.0.get(name) {
            Some(WidgetValue::Int(value)) => *value,
            Some(_) => panic!("fixture param {name:?} is not an int"),
            None => panic!("fixture param {name:?} is not declared"),
        }
    }

    /// Get a float param by name.
    pub fn float(&self, name: &str) -> f64 {
        match self.0.get(name) {
            Some(WidgetValue::Float(value)) => *value,
            Some(_) => panic!("fixture param {name:?} is not a float"),
            None => panic!("fixture param {name:?} is not declared"),
        }
    }

    /// Get a text param by name.
    pub fn text(&self, name: &str) -> &str {
        match self.0.get(name) {
            Some(WidgetValue::Text(value)) => value,
            Some(_) => panic!("fixture param {name:?} is not text"),
            None => panic!("fixture param {name:?} is not declared"),
        }
    }

    /// Get a param by name.
    pub fn get(&self, name: &str) -> Option<&WidgetValue> {
        self.0.get(name)
    }

    /// Return the validated params as a map.
    pub fn as_map(&self) -> &BTreeMap<String, WidgetValue> {
        &self.0
    }

    /// Consume this wrapper into the validated param map.
    pub fn into_map(self) -> BTreeMap<String, WidgetValue> {
        self.0
    }
}

impl FixtureResponse {
    /// Create an empty fixture response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a handler-returned value.
    pub fn value(mut self, name: impl Into<String>, value: impl Into<WidgetValue>) -> Self {
        self.values.insert(name.into(), value.into());
        self
    }

    /// Add a handler-returned dynamic ready.
    pub fn ready(mut self, ready: FixtureTargetSpec) -> Self {
        self.ready.push(ready);
        self
    }
}

impl FixtureError {
    /// Create a fixture error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Attach structured error details.
    pub fn details<T: serde::Serialize>(mut self, details: T) -> Self {
        self.details = serde_json::to_value(details).ok();
        self
    }

    pub(crate) fn handler_panic(name: &str, panic: &(dyn Any + Send)) -> Self {
        Self::new(
            "panic",
            format!(
                "fixture handler {name:?} panicked: {}",
                panic_message(panic)
            ),
        )
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
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
        Ok(())
    }
}

impl FixtureTargetSpec {
    /// Create a fixture target with an explicit shared condition.
    pub fn new(widget_id: impl Into<String>, condition: WidgetCondition) -> Self {
        Self {
            widget_id: widget_id.into(),
            viewport_id: None,
            condition,
        }
    }

    /// Create a visible-widget readiness target.
    pub fn visible(widget_id: impl Into<String>) -> Self {
        Self::new(widget_id, WidgetCondition::visible())
    }

    /// Create an exact-label ready.
    pub fn label(widget_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(widget_id, WidgetCondition::label(label))
    }

    /// Create an exact-value ready.
    pub fn value(widget_id: impl Into<String>, value: impl Into<WidgetValue>) -> Self {
        Self::new(widget_id, WidgetCondition::value(value.into()))
    }

    /// Create a scroll-readiness target.
    pub fn scroll_ready(widget_id: impl Into<String>) -> Self {
        Self::new(widget_id, WidgetCondition::scroll_ready())
    }

    /// Create a scroll-position ready.
    pub fn scroll_at(
        widget_id: impl Into<String>,
        offset: impl Into<Vec2>,
        tolerance: f32,
    ) -> Self {
        Self::new(widget_id, WidgetCondition::scroll_at(offset, tolerance))
    }

    /// Create a widget-data ready.
    pub fn data(
        widget_id: impl Into<String>,
        pointer: impl Into<String>,
        equals: impl Into<serde_json::Value>,
    ) -> Self {
        Self::new(widget_id, WidgetCondition::data(pointer, equals))
    }

    /// Scope this ready to a viewport selector.
    pub fn in_viewport(mut self, viewport: impl Into<ViewportSel>) -> Self {
        self.viewport_id = Some(viewport.into().to_selector_string());
        self
    }

    /// Return a human-readable description of the ready.
    pub fn describe(&self) -> String {
        let target = match &self.viewport_id {
            Some(viewport_id) => format!("{} in {}", self.widget_id, viewport_id),
            None => self.widget_id.clone(),
        };
        format!("{target} {}", self.condition)
    }

    /// Validate the ready contents.
    pub fn validate(&self) -> Result<(), String> {
        if self.widget_id.trim().is_empty() {
            return Err("widget_id must not be empty".to_string());
        }
        if let Some(viewport_id) = &self.viewport_id
            && viewport_id.trim().is_empty()
        {
            return Err("viewport_id must not be empty when provided".to_string());
        }
        self.condition.validate()
    }
}

impl fmt::Display for WidgetCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = serde_json::to_string(self).map_err(|_| fmt::Error)?;
        f.write_str(&value)
    }
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

/// Widget reference used in tool calls.
///
/// Matching rules:
/// - `id` is the canonical widget selector.
/// - `viewport_id` acts as an additional selector to narrow matches.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetRef {
    /// Canonical widget id.
    ///
    /// If instrumentation provides an explicit id, eguidev uses it verbatim.
    /// Otherwise eguidev generates an opaque hex id that is best-effort stable
    /// within the current app session.
    pub id: String,
    /// Optional viewport selector (`root` or `vp:...`).
    #[serde(default)]
    pub viewport_id: Option<String>,
}

/// Widget role taxonomy for automation and scripting filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum WidgetRole {
    Button,
    Link,
    Image,
    Label,
    TextEdit,
    Slider,
    Checkbox,
    ComboBox,
    Radio,
    DragValue,
    Toggle,
    Selectable,
    Separator,
    Spinner,
    ScrollArea,
    MenuButton,
    CollapsingHeader,
    Window,
    ProgressBar,
    ColorPicker,
    #[default]
    Unknown,
}

/// Captured widget value for stateful controls.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetValue {
    /// Boolean value from checkboxes/toggles.
    Bool(bool),
    /// Floating-point value from sliders/drag values.
    Float(f64),
    /// Integer value from drag values/combos.
    Int(i64),
    /// Text value from text edits.
    Text(String),
}

impl WidgetValue {
    /// String representation matching Luau `tostring()` semantics.
    pub fn to_text(&self) -> String {
        match self {
            Self::Bool(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Int(v) => v.to_string(),
            Self::Text(v) => v.clone(),
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Float(_) => "float",
            Self::Int(_) => "int",
            Self::Text(_) => "text",
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            Self::Int(value) => Some(*value as f64),
            Self::Bool(_) | Self::Text(_) => None,
        }
    }
}

impl From<bool> for WidgetValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for WidgetValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for WidgetValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<String> for WidgetValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for WidgetValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl<'de> Deserialize<'de> for WidgetValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        widget_value_from_json(value).map_err(de::Error::custom)
    }
}

impl Serialize for WidgetValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Float(value) => serializer.serialize_f64(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Text(value) => serializer.serialize_str(value),
        }
    }
}

#[doc(hidden)]
impl JsonSchema for WidgetRole {
    fn schema_name() -> Cow<'static, str> {
        "WidgetRole".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": [
                "button",
                "link",
                "image",
                "label",
                "text_edit",
                "slider",
                "checkbox",
                "combo_box",
                "radio",
                "drag_value",
                "toggle",
                "selectable",
                "separator",
                "spinner",
                "scroll_area",
                "menu_button",
                "collapsing_header",
                "window",
                "progress_bar",
                "color_picker",
                "unknown"
            ]
        })
    }
}

#[doc(hidden)]
impl JsonSchema for WidgetValue {
    fn schema_name() -> Cow<'static, str> {
        "WidgetValue".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "oneOf": [
                { "type": "boolean" },
                { "type": "integer" },
                { "type": "number" },
                { "type": "string" }
            ]
        })
    }
}

fn widget_value_from_json(value: serde_json::Value) -> Result<WidgetValue, String> {
    match value {
        serde_json::Value::Object(map) => {
            if map.len() != 1 {
                return Err("WidgetValue must include exactly one field".to_string());
            }
            let (key, value) = map.into_iter().next().expect("map entry");
            match key.as_str() {
                "bool" => match value {
                    serde_json::Value::Bool(value) => Ok(WidgetValue::Bool(value)),
                    _ => Err("WidgetValue bool must be a boolean".to_string()),
                },
                "float" => match value {
                    serde_json::Value::Number(number) => number
                        .as_f64()
                        .map(WidgetValue::Float)
                        .ok_or_else(|| "WidgetValue float must be a number".to_string()),
                    _ => Err("WidgetValue float must be a number".to_string()),
                },
                "int" => match value {
                    serde_json::Value::Number(number) => number
                        .as_i64()
                        .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
                        .map(WidgetValue::Int)
                        .ok_or_else(|| "WidgetValue int must be an integer".to_string()),
                    _ => Err("WidgetValue int must be an integer".to_string()),
                },
                "text" => match value {
                    serde_json::Value::String(value) => Ok(WidgetValue::Text(value)),
                    _ => Err("WidgetValue text must be a string".to_string()),
                },
                _ => Err("WidgetValue field must be one of bool, float, int, text".to_string()),
            }
        }
        serde_json::Value::Bool(value) => Ok(WidgetValue::Bool(value)),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(WidgetValue::Int(value))
            } else if let Some(value) = number.as_u64() {
                i64::try_from(value)
                    .map(WidgetValue::Int)
                    .map_err(|_| "WidgetValue int is out of range".to_string())
            } else if let Some(value) = number.as_f64() {
                Ok(WidgetValue::Float(value))
            } else {
                Err("WidgetValue number must be int or float".to_string())
            }
        }
        serde_json::Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.starts_with('{')
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
            {
                return widget_value_from_json(parsed);
            }
            Ok(WidgetValue::Text(value))
        }
        _ => Err("WidgetValue must be bool, number, string, or tagged object".to_string()),
    }
}

/// Layout metadata captured for a widget when available.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetLayout {
    /// Desired size of the widget before layout constraints.
    pub desired_size: Vec2,
    /// Actual size assigned to the widget.
    pub actual_size: Vec2,
    /// Clip rect for the widget at layout time.
    pub clip_rect: Rect,
    /// Whether any part of the widget is clipped.
    pub clipped: bool,
    /// Whether the widget extends beyond its allocated layout slot.
    pub overflow: bool,
    /// Available rect before the widget was laid out.
    pub available_rect: Rect,
    /// Visible fraction of the widget within the clip rect.
    pub visible_fraction: f32,
}

/// Scroll metadata captured for a scroll area.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScrollAreaMeta {
    /// Current scroll offset.
    pub offset: Vec2,
    /// Viewport size available to the scroll contents.
    pub viewport_size: Vec2,
    /// Total content size within the scroll area.
    pub content_size: Vec2,
    /// Maximum reachable scroll offset after clamping.
    pub max_offset: Vec2,
}

impl ScrollAreaMeta {
    /// Build scroll metadata and derive the clamped maximum offset.
    pub fn new(offset: Vec2, viewport_size: Vec2, content_size: Vec2) -> Self {
        Self {
            offset,
            viewport_size,
            content_size,
            max_offset: Vec2 {
                x: (content_size.x - viewport_size.x).max(0.0),
                y: (content_size.y - viewport_size.y).max(0.0),
            },
        }
    }
}

/// Min/max bounds for a numeric widget.
///
/// The bounds are `f64` even though `DevUiExt` sliders and drag values take
/// `f32` or `i32`. Scripts see one numeric type, so every recorded range widens
/// to the type that carries all of them without loss.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetRange {
    /// Minimum allowed value.
    pub min: f64,
    /// Maximum allowed value.
    pub max: f64,
}

impl WidgetRange {
    /// Check whether the range contains the provided value.
    pub fn contains(self, value: f64) -> bool {
        self.min <= value && value <= self.max
    }
}

/// Role-specific widget metadata kept on internal registry entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleState {
    /// Scroll area metadata.
    ScrollArea {
        /// Current scroll offset.
        offset: Vec2,
        /// Viewport size available to the scroll contents.
        viewport_size: Vec2,
        /// Total content size within the scroll area.
        content_size: Vec2,
    },
    /// Slider range metadata.
    Slider {
        /// Allowed numeric range.
        range: WidgetRange,
    },
    /// Drag value range metadata.
    DragValue {
        /// Allowed numeric range when constrained by the app.
        range: Option<WidgetRange>,
    },
    /// Combo box option labels.
    ComboBox {
        /// Available option labels.
        options: Vec<String>,
    },
    /// Selected/toggled button state.
    Button {
        /// Whether the button is in a selected state.
        selected: bool,
    },
    /// Checkbox third-state metadata.
    Checkbox {
        /// Whether the checkbox is visually indeterminate.
        indeterminate: bool,
    },
    /// Text edit configuration metadata.
    TextEdit {
        /// Whether the edit is multiline.
        multiline: bool,
        /// Whether the edit masks its input.
        password: bool,
    },
}

/// Role taxonomy entry together with the metadata that the role requires.
///
/// Instrumentation authors this type; the registry records the flat
/// [`WidgetRole`] and [`RoleState`] that it projects. Every role that can carry
/// metadata has its own variant, so a scroll area cannot be recorded without a
/// content size and a slider cannot be recorded without a range. `Plain` is
/// only for roles that carry no metadata at all, such as a label or a
/// separator; a button with no known selection state is
/// `Button { selected: None }`, never `Plain(WidgetRole::Button)`.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetRoleMeta {
    /// A role that carries no role-specific metadata.
    Plain(WidgetRole),
    /// Scroll area with its current geometry.
    ScrollArea {
        /// Current scroll offset.
        offset: Vec2,
        /// Viewport size available to the scroll contents.
        viewport_size: Vec2,
        /// Total content size within the scroll area.
        content_size: Vec2,
    },
    /// Slider with its allowed range.
    Slider {
        /// Allowed numeric range.
        range: WidgetRange,
    },
    /// Drag value, optionally constrained to a range.
    DragValue {
        /// Allowed numeric range when the app constrains one.
        range: Option<WidgetRange>,
    },
    /// Combo box with its option labels.
    ComboBox {
        /// Available option labels.
        options: Vec<String>,
    },
    /// Button, optionally carrying a selected state.
    Button {
        /// Whether the button is in a selected state, when the app tracks one.
        selected: Option<bool>,
    },
    /// Checkbox, optionally carrying a third visual state.
    Checkbox {
        /// Whether the checkbox is visually indeterminate, when the app tracks
        /// it.
        indeterminate: Option<bool>,
    },
    /// Text edit with its input configuration.
    TextEdit {
        /// Whether the edit is multiline.
        multiline: bool,
        /// Whether the edit masks its input.
        password: bool,
    },
}

impl WidgetRoleMeta {
    /// Project the flat taxonomy entry that scripts filter on.
    pub fn role(&self) -> WidgetRole {
        match self {
            Self::Plain(role) => role.clone(),
            Self::ScrollArea { .. } => WidgetRole::ScrollArea,
            Self::Slider { .. } => WidgetRole::Slider,
            Self::DragValue { .. } => WidgetRole::DragValue,
            Self::ComboBox { .. } => WidgetRole::ComboBox,
            Self::Button { .. } => WidgetRole::Button,
            Self::Checkbox { .. } => WidgetRole::Checkbox,
            Self::TextEdit { .. } => WidgetRole::TextEdit,
        }
    }

    /// Project the role-specific metadata recorded on the registry entry.
    pub(crate) fn state(&self) -> Option<RoleState> {
        match self {
            Self::Plain(_) => None,
            Self::ScrollArea {
                offset,
                viewport_size,
                content_size,
            } => Some(RoleState::ScrollArea {
                offset: *offset,
                viewport_size: *viewport_size,
                content_size: *content_size,
            }),
            Self::Slider { range } => Some(RoleState::Slider { range: *range }),
            Self::DragValue { range } => Some(RoleState::DragValue { range: *range }),
            Self::ComboBox { options } => Some(RoleState::ComboBox {
                options: options.clone(),
            }),
            Self::Button { selected } => selected.map(|selected| RoleState::Button { selected }),
            Self::Checkbox { indeterminate } => {
                indeterminate.map(|indeterminate| RoleState::Checkbox { indeterminate })
            }
            Self::TextEdit {
                multiline,
                password,
            } => Some(RoleState::TextEdit {
                multiline: *multiline,
                password: *password,
            }),
        }
    }
}

impl Default for WidgetRoleMeta {
    /// The unknown role, which carries no metadata.
    fn default() -> Self {
        Self::Plain(WidgetRole::Unknown)
    }
}

impl RoleState {
    /// Project scroll-area metadata into the flat scripting shape.
    pub fn scroll_state(&self) -> Option<ScrollAreaMeta> {
        match self {
            Self::ScrollArea {
                offset,
                viewport_size,
                content_size,
            } => Some(ScrollAreaMeta::new(*offset, *viewport_size, *content_size)),
            _ => None,
        }
    }

    /// Project numeric range metadata into the flat scripting shape.
    pub fn range(&self) -> Option<WidgetRange> {
        match self {
            Self::Slider { range } => Some(*range),
            Self::DragValue { range } => *range,
            _ => None,
        }
    }

    /// Return combo-box options when present.
    pub fn options(&self) -> Option<&[String]> {
        match self {
            Self::ComboBox { options } => Some(options),
            _ => None,
        }
    }

    /// Return button selected metadata when present.
    pub fn selected(&self) -> Option<bool> {
        match self {
            Self::Button { selected } => Some(*selected),
            _ => None,
        }
    }

    /// Return checkbox indeterminate metadata when present.
    pub fn indeterminate(&self) -> Option<bool> {
        match self {
            Self::Checkbox { indeterminate } => Some(*indeterminate),
            _ => None,
        }
    }

    /// Return text-edit metadata when present: `(multiline, password)`.
    pub fn text_edit(&self) -> Option<(bool, bool)> {
        match self {
            Self::TextEdit {
                multiline,
                password,
            } => Some((*multiline, *password)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        FixtureParam, FixtureSpec, FixtureTargetSpec, Modifiers, Pos2, RawInputAction,
        RawInputEvent, Rect, RoleState, Vec2, ViewportCondition, ViewportSel, WidgetCondition,
        WidgetRange, WidgetRegistryEntry, WidgetRole, WidgetRoleMeta, WidgetState, WidgetValue,
        validate_viewport_name,
    };

    #[test]
    fn role_meta_projects_role_and_state_for_every_variant() {
        let cases = [
            (
                WidgetRoleMeta::Plain(WidgetRole::Label),
                WidgetRole::Label,
                None,
            ),
            (
                WidgetRoleMeta::ScrollArea {
                    offset: Vec2 { x: 0.0, y: 8.0 },
                    viewport_size: Vec2 { x: 100.0, y: 40.0 },
                    content_size: Vec2 { x: 100.0, y: 400.0 },
                },
                WidgetRole::ScrollArea,
                Some(RoleState::ScrollArea {
                    offset: Vec2 { x: 0.0, y: 8.0 },
                    viewport_size: Vec2 { x: 100.0, y: 40.0 },
                    content_size: Vec2 { x: 100.0, y: 400.0 },
                }),
            ),
            (
                WidgetRoleMeta::Slider {
                    range: WidgetRange {
                        min: 0.0,
                        max: 10.0,
                    },
                },
                WidgetRole::Slider,
                Some(RoleState::Slider {
                    range: WidgetRange {
                        min: 0.0,
                        max: 10.0,
                    },
                }),
            ),
            (
                WidgetRoleMeta::DragValue { range: None },
                WidgetRole::DragValue,
                Some(RoleState::DragValue { range: None }),
            ),
            (
                WidgetRoleMeta::ComboBox {
                    options: vec!["Alpha".to_string()],
                },
                WidgetRole::ComboBox,
                Some(RoleState::ComboBox {
                    options: vec!["Alpha".to_string()],
                }),
            ),
            (
                WidgetRoleMeta::Button { selected: None },
                WidgetRole::Button,
                None,
            ),
            (
                WidgetRoleMeta::Button {
                    selected: Some(true),
                },
                WidgetRole::Button,
                Some(RoleState::Button { selected: true }),
            ),
            (
                WidgetRoleMeta::Checkbox {
                    indeterminate: Some(false),
                },
                WidgetRole::Checkbox,
                Some(RoleState::Checkbox {
                    indeterminate: false,
                }),
            ),
            (
                WidgetRoleMeta::TextEdit {
                    multiline: true,
                    password: false,
                },
                WidgetRole::TextEdit,
                Some(RoleState::TextEdit {
                    multiline: true,
                    password: false,
                }),
            ),
        ];

        for (meta, role, state) in cases {
            assert_eq!(meta.role(), role, "role projection for {meta:?}");
            assert_eq!(meta.state(), state, "state projection for {meta:?}");
        }
    }

    #[test]
    fn data_anchor_validation_follows_rfc_6901() {
        let accepted = ["", "/analysed", "/a~0b", "/a~1b", "/nested/0/name"];
        for pointer in accepted {
            FixtureTargetSpec::data("status.summary", pointer, 3)
                .validate()
                .unwrap_or_else(|error| panic!("pointer {pointer:?} rejected: {error}"));
        }

        let rejected = ["analysed", "/a~", "/a~2b"];
        for pointer in rejected {
            let error = FixtureTargetSpec::data("status.summary", pointer, 3)
                .validate()
                .expect_err("malformed pointer must be rejected");
            assert!(error.contains("RFC 6901"), "{error}");
        }
    }

    #[test]
    fn fixture_validation_reports_a_malformed_data_anchor_pointer() {
        let error = FixtureSpec::new("viewer.mixed", "Mixed games")
            .ready("status.summary")
            .ready_data("status.summary", "analysed", 3)
            .validate(true)
            .expect_err("malformed pointer must fail fixture validation");
        assert!(error.contains("ready 2"), "{error}");
    }

    #[test]
    fn fixture_transition_is_derived_from_preconditions() {
        let baseline = FixtureSpec::new("baseline", "Independent baseline").ready("status");
        let transition = FixtureSpec::new("transition", "State transition")
            .precondition("entry")
            .ready("status");

        assert!(!baseline.is_transition());
        assert!(transition.is_transition());
    }

    #[test]
    fn widget_conditions_serialize_as_the_shared_record_shape() {
        let cases = [
            (
                WidgetCondition::visible(),
                serde_json::json!({ "visible": true }),
            ),
            (
                WidgetCondition::scroll_ready(),
                serde_json::json!({ "scroll_ready": true }),
            ),
            (
                WidgetCondition::label("Ready"),
                serde_json::json!({ "label": "Ready" }),
            ),
            (
                WidgetCondition::data("/analysed", 3),
                serde_json::json!({ "data": { "pointer": "/analysed", "equals": 3 } }),
            ),
        ];
        for (condition, expected) in cases {
            assert_eq!(
                serde_json::to_value(&condition).expect("serialize"),
                expected
            );
            assert_eq!(
                serde_json::from_value::<WidgetCondition>(expected).expect("deserialize"),
                condition
            );
        }
    }

    #[test]
    fn absent_conditions_reject_state_fields() {
        let widget = WidgetCondition {
            present: Some(false),
            visible: Some(false),
            ..WidgetCondition::default()
        };
        assert!(widget.validate().is_err());

        let viewport = ViewportCondition {
            present: Some(false),
            title_contains: Some("demo".to_string()),
            ..ViewportCondition::default()
        };
        assert!(viewport.validate().is_err());
    }

    #[test]
    fn raw_input_uses_the_public_tagged_shape() {
        let event = RawInputEvent::Key {
            key: "escape".to_string(),
            action: RawInputAction::Press,
            modifiers: Some(Modifiers {
                shift: true,
                ..Modifiers::default()
            }),
        };
        assert_eq!(
            serde_json::to_value(event).expect("serialize"),
            serde_json::json!({
                "type": "key",
                "key": "escape",
                "action": "press",
                "modifiers": {
                    "ctrl": false,
                    "shift": true,
                    "alt": false,
                    "command": false,
                },
            })
        );
    }

    #[test]
    fn role_meta_defaults_to_the_unknown_plain_role() {
        let meta = WidgetRoleMeta::default();
        assert_eq!(meta.role(), WidgetRole::Unknown);
        assert_eq!(meta.state(), None);
    }

    fn fixture_param_map<const N: usize>(
        entries: [(&str, WidgetValue); N],
    ) -> BTreeMap<String, WidgetValue> {
        entries
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    fn param_spec() -> FixtureSpec {
        FixtureSpec::new("param.demo", "Parameterized fixture")
            .param(
                FixtureParam::text("mode", "Mode to apply.")
                    .default("fast")
                    .choices(["fast", "slow"]),
            )
            .param(FixtureParam::float("offset", "Offset in points.").range(0.0, 10.0))
            .param(FixtureParam::int("count", "Item count."))
    }

    #[test]
    fn widget_value_deserializes_tagged_object() {
        let value: WidgetValue = serde_json::from_value(serde_json::json!({"bool": false}))
            .expect("deserialize tagged bool");
        assert_eq!(value, WidgetValue::Bool(false));
    }

    #[test]
    fn widget_value_deserializes_stringified_object() {
        let value: WidgetValue = serde_json::from_value(serde_json::json!("{\"bool\": false}"))
            .expect("deserialize stringified bool");
        assert_eq!(value, WidgetValue::Bool(false));
    }

    #[test]
    fn widget_value_deserializes_plain_text() {
        let value: WidgetValue =
            serde_json::from_value(serde_json::json!("hello")).expect("deserialize text");
        assert_eq!(value, WidgetValue::Text("hello".to_string()));
    }

    #[test]
    fn widget_value_serialization() {
        let v = WidgetValue::Bool(true);
        assert_eq!(serde_json::to_string(&v).unwrap(), "true");
    }

    #[test]
    fn fixture_params_validate_defaults_and_int_to_float() {
        let params = param_spec()
            .validate_params(fixture_param_map([
                ("offset", WidgetValue::Int(3)),
                ("count", WidgetValue::Int(2)),
            ]))
            .expect("valid params");

        assert_eq!(params.text("mode"), "fast");
        assert_eq!(params.float("offset"), 3.0);
        assert_eq!(params.int("count"), 2);
        assert_eq!(
            params.as_map().get("offset"),
            Some(&WidgetValue::Float(3.0))
        );
    }

    #[test]
    fn fixture_float_param_choices_normalize_int_literals() {
        let spec = FixtureSpec::new("float.choice", "Float choice fixture").param(
            FixtureParam::float("scale", "Scale factor.")
                .default(1_i64)
                .choices([1_i64, 2_i64]),
        );

        let defaults = spec
            .validate_params(BTreeMap::new())
            .expect("default params");
        assert_eq!(defaults.float("scale"), 1.0);

        let explicit = spec
            .validate_params(fixture_param_map([("scale", WidgetValue::Int(2))]))
            .expect("explicit params");
        assert_eq!(explicit.float("scale"), 2.0);
    }

    #[test]
    fn fixture_params_reject_unknown_and_missing_params() {
        let unknown = param_spec()
            .validate_params(fixture_param_map([
                ("offset", WidgetValue::Float(3.0)),
                ("count", WidgetValue::Int(2)),
                ("extra", WidgetValue::Bool(true)),
            ]))
            .expect_err("unknown param rejected");
        assert_eq!(unknown.code, "unknown_param");

        let missing = param_spec()
            .validate_params(fixture_param_map([(
                "mode",
                WidgetValue::Text("fast".to_string()),
            )]))
            .expect_err("missing param rejected");
        assert_eq!(missing.code, "missing_param");
    }

    #[test]
    fn fixture_params_reject_type_choice_and_range_errors() {
        let wrong_type = param_spec()
            .validate_params(fixture_param_map([
                ("mode", WidgetValue::Text("fast".to_string())),
                ("offset", WidgetValue::Text("three".to_string())),
                ("count", WidgetValue::Int(2)),
            ]))
            .expect_err("type rejected");
        assert_eq!(wrong_type.code, "invalid_param_type");

        let bad_choice = param_spec()
            .validate_params(fixture_param_map([
                ("mode", WidgetValue::Text("medium".to_string())),
                ("offset", WidgetValue::Float(3.0)),
                ("count", WidgetValue::Int(2)),
            ]))
            .expect_err("choice rejected");
        assert_eq!(bad_choice.code, "invalid_param_choice");

        let below_min = param_spec()
            .validate_params(fixture_param_map([
                ("mode", WidgetValue::Text("fast".to_string())),
                ("offset", WidgetValue::Float(-1.0)),
                ("count", WidgetValue::Int(2)),
            ]))
            .expect_err("range min rejected");
        assert_eq!(below_min.code, "param_below_min");

        let above_max = param_spec()
            .validate_params(fixture_param_map([
                ("mode", WidgetValue::Text("fast".to_string())),
                ("offset", WidgetValue::Float(11.0)),
                ("count", WidgetValue::Int(2)),
            ]))
            .expect_err("range max rejected");
        assert_eq!(above_max.code, "param_above_max");
    }

    #[test]
    fn viewport_selector_parses_canonical_strings() {
        assert_eq!(
            ViewportSel::parse("root").unwrap().to_selector_string(),
            "root"
        );
        assert_eq!(
            ViewportSel::parse("details").unwrap().to_selector_string(),
            "details"
        );
        assert_eq!(
            ViewportSel::parse("vp:AB").unwrap().to_selector_string(),
            "vp:ab"
        );
        assert_eq!(
            ViewportSel::parse("vp:00ff").unwrap().to_selector_string(),
            "vp:ff"
        );
    }

    #[test]
    fn viewport_selector_rejects_invalid_raw_ids() {
        for selector in ["vp:", "vp:+ff", "vp:zz"] {
            assert!(
                ViewportSel::parse(selector).is_err(),
                "{selector} should be rejected"
            );
        }
    }

    #[test]
    fn viewport_name_validation_rejects_empty_and_reserved_names() {
        for name in ["", "   "] {
            let error = validate_viewport_name(name).expect_err("empty name rejected");
            assert_eq!(error.code, "empty_viewport_name");
        }
        for name in ["root", "vp:123"] {
            let error = validate_viewport_name(name).expect_err("reserved name rejected");
            assert_eq!(error.code, "reserved_viewport_name");
        }
    }

    #[test]
    fn scroll_area_meta_computes_max_offset() {
        let scroll = RoleState::ScrollArea {
            offset: Vec2 { x: 2.0, y: 3.0 },
            viewport_size: Vec2 { x: 100.0, y: 40.0 },
            content_size: Vec2 { x: 180.0, y: 150.0 },
        }
        .scroll_state()
        .expect("scroll metadata");

        assert_eq!(scroll.max_offset.x, 80.0);
        assert_eq!(scroll.max_offset.y, 110.0);
    }

    #[test]
    fn scroll_area_meta_clamps_negative_max_offset() {
        let scroll = RoleState::ScrollArea {
            offset: Vec2 { x: 0.0, y: 0.0 },
            viewport_size: Vec2 { x: 100.0, y: 40.0 },
            content_size: Vec2 { x: 80.0, y: 20.0 },
        }
        .scroll_state()
        .expect("scroll metadata");

        assert_eq!(scroll.max_offset.x, 0.0);
        assert_eq!(scroll.max_offset.y, 0.0);
    }

    #[test]
    fn widget_state_projects_selected_for_toggle_roles() {
        fn entry(
            role: WidgetRole,
            value: Option<WidgetValue>,
            role_state: Option<RoleState>,
        ) -> WidgetRegistryEntry {
            let rect = Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 1.0, y: 1.0 },
            };
            WidgetRegistryEntry {
                id: "control".to_string(),
                explicit_id: true,
                native_id: 1,
                viewport_id: "root".to_string(),
                layer_id: "background".to_string(),
                layer_order: 0,
                rect,
                interact_rect: rect,
                role,
                label: None,
                value,
                data: None,
                layout: None,
                role_state,
                parent_id: None,
                enabled: true,
                visible: true,
                focused: false,
            }
        }

        let toggle = WidgetState::from(&entry(
            WidgetRole::Toggle,
            Some(WidgetValue::Bool(true)),
            None,
        ));
        assert_eq!(toggle.selected, Some(true));

        let checkbox = WidgetState::from(&entry(
            WidgetRole::Checkbox,
            Some(WidgetValue::Bool(false)),
            Some(RoleState::Checkbox {
                indeterminate: false,
            }),
        ));
        assert_eq!(checkbox.selected, Some(false));

        let indeterminate = WidgetState::from(&entry(
            WidgetRole::Checkbox,
            Some(WidgetValue::Bool(true)),
            Some(RoleState::Checkbox {
                indeterminate: true,
            }),
        ));
        assert_eq!(indeterminate.selected, None);

        let selected_button = WidgetState::from(&entry(
            WidgetRole::Button,
            None,
            Some(RoleState::Button { selected: true }),
        ));
        assert_eq!(selected_button.selected, Some(true));
    }
}

/// Widget registry entry captured per frame.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetRegistryEntry {
    /// Canonical widget id.
    pub id: String,
    /// Whether the id was explicitly provided by instrumentation.
    #[serde(skip_serializing, skip_deserializing)]
    #[schemars(skip)]
    pub explicit_id: bool,
    /// Raw egui widget id used for low-level engine interactions.
    #[serde(skip_serializing, skip_deserializing)]
    #[schemars(skip)]
    pub native_id: u64,
    /// Viewport id string.
    pub viewport_id: String,
    /// Layer id rendered as a stable string (internal use only, e.g. debug
    /// overlay).
    #[serde(skip_serializing, skip_deserializing)]
    #[schemars(skip)]
    pub layer_id: String,
    /// Paint order of the widget's layer, where a larger value paints later
    /// (internal use only).
    #[serde(skip_serializing, skip_deserializing)]
    #[schemars(skip)]
    pub layer_order: u8,
    /// Widget rect.
    pub rect: Rect,
    /// Widget interaction rect.
    pub interact_rect: Rect,
    /// Role taxonomy entry.
    pub role: WidgetRole,
    /// Optional label.
    pub label: Option<String>,
    /// Optional widget value for stateful controls.
    pub value: Option<WidgetValue>,
    /// Structured app-domain metadata attached to this widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Optional layout metadata.
    pub layout: Option<WidgetLayout>,
    /// Optional role-specific metadata encoded as a nested enum.
    #[serde(default)]
    pub role_state: Option<RoleState>,
    /// Optional parent id for container scoping.
    pub parent_id: Option<String>,
    /// Whether the widget is enabled.
    pub enabled: bool,
    /// Whether the widget is visible.
    pub visible: bool,
    /// Whether the widget reported egui focus in the captured frame (may lag
    /// keyboard focus).
    pub focused: bool,
}

/// Live widget snapshot exposed to scripting surfaces.
///
/// `(viewport_id, id)` is the one widget identity across handles, states,
/// dumps, deltas, waits, and error details, so a state that a wait returns
/// names its own widget without a second lookup.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetState {
    /// Canonical widget id.
    pub id: String,
    /// Id of the viewport that holds the widget.
    pub viewport_id: String,
    /// Parent widget id in the same viewport, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Child widget ids in the same viewport, in registry order.
    #[serde(default)]
    pub child_ids: Vec<String>,
    /// Widget rect.
    pub rect: Rect,
    /// Widget interaction rect.
    pub interact_rect: Rect,
    /// Role taxonomy entry.
    pub role: WidgetRole,
    /// Optional label.
    pub label: Option<String>,
    /// Optional widget value for stateful controls.
    pub value: Option<WidgetValue>,
    /// Structured app-domain metadata attached to this widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// String representation of the widget value. Empty string when value is
    /// `None`. For `Bool` → `"true"`/`"false"`, `Float` → decimal, `Int` →
    /// decimal, `Text` → verbatim.
    pub value_text: String,
    /// Optional layout metadata.
    pub layout: Option<WidgetLayout>,
    /// Optional scroll metadata for scroll areas.
    #[serde(rename = "scroll_state")]
    pub scroll: Option<ScrollAreaMeta>,
    /// Optional numeric range for sliders and ranged drag values.
    pub range: Option<WidgetRange>,
    /// Optional option labels for combo boxes.
    pub options: Option<Vec<String>>,
    /// Optional selected/toggled state for selected-aware buttons.
    pub selected: Option<bool>,
    /// Optional third visual state for indeterminate checkboxes.
    pub indeterminate: Option<bool>,
    /// Optional multiline flag for text edits.
    pub multiline: Option<bool>,
    /// Optional password-masking flag for text edits.
    ///
    /// Masking is presentation only. [`Self::value`] still holds the cleartext.
    pub password: Option<bool>,
    /// Whether the widget is enabled.
    pub enabled: bool,
    /// Whether the widget is visible.
    pub visible: bool,
    /// Whether the widget reported egui focus in the captured frame (may lag
    /// keyboard focus).
    pub focused: bool,
}

impl WidgetRegistryEntry {
    /// Selected-on projection used by widget state, dumps, and list filters.
    pub fn selected(&self) -> Option<bool> {
        widget_selected_state(self)
    }
}

fn widget_selected_state(entry: &WidgetRegistryEntry) -> Option<bool> {
    if let Some(selected) = entry.role_state.as_ref().and_then(RoleState::selected) {
        return Some(selected);
    }
    let value = match &entry.value {
        Some(WidgetValue::Bool(value)) => *value,
        _ => return None,
    };
    match entry.role {
        WidgetRole::Toggle | WidgetRole::Radio | WidgetRole::Selectable => Some(value),
        WidgetRole::Checkbox
            if entry.role_state.as_ref().and_then(RoleState::indeterminate) != Some(true) =>
        {
            Some(value)
        }
        _ => None,
    }
}

impl From<&WidgetRegistryEntry> for WidgetState {
    fn from(entry: &WidgetRegistryEntry) -> Self {
        let value_text = entry
            .value
            .as_ref()
            .map(|v| v.to_text())
            .unwrap_or_default();
        let scroll = entry.role_state.as_ref().and_then(RoleState::scroll_state);
        let range = entry.role_state.as_ref().and_then(RoleState::range);
        let options = entry
            .role_state
            .as_ref()
            .and_then(RoleState::options)
            .map(<[String]>::to_vec);
        let selected = entry.selected();
        let indeterminate = entry.role_state.as_ref().and_then(RoleState::indeterminate);
        let (multiline, password) = entry
            .role_state
            .as_ref()
            .and_then(RoleState::text_edit)
            .map_or((None, None), |(multiline, password)| {
                (Some(multiline), Some(password))
            });
        Self {
            id: entry.id.clone(),
            viewport_id: entry.viewport_id.clone(),
            parent_id: entry.parent_id.clone(),
            child_ids: Vec::new(),
            rect: entry.rect,
            interact_rect: entry.interact_rect,
            role: entry.role.clone(),
            label: entry.label.clone(),
            value: entry.value.clone(),
            data: entry.data.clone(),
            value_text,
            layout: entry.layout.clone(),
            scroll,
            range,
            options,
            selected,
            indeterminate,
            multiline,
            password,
            enabled: entry.enabled,
            visible: entry.visible,
            focused: entry.focused,
        }
    }
}
