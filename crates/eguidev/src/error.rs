//! The domain error of the automation surface and its stable codes.

use std::{
    error::Error as StdError,
    fmt::{Display, Formatter, Result as FmtResult},
};

use serde_json::Value;

/// A failure of an automation operation: a stable code, a message, and
/// optional machine-readable details.
#[derive(Debug, Clone)]
pub struct ToolError {
    /// The stable code of the failure.
    pub(crate) code: ErrorCode,
    /// The human-readable message.
    pub(crate) message: String,
    /// Machine-readable evidence of the failure.
    details: Option<Value>,
}

impl ToolError {
    /// Build an error with a code and a message, and no details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Attach machine-readable details, in place of any earlier ones.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Return the stable code of the failure.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// Return the human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Return the machine-readable details, when the failure has them.
    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }
}

impl Display for ToolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.message)
    }
}

impl StdError for ToolError {}

/// The stable code of a failure, as the declared `ErrorCode` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// An argument or an option is not valid.
    InvalidArgument,
    /// The target widget, viewport, or fixture does not exist.
    NotFound,
    /// The target cannot take the action now, for example because it is
    /// hidden, covered, or disabled.
    NotActionable,
    /// The instrumentation of the app is faulty, for example with a
    /// duplicate explicit ID.
    InstrumentationFault,
    /// A wait or an operation did not finish within its timeout.
    Timeout,
    /// The platform or the configuration does not support the operation.
    Unsupported,
    /// A fixture handler or a fixture readiness wait failed.
    FixtureFailed,
    /// A diagnostic provider failed.
    DiagnosticFailed,
    /// An expectation of a script did not match.
    ExpectationFailed,
    /// An internal failure of the runtime.
    Internal,
}

impl ErrorCode {
    /// Return the stable wire string of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::NotActionable => "not_actionable",
            Self::InstrumentationFault => "instrumentation_fault",
            Self::Timeout => "timeout",
            Self::Unsupported => "unsupported",
            Self::FixtureFailed => "fixture_failed",
            Self::DiagnosticFailed => "diagnostic_failed",
            Self::ExpectationFailed => "expectation_failed",
            Self::Internal => "internal",
        }
    }
}
