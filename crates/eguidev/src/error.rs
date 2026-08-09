#![allow(missing_docs)]

use std::{
    error::Error as StdError,
    fmt::{Display, Formatter, Result as FmtResult},
    sync::Arc,
};

use serde::Serialize;
use serde_json::Value;

type SourceError = Arc<dyn StdError + Send + Sync>;

#[derive(Debug, Clone, Serialize)]
pub struct ErrorTarget {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct ToolError {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
    context: Option<Box<ErrorContext>>,
}

#[derive(Debug, Clone, Default)]
struct ErrorContext {
    operation: Option<String>,
    target: Option<ErrorTarget>,
    details: Option<Value>,
    source: Option<SourceError>,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: None,
        }
    }

    fn context_mut(&mut self) -> &mut ErrorContext {
        self.context
            .get_or_insert_with(|| Box::new(ErrorContext::default()))
    }

    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.context_mut().operation = Some(operation.into());
        self
    }

    pub fn with_target(mut self, kind: impl Into<String>, id: impl Into<String>) -> Self {
        self.context_mut().target = Some(ErrorTarget {
            kind: kind.into(),
            id: id.into(),
        });
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.context_mut().details = Some(details);
        self
    }

    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.context_mut().source = Some(Arc::new(source));
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn operation(&self) -> Option<&str> {
        self.context
            .as_ref()
            .and_then(|context| context.operation.as_deref())
    }

    pub fn target(&self) -> Option<&ErrorTarget> {
        self.context
            .as_ref()
            .and_then(|context| context.target.as_ref())
    }

    pub fn details(&self) -> Option<&Value> {
        self.context
            .as_ref()
            .and_then(|context| context.details.as_ref())
    }

    pub fn into_parts(
        self,
    ) -> (
        ErrorCode,
        String,
        Option<String>,
        Option<ErrorTarget>,
        Option<Value>,
        Option<String>,
    ) {
        let context = self.context.map(|context| *context).unwrap_or_default();
        (
            self.code,
            self.message,
            context.operation,
            context.target,
            context.details,
            context.source.map(|source| source.to_string()),
        )
    }
}

impl Display for ToolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.message)
    }
}

impl StdError for ToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.context
            .as_ref()
            .and_then(|context| context.source.as_deref())
            .map(|source| source as &(dyn StdError + 'static))
    }
}

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
