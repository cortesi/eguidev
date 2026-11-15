//! Widget references, roles, values, and recorded state.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{
    Deserialize, Serialize,
    de::{self, Deserializer},
    ser::Serializer,
};

use super::*;

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
    /// Optional viewport selector (`root`, semantic name, or `vp:...`).
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

impl From<egui::Role> for WidgetRole {
    /// Classify an egui accessibility role in the eguidev taxonomy.
    ///
    /// The taxonomy is coarser than AccessKit, so several roles share one
    /// entry: each text input role is a text edit, and each progress indicator
    /// is a progress bar, since egui gives a spinner the same role. A role
    /// that the taxonomy has no entry for is unknown.
    fn from(role: egui::Role) -> Self {
        use egui::Role;

        match role {
            Role::Button | Role::DefaultButton => Self::Button,
            Role::Link => Self::Link,
            Role::Image => Self::Image,
            Role::Label => Self::Label,
            Role::TextInput
            | Role::MultilineTextInput
            | Role::PasswordInput
            | Role::SearchInput
            | Role::EmailInput
            | Role::UrlInput
            | Role::PhoneNumberInput => Self::TextEdit,
            Role::Slider => Self::Slider,
            Role::CheckBox => Self::Checkbox,
            Role::ComboBox | Role::EditableComboBox => Self::ComboBox,
            Role::RadioButton => Self::Radio,
            Role::SpinButton => Self::DragValue,
            Role::Switch => Self::Toggle,
            Role::Splitter => Self::Separator,
            Role::ScrollView => Self::ScrollArea,
            Role::DisclosureTriangle => Self::CollapsingHeader,
            Role::Window | Role::Dialog | Role::AlertDialog => Self::Window,
            Role::ProgressIndicator => Self::ProgressBar,
            Role::ColorWell => Self::ColorPicker,
            _ => Self::Unknown,
        }
    }
}

impl WidgetRole {
    /// The egui accessibility role that best describes this entry.
    ///
    /// AccessKit has no role for some entries, so they share the role that
    /// egui itself gives such a widget: a selectable label, a toggle value,
    /// and a menu button are buttons, and a spinner is a progress indicator.
    pub fn accesskit_role(&self) -> egui::Role {
        use egui::Role;

        match self {
            Self::Button | Self::Selectable | Self::MenuButton => Role::Button,
            Self::Link => Role::Link,
            Self::Image => Role::Image,
            Self::Label => Role::Label,
            Self::TextEdit => Role::TextInput,
            Self::Slider => Role::Slider,
            Self::Checkbox => Role::CheckBox,
            Self::ComboBox => Role::ComboBox,
            Self::Radio => Role::RadioButton,
            Self::DragValue => Role::SpinButton,
            Self::Toggle => Role::Switch,
            Self::Separator => Role::Splitter,
            Self::Spinner | Self::ProgressBar => Role::ProgressIndicator,
            Self::ScrollArea => Role::ScrollView,
            Self::CollapsingHeader => Role::DisclosureTriangle,
            Self::Window => Role::Window,
            Self::ColorPicker => Role::ColorWell,
            Self::Unknown => Role::Unknown,
        }
    }
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

    pub(super) fn kind_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Float(_) => "float",
            Self::Int(_) => "int",
            Self::Text(_) => "text",
        }
    }

    pub(super) fn as_f64(&self) -> Option<f64> {
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
    /// Exact text layout painted by the widget, when the caller supplies its
    /// galley.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<WidgetTextLayout>,
}

/// Captured text layout from the galley that a widget painted.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetTextLayout {
    /// Fonts used by the galley's layout sections.
    pub fonts: Vec<WidgetFont>,
    /// Painted lines in visual order.
    pub lines: Vec<WidgetTextLine>,
    /// Height of the first painted line.
    pub line_height: f32,
    /// Whether egui removed text because of its row limit.
    pub elided: bool,
}

/// One font used by a captured galley.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetFont {
    /// Egui font family name.
    pub family: String,
    /// Font size in logical points.
    pub size: f32,
}

/// One painted line from a captured galley.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetTextLine {
    /// Text on this line.
    pub text: String,
    /// Painted line width in logical points.
    pub width: f32,
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

    /// The egui accessibility role that best describes this entry. A text
    /// edit names its multiline or password form.
    pub fn accesskit_role(&self) -> egui::Role {
        match self {
            Self::TextEdit { password: true, .. } => egui::Role::PasswordInput,
            Self::TextEdit {
                multiline: true, ..
            } => egui::Role::MultilineTextInput,
            _ => self.role().accesskit_role(),
        }
    }

    /// Build the entry of an egui accessibility role, when the role carries
    /// all the metadata that its entry requires.
    ///
    /// `range` is the numeric range of the widget, when egui reports one. A
    /// slider needs it. A scroll area and a combo box need metadata that egui
    /// does not report, so they and unknown roles give `None`.
    pub fn from_accesskit(role: egui::Role, range: Option<WidgetRange>) -> Option<Self> {
        Some(match WidgetRole::from(role) {
            WidgetRole::Unknown | WidgetRole::ScrollArea | WidgetRole::ComboBox => return None,
            WidgetRole::Slider => Self::Slider { range: range? },
            WidgetRole::DragValue => Self::DragValue { range },
            WidgetRole::Button => Self::Button { selected: None },
            WidgetRole::Checkbox => Self::Checkbox {
                indeterminate: None,
            },
            WidgetRole::TextEdit => Self::TextEdit {
                multiline: role == egui::Role::MultilineTextInput,
                password: role == egui::Role::PasswordInput,
            },
            plain => Self::Plain(plain),
        })
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

/// The layer of an entry that serde builds, which holds no layer.
fn background_layer() -> egui::LayerId {
    egui::LayerId::background()
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
    /// Layer of the widget (internal use only, e.g. the debug overlay and
    /// dumps, which format it).
    #[serde(skip, default = "background_layer")]
    #[schemars(skip)]
    pub layer_id: egui::LayerId,
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
    /// Whether another egui layer covers the widget's action point (the
    /// center of `interact_rect`), so a pointer action here would route to
    /// that layer instead. `false` when no layer, or only the widget's own
    /// layer, sits at that point.
    pub covered: bool,
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
    /// Whether another egui layer covers the widget's action point. See
    /// [`WidgetRegistryEntry::covered`].
    pub covered: bool,
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
            covered: entry.covered,
        }
    }
}
