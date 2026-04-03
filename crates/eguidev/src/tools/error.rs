use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolError {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
    pub(crate) details: Option<Value>,
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

    pub(crate) fn into_tmcp(self) -> tmcp::ToolError {
        let code = self.code.as_str();
        let message = self.message;
        let mut structured = serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        });
        if let Some(details) = self.details
            && let Some(error) = structured.get_mut("error")
            && let Some(map) = error.as_object_mut()
        {
            map.insert("details".to_string(), details);
        }
        tmcp::ToolError::new(code, message).with_structured(structured)
    }
}

impl From<ToolError> for tmcp::ToolError {
    fn from(error: ToolError) -> Self {
        error.into_tmcp()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotFound,
    Ambiguous,
    InvalidRef,
    TargetNotFocusable,
    FocusNotAcquired,
    TargetDetached,
    DuplicateWidgetId,
    Timeout,
    Internal,
}

impl ErrorCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::InvalidRef => "invalid_ref",
            Self::TargetNotFocusable => "target_not_focusable",
            Self::FocusNotAcquired => "focus_not_acquired",
            Self::TargetDetached => "target_detached",
            Self::DuplicateWidgetId => "duplicate_widget_id",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }
}
