//! Viewport selectors and viewport names.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

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
