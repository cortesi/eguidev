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

    /// Parse the Luau/tool selector grammar: `root`, a semantic name, or `vp:<hex>`.
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
    pub ctrl: bool,
    /// Shift key pressed.
    pub shift: bool,
    /// Alt key pressed.
    pub alt: bool,
    /// Command key pressed.
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
    /// Declarative readiness anchors that must be satisfied before fixture application.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<Anchor>,
    /// Declarative readiness anchors for the fixture baseline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<Anchor>,
    /// Typed scalar params accepted by this fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<FixtureParam>,
    /// Searchable fixture categories used by docs and CLI output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// A single readiness anchor for a fixture baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Anchor {
    /// Widget id to resolve from the registry.
    pub widget_id: String,
    /// Optional viewport selector (`root`, semantic name, or `vp:...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_id: Option<String>,
    /// Readiness condition to evaluate against the widget state.
    pub check: AnchorCheck,
}

/// Declarative readiness checks for fixture anchors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum AnchorCheck {
    /// Widget exists and is visible.
    Visible,
    /// Widget label matches exactly.
    Label(String),
    /// Widget value matches.
    Value(WidgetValue),
    /// Scroll area is initialized and stable across captures.
    ScrollReady,
    /// Scroll area is initialized, stable, and near the requested offset.
    ScrollAt {
        /// Target scroll offset.
        offset: Vec2,
        /// Allowed absolute error per axis.
        tolerance: f32,
    },
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
    /// Optional default. Missing default means the caller must supply the param.
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
    /// Handler-returned dynamic anchors waited on together with the spec anchors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<Anchor>,
}

/// Result returned by a fixture handler.
pub type FixtureResult = Result<FixtureResponse, FixtureError>;

/// Structured fixture handler failure.
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
            anchors: Vec::new(),
            params: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// Add a visible-widget precondition checked before fixture application.
    pub fn precondition(self, widget_id: impl Into<String>) -> Self {
        self.push_precondition(widget_id.into(), None, AnchorCheck::Visible)
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
            AnchorCheck::Visible,
        )
    }

    /// Add an exact-value precondition checked before fixture application.
    pub fn precondition_value(self, widget_id: impl Into<String>, value: WidgetValue) -> Self {
        self.push_precondition(widget_id.into(), None, AnchorCheck::Value(value))
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
            AnchorCheck::Value(value),
        )
    }

    /// Add a visible-widget readiness anchor.
    pub fn anchor(self, widget_id: impl Into<String>) -> Self {
        self.push_anchor(widget_id.into(), None, AnchorCheck::Visible)
    }

    /// Add a visible-widget readiness anchor scoped to a viewport.
    pub fn anchor_in(self, widget_id: impl Into<String>, viewport: impl Into<ViewportSel>) -> Self {
        self.push_anchor(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            AnchorCheck::Visible,
        )
    }

    /// Add an exact-label readiness anchor.
    pub fn anchor_label(self, widget_id: impl Into<String>, text: impl Into<String>) -> Self {
        self.push_anchor(widget_id.into(), None, AnchorCheck::Label(text.into()))
    }

    /// Add an exact-label readiness anchor scoped to a viewport.
    pub fn anchor_label_in(
        self,
        widget_id: impl Into<String>,
        text: impl Into<String>,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_anchor(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            AnchorCheck::Label(text.into()),
        )
    }

    /// Add an exact-value readiness anchor.
    pub fn anchor_value(self, widget_id: impl Into<String>, value: WidgetValue) -> Self {
        self.push_anchor(widget_id.into(), None, AnchorCheck::Value(value))
    }

    /// Add an exact-value readiness anchor scoped to a viewport.
    pub fn anchor_value_in(
        self,
        widget_id: impl Into<String>,
        value: WidgetValue,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_anchor(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            AnchorCheck::Value(value),
        )
    }

    /// Add a scroll-readiness anchor.
    pub fn anchor_scroll(self, widget_id: impl Into<String>) -> Self {
        self.push_anchor(widget_id.into(), None, AnchorCheck::ScrollReady)
    }

    /// Add a scroll-readiness anchor scoped to a viewport.
    pub fn anchor_scroll_in(
        self,
        widget_id: impl Into<String>,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_anchor(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            AnchorCheck::ScrollReady,
        )
    }

    /// Add a scroll-position readiness anchor.
    pub fn anchor_scroll_at(
        self,
        widget_id: impl Into<String>,
        offset: impl Into<Vec2>,
        tolerance: f32,
    ) -> Self {
        self.push_anchor(
            widget_id.into(),
            None,
            AnchorCheck::ScrollAt {
                offset: offset.into(),
                tolerance,
            },
        )
    }

    /// Add a scroll-position readiness anchor scoped to a viewport.
    pub fn anchor_scroll_at_in(
        self,
        widget_id: impl Into<String>,
        offset: impl Into<Vec2>,
        tolerance: f32,
        viewport: impl Into<ViewportSel>,
    ) -> Self {
        self.push_anchor(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            AnchorCheck::ScrollAt {
                offset: offset.into(),
                tolerance,
            },
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

    /// Validate fixture metadata and readiness anchors.
    pub fn validate(&self, require_anchors: bool) -> Result<(), String> {
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
        for (index, anchor) in self.preconditions.iter().enumerate() {
            anchor.validate().map_err(|error| {
                format!("fixture {} precondition {}: {error}", self.name, index + 1)
            })?;
        }
        for (index, anchor) in self.anchors.iter().enumerate() {
            anchor
                .validate()
                .map_err(|error| format!("fixture {} anchor {}: {error}", self.name, index + 1))?;
        }
        if require_anchors && self.anchors.is_empty() {
            return Err(format!(
                "fixture {} must declare at least one readiness anchor",
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
            .map(Anchor::describe)
            .collect::<Vec<_>>();
        let anchors = self
            .anchors
            .iter()
            .map(Anchor::describe)
            .collect::<Vec<_>>();
        match (preconditions.is_empty(), anchors.is_empty()) {
            (true, true) => "No readiness anchors declared.".to_string(),
            (true, false) => anchors.join("; "),
            (false, true) => format!("preconditions: {}", preconditions.join("; ")),
            (false, false) => format!(
                "preconditions: {}; anchors: {}",
                preconditions.join("; "),
                anchors.join("; ")
            ),
        }
    }

    fn push_precondition(
        mut self,
        widget_id: String,
        viewport_id: Option<String>,
        check: AnchorCheck,
    ) -> Self {
        self.preconditions.push(Anchor {
            widget_id,
            viewport_id,
            check,
        });
        self
    }

    fn push_anchor(
        mut self,
        widget_id: String,
        viewport_id: Option<String>,
        check: AnchorCheck,
    ) -> Self {
        self.anchors.push(Anchor {
            widget_id,
            viewport_id,
            check,
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

    /// Add a handler-returned dynamic anchor.
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchors.push(anchor);
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

impl Anchor {
    /// Create a visible-widget anchor.
    pub fn visible(widget_id: impl Into<String>) -> Self {
        Self {
            widget_id: widget_id.into(),
            viewport_id: None,
            check: AnchorCheck::Visible,
        }
    }

    /// Create an exact-label anchor.
    pub fn label(widget_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            widget_id: widget_id.into(),
            viewport_id: None,
            check: AnchorCheck::Label(label.into()),
        }
    }

    /// Create an exact-value anchor.
    pub fn value(widget_id: impl Into<String>, value: impl Into<WidgetValue>) -> Self {
        Self {
            widget_id: widget_id.into(),
            viewport_id: None,
            check: AnchorCheck::Value(value.into()),
        }
    }

    /// Create a scroll-readiness anchor.
    pub fn scroll_ready(widget_id: impl Into<String>) -> Self {
        Self {
            widget_id: widget_id.into(),
            viewport_id: None,
            check: AnchorCheck::ScrollReady,
        }
    }

    /// Create a scroll-position anchor.
    pub fn scroll_at(
        widget_id: impl Into<String>,
        offset: impl Into<Vec2>,
        tolerance: f32,
    ) -> Self {
        Self {
            widget_id: widget_id.into(),
            viewport_id: None,
            check: AnchorCheck::ScrollAt {
                offset: offset.into(),
                tolerance,
            },
        }
    }

    /// Scope this anchor to a viewport selector.
    pub fn in_viewport(mut self, viewport: impl Into<ViewportSel>) -> Self {
        self.viewport_id = Some(viewport.into().to_selector_string());
        self
    }

    /// Return a human-readable description of the anchor.
    pub fn describe(&self) -> String {
        let target = match &self.viewport_id {
            Some(viewport_id) => format!("{} in {}", self.widget_id, viewport_id),
            None => self.widget_id.clone(),
        };
        format!("{target} {}", self.check)
    }

    /// Validate the anchor contents.
    pub fn validate(&self) -> Result<(), String> {
        if self.widget_id.trim().is_empty() {
            return Err("widget_id must not be empty".to_string());
        }
        if let Some(viewport_id) = &self.viewport_id
            && viewport_id.trim().is_empty()
        {
            return Err("viewport_id must not be empty when provided".to_string());
        }
        match &self.check {
            AnchorCheck::Label(text) if text.is_empty() => {
                Err("label anchors must not be empty".to_string())
            }
            AnchorCheck::ScrollAt { tolerance, .. }
                if !tolerance.is_finite() || *tolerance <= 0.0 =>
            {
                Err("scroll_at tolerance must be finite and greater than 0".to_string())
            }
            _ => Ok(()),
        }
    }
}

impl fmt::Display for AnchorCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Visible => f.write_str("visible"),
            Self::Label(text) => write!(f, "label == \"{text}\""),
            Self::Value(value) => match value {
                WidgetValue::Text(text) => write!(f, "value == \"{text}\""),
                _ => write!(f, "value == {}", value.to_text()),
            },
            Self::ScrollReady => f.write_str("scroll_ready"),
            Self::ScrollAt { offset, tolerance } => write!(
                f,
                "scroll_at ({:.1}, {:.1}) ± {:.2}",
                offset.x, offset.y, tolerance
            ),
        }
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
    #[serde(default)]
    pub id: Option<String>,
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
        FixtureParam, FixtureSpec, RoleState, Vec2, ViewportSel, WidgetValue,
        validate_viewport_name,
    };

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
    /// Layer id rendered as a stable string (internal use only, e.g. debug overlay).
    #[serde(skip_serializing, skip_deserializing)]
    #[schemars(skip)]
    pub layer_id: String,
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
    /// Whether the widget reported egui focus in the captured frame (may lag keyboard focus).
    pub focused: bool,
}

/// Live widget snapshot exposed to scripting surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WidgetState {
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
    pub password: Option<bool>,
    /// Whether the widget is enabled.
    pub enabled: bool,
    /// Whether the widget is visible.
    pub visible: bool,
    /// Whether the widget reported egui focus in the captured frame (may lag keyboard focus).
    pub focused: bool,
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
        let selected = entry.role_state.as_ref().and_then(RoleState::selected);
        let indeterminate = entry.role_state.as_ref().and_then(RoleState::indeterminate);
        let (multiline, password) = entry
            .role_state
            .as_ref()
            .and_then(RoleState::text_edit)
            .map_or((None, None), |(multiline, password)| {
                (Some(multiline), Some(password))
            });
        Self {
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
