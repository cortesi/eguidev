pub use base_error::ErrorCode;
use eguidev::internal::error as base_error;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolError(base_error::ToolError);

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(base_error::ToolError::new(code, message))
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.0 = self.0.with_details(details);
        self
    }

    pub(crate) fn code(&self) -> ErrorCode {
        self.0.code()
    }

    #[cfg(test)]
    pub(crate) fn message(&self) -> &str {
        self.0.message()
    }

    pub(crate) fn details(&self) -> Option<&Value> {
        self.0.details()
    }

    pub(crate) fn into_tmcp(self) -> tmcp::ToolError {
        let code = self.0.code().as_str();
        let message = self.0.message().to_string();
        let mut structured = serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        });
        if let Some(details) = self.0.details()
            && let Some(error) = structured.get_mut("error")
            && let Some(map) = error.as_object_mut()
        {
            map.insert("details".to_string(), details.clone());
        }
        tmcp::ToolError::new(code, message).with_structured(structured)
    }
}

impl From<base_error::ToolError> for ToolError {
    fn from(error: base_error::ToolError) -> Self {
        Self(error)
    }
}

impl From<ToolError> for tmcp::ToolError {
    fn from(error: ToolError) -> Self {
        error.into_tmcp()
    }
}
