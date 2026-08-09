//! Fixture-domain error adaptation.

use super::*;

pub(super) fn fixture_error_to_tool(error: eguidev::FixtureError) -> ToolError {
    let code = match error.code.as_str() {
        "timeout" => ErrorCode::Timeout,
        "unknown_param"
        | "missing_param"
        | "invalid_param_type"
        | "invalid_param_choice"
        | "param_below_min"
        | "param_above_max" => ErrorCode::InvalidArgument,
        _ => ErrorCode::FixtureFailed,
    };
    let details = match error.details {
        Some(details) => json!({
            "cause_code": error.code,
            "details": details,
        }),
        None => json!({ "cause_code": error.code }),
    };
    ToolError::new(code, error.message).with_details(details)
}

impl DevMcpServer {
    pub(super) async fn fixture_apply_internal(
        &self,
        name: &str,
        params: BTreeMap<String, WidgetValue>,
        timeout_ms: u64,
    ) -> Result<FixtureApplyOutcome, ToolError> {
        let Some(spec) = self.inner.fixtures.fixture(name) else {
            return Err(ToolError::new(
                ErrorCode::NotFound,
                format!("Unknown fixture: {name}"),
            ));
        };
        let params = spec
            .validate_params(params)
            .map_err(fixture_error_to_tool)?;
        let validated_params = params.as_map().clone();
        let call = FixtureCall {
            name: name.to_string(),
            params,
        };

        self.inner.clear_all();
        self.inner.dismiss_transient_ui(None);
        let _fixture_epoch = self.inner.begin_fixture_epoch();
        let result = match self.inner.start_fixture(call) {
            FixtureExecution::Ready(result) => result,
            FixtureExecution::Queued(receiver) => {
                self.inner.request_repaint();
                spawn_blocking(move || receiver.recv_timeout(Duration::from_millis(timeout_ms)))
                    .await
                    .map_err(|error| {
                        ToolError::new(
                            ErrorCode::Internal,
                            format!("fixture wait task failed: {error}"),
                        )
                    })?
            }
        };
        self.inner.dismiss_transient_ui(None);
        let response = result.map_err(fixture_error_to_tool)?;
        Ok(FixtureApplyOutcome {
            params: validated_params,
            values: response.values,
            ready: response.ready,
        })
    }

    /// Apply an app-defined fixture without waiting for readiness.
    pub(super) async fn fixture_apply(
        &self,
        name: String,
        params: Option<BTreeMap<String, WidgetValue>>,
    ) -> ToolResult<FixtureApplyOutcome> {
        Ok(self
            .fixture_apply_internal(&name, params.unwrap_or_default(), DEFAULT_WAIT_TIMEOUT_MS)
            .await?)
    }
}
