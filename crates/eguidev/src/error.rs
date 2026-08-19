#![allow(missing_docs)]

use std::{
    error::Error as StdError,
    fmt::{Display, Formatter, Result as FmtResult},
};

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolError {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
    details: Option<Value>,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidArgument,
    NotFound,
    NotActionable,
    InstrumentationFault,
    Timeout,
    Unsupported,
    FixtureFailed,
    DiagnosticFailed,
    ExpectationFailed,
    Internal,
}

impl ErrorCode {
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
