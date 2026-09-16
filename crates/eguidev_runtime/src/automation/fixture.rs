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
        let result = match self.inner.start_fixture(call) {
            FixtureExecution::Ready(result) => result,
            FixtureExecution::Queued(receiver) => {
                self.inner.request_repaint();
                let (_, response, _, _) = wait_until_condition(
                    &self.inner,
                    timeout_ms,
                    DEFAULT_POLL_INTERVAL_MS,
                    Some(egui::ViewportId::ROOT),
                    None,
                    move || {
                        let response = receiver.try_recv();
                        async move { Ok::<_, ToolError>((response.is_some(), response)) }
                    },
                )
                .await?;
                response.unwrap_or_else(|| {
                    Err(eguidev::FixtureError::new(
                        "timeout",
                        format!("fixture handler {name:?} timed out"),
                    ))
                })
            }
        };
        // A frame can start while the handler is still publishing its state.
        // Only frames begun after the handler returns can prove readiness.
        let _fixture_epoch = self.inner.begin_fixture_epoch();
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

#[cfg(test)]
mod tests {
    use std::{
        future::poll_fn,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        task::Poll,
    };

    use eguidev::{FixtureResponse, FixtureSpec};

    use super::*;
    use crate::fixtures::FixtureHandler;

    /// Build a fixture whose invocation is observable without running an app.
    fn queued_fixture_server() -> (DevMcpServer, Arc<AtomicBool>) {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("queued", "Queued fixture").ready("status"),
        ]);
        let called = Arc::new(AtomicBool::new(false));
        let recorded = Arc::clone(&called);
        inner
            .fixtures
            .set_handler(FixtureHandler::Ui(Arc::new(Mutex::new(Box::new(
                move |_ctx, _call| {
                    recorded.store(true, Ordering::SeqCst);
                    Ok(FixtureResponse::new())
                },
            )))))
            .expect("handler");
        let server = DevMcpServer::new(Arc::clone(&inner));
        (server, called)
    }

    #[tokio::test]
    async fn frame_started_during_fixture_handler_is_not_a_fresh_fixture_capture() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("repeated", "Repeated fixture").ready("status"),
        ]);
        let observing = Arc::downgrade(&inner);
        inner
            .fixtures
            .set_handler(FixtureHandler::Runtime(Arc::new(move |_call| {
                // The UI can start another frame before the runtime handler has
                // published the new fixture selection.
                observing
                    .upgrade()
                    .expect("live registry")
                    .begin_frame(egui::ViewportId::ROOT);
                Ok(FixtureResponse::new())
            })))
            .expect("runtime fixture handler");
        let server = DevMcpServer::new(Arc::clone(&inner));
        server
            .fixture_apply_internal("repeated", BTreeMap::new(), 1000)
            .await
            .expect("fixture applied");
        let captured_epoch = inner
            .finish_frame_fixture_epoch(egui::ViewportId::ROOT)
            .expect("frame started in handler");
        assert!(
            captured_epoch < inner.fixture_epoch(),
            "a frame started before the handler completed must not satisfy fixture readiness"
        );
        inner.begin_frame(egui::ViewportId::ROOT);
        assert_eq!(
            inner.finish_frame_fixture_epoch(egui::ViewportId::ROOT),
            Some(inner.fixture_epoch()),
            "the next frame can observe the completed fixture selection"
        );
    }

    #[tokio::test]
    async fn cancelled_ui_fixture_future_does_not_apply_the_fixture_later() {
        let (server, called) = queued_fixture_server();
        let mut apply = Box::pin(server.fixture_apply_internal("queued", BTreeMap::new(), 5_000));
        poll_fn(|cx| {
            assert!(apply.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(apply);

        server.inner.fixtures.drain_ui(&egui::Context::default());

        assert!(
            !called.load(Ordering::SeqCst),
            "cancelled fixture must not modify the app"
        );
    }

    #[tokio::test]
    async fn timed_out_ui_fixture_future_preserves_error_and_cancels_request() {
        let (server, called) = queued_fixture_server();
        let error = server
            .fixture_apply_internal("queued", BTreeMap::new(), 0)
            .await
            .expect_err("UI did not drain the request");
        assert_eq!(error.code(), ErrorCode::Timeout);
        assert_eq!(error.message(), "fixture handler \"queued\" timed out");
        assert_eq!(error.details(), Some(&json!({ "cause_code": "timeout" })));

        server.inner.fixtures.drain_ui(&egui::Context::default());
        assert!(!called.load(Ordering::SeqCst));
    }
}
