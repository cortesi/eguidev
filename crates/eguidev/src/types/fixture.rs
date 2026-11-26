//! Fixture specs, params, calls, and results.

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
};

use serde::{Deserialize, Serialize};

use super::*;
use crate::diagnostics::panic_message;

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
    #[must_use]
    pub fn precondition(self, widget_id: impl Into<String>) -> Self {
        self.push_precondition(widget_id.into(), None, WidgetCondition::visible())
    }

    /// Add a visible-widget precondition scoped to a viewport.
    #[must_use]
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
    #[must_use]
    pub fn precondition_value(self, widget_id: impl Into<String>, value: WidgetValue) -> Self {
        self.push_precondition(widget_id.into(), None, WidgetCondition::value(value))
    }

    /// Add an exact-value precondition scoped to a viewport.
    #[must_use]
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
    #[must_use]
    pub fn ready(self, widget_id: impl Into<String>) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::visible())
    }

    /// Add a visible-widget ready condition scoped to a viewport.
    #[must_use]
    pub fn ready_in(self, widget_id: impl Into<String>, viewport: impl Into<ViewportSel>) -> Self {
        self.push_ready(
            widget_id.into(),
            Some(viewport.into().to_selector_string()),
            WidgetCondition::visible(),
        )
    }

    /// Add an exact-label ready condition.
    #[must_use]
    pub fn ready_label(self, widget_id: impl Into<String>, text: impl Into<String>) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::label(text))
    }

    /// Add an exact-label ready condition scoped to a viewport.
    #[must_use]
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
    #[must_use]
    pub fn ready_value(self, widget_id: impl Into<String>, value: WidgetValue) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::value(value))
    }

    /// Add an exact-value ready condition scoped to a viewport.
    #[must_use]
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
    #[must_use]
    pub fn ready_scroll(self, widget_id: impl Into<String>) -> Self {
        self.push_ready(widget_id.into(), None, WidgetCondition::scroll_ready())
    }

    /// Add a scroll-readiness condition scoped to a viewport.
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn param(mut self, param: FixtureParam) -> Self {
        self.params.push(param);
        self
    }

    /// Add a fixture tag used by CLI/docs output.
    #[must_use]
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
    #[must_use]
    pub fn default(mut self, value: impl Into<WidgetValue>) -> Self {
        self.default = Some(self.normalize_literal(value.into()));
        self
    }

    /// Restrict this parameter to exact choices.
    #[must_use]
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
    #[must_use]
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
        if value.kind() != self.kind {
            return Err(FixtureError::new(
                "invalid_param_type",
                format!(
                    "param {:?} expected {}, got {}",
                    self.name,
                    self.kind.as_str(),
                    value.kind().as_str()
                ),
            )
            .details(serde_json::json!({
                "param": self.name,
                "expected": self.kind.as_str(),
                "actual": value.kind().as_str(),
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
    /// The name of this kind, as a fixture catalog spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Text => "text",
        }
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
    #[must_use]
    pub fn value(mut self, name: impl Into<String>, value: impl Into<WidgetValue>) -> Self {
        self.values.insert(name.into(), value.into());
        self
    }

    /// Add a handler-returned dynamic ready.
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
