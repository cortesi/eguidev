//! Shared wire types for connection-scoped automation presentation.

use serde::{Deserialize, Serialize};

/// Private experimental MCP capability used to negotiate app presentation.
pub const EXPERIMENTAL_PRESENTATION_CAPABILITY: &str = "eguidev.presentation";

/// Presentation requested for one automation connection or launcher.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Presentation {
    /// Keep the app out of the foreground while automation is connected.
    #[default]
    Background,
    /// Preserve the app's ordinary foreground presentation.
    Foreground,
}

impl Presentation {
    /// Return the wire spelling used in the initialize capability and status payload.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Foreground => "foreground",
        }
    }
}

/// Serializable macOS presentation diagnostics reported by the runtime.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct PresentationStatus {
    /// Presentation requested when the automation connection was initialized.
    pub requested_presentation: Presentation,
    /// Activation policy observed in the app process, when available.
    pub observed_activation_policy: Option<String>,
    /// Absolute executable path reported by the app process, when available.
    pub executable: Option<String>,
    /// Enclosing application bundle, when the executable is bundled.
    pub bundle_root: Option<String>,
    /// Application bundle identifier, when available.
    pub bundle_identifier: Option<String>,
}

impl PresentationStatus {
    /// Build the initial status before the runtime health payload is available.
    pub const fn requested(requested_presentation: Presentation) -> Self {
        Self {
            requested_presentation,
            observed_activation_policy: None,
            executable: None,
            bundle_root: None,
            bundle_identifier: None,
        }
    }
}
