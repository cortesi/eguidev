//! Fixtures and diagnostics of the script runtime.

use super::*;

impl ScriptRuntime {
    pub(in super::super) async fn fixture_apply(
        &self,
        pos: ScriptPosition,
        name: String,
        params: Option<Value>,
    ) -> ScriptResult<Value> {
        self.record_fixture_viewports(&name);
        let params = fixture_params(params).map_err(|message| self.type_error(pos, message))?;
        let outcome = self
            .await_tool(pos, self.server.fixture_apply(name.clone(), Some(params)))
            .await?;
        self.record_fixture(name, outcome.params.clone());
        self.to_json(pos, outcome)
    }

    pub(in super::super) fn fixtures(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.to_json(pos, self.server.inner.fixtures.fixtures_sorted())
    }

    pub(in super::super) async fn diagnostic(
        &self,
        pos: ScriptPosition,
        name: String,
    ) -> ScriptResult<Value> {
        self.run_diagnostic(pos, &name)
            .await
            .map_err(|error| self.diagnostic_error(pos, error))
    }

    pub(in super::super) async fn diagnostics(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        let mut values = BTreeMap::new();
        let mut errors = BTreeMap::new();
        for name in self.server.inner.diagnostics.names() {
            match self.run_diagnostic(pos, &name).await {
                Ok(value) => {
                    values.insert(name, value);
                }
                Err(error) => {
                    errors.insert(name, error);
                }
            }
        }
        values.insert(
            "eguidev.input".to_string(),
            self.server.inner.actions.pointer_trace(),
        );
        self.to_json(
            pos,
            serde_json::json!({
                "values": values,
                "errors": errors,
            }),
        )
    }

    pub(super) async fn run_diagnostic(
        &self,
        pos: ScriptPosition,
        name: &str,
    ) -> eguidev::DiagnosticResult {
        match self.server.inner.diagnostics.start(name) {
            DiagnosticExecution::Ready(result) => result,
            DiagnosticExecution::Queued(receiver) => {
                self.server.inner.request_repaint();
                let remaining = self.remaining_script_duration(pos).map_err(|error| {
                    eguidev::DiagnosticError::new(
                        error.code.as_deref().unwrap_or("timeout"),
                        error.message,
                    )
                })?;
                let remaining = remaining.unwrap_or(Duration::from_secs(24 * 60 * 60));
                let (_, response, _, _) =
                    super::super::super::utils::wait_until_condition(
                        &self.server.inner,
                        remaining.as_millis() as u64,
                        DEFAULT_POLL_INTERVAL_MS,
                        Some(egui::ViewportId::ROOT),
                        self.deadline,
                        move || {
                            let response = receiver.try_recv();
                            async move {
                                Ok::<_, eguidev::DiagnosticError>((response.is_some(), response))
                            }
                        },
                    )
                    .await?;
                response.unwrap_or_else(|| {
                    Err(eguidev::DiagnosticError::new(
                        "timeout",
                        format!("diagnostic provider {name:?} timed out"),
                    ))
                })
            }
        }
    }

    pub(super) fn diagnostic_error(
        &self,
        pos: ScriptPosition,
        error: eguidev::DiagnosticError,
    ) -> ScriptErrorInfo {
        super::super::super::diagnostic::error_info(error, self.error_location(pos))
    }
}

pub(super) fn fixture_params(
    params: Option<Value>,
) -> Result<BTreeMap<String, WidgetValue>, String> {
    let Some(params) = params else {
        return Ok(BTreeMap::new());
    };
    if params.is_null() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_value(params).map_err(|error| format!("invalid fixture params: {error}"))
}
