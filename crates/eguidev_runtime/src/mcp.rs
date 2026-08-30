//! Thin MCP adapter for the app-owned script boundary.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use ruau_script_api::{ScriptApiQuery, ScriptApiResponse};
use tmcp::{
    ServerCtx, ToolResult, mcp_server,
    schema::{
        CallToolResult, ClientCapabilities, Implementation, InitializeResult, ProtocolVersion,
    },
};

use crate::{
    automation::script::{self, ScriptEvalOptions},
    presentation::parse_client_capabilities,
    registry::Inner,
    runtime::Runtime,
    script_docs::{script_api_response, script_api_tool_result},
};

/// App MCP server. Automation policy remains in the script and automation
/// modules.
pub struct AppMcpServer {
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    presentation_session_id: u64,
}

/// Monotonic identity for one app MCP connection's presentation request.
static NEXT_PRESENTATION_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[mcp_server(initialize_fn = initialize, shutdown_fn = shutdown)]
impl AppMcpServer {
    pub(crate) fn new(inner: Arc<Inner>, runtime: Arc<Runtime>) -> Self {
        Self {
            inner,
            runtime,
            presentation_session_id: NEXT_PRESENTATION_SESSION_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        let presentation =
            parse_client_capabilities(&capabilities).map_err(tmcp::Error::InvalidParams)?;
        self.runtime
            .configure_presentation(&self.inner, self.presentation_session_id, presentation)
            .await
            .map_err(tmcp::Error::InternalError)?;
        Ok(InitializeResult::new("eguidev")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_tools(Some(true)))
    }

    async fn shutdown(&self) -> tmcp::Result<()> {
        self.runtime
            .disconnect_presentation(self.presentation_session_id)
            .await
            .map_err(tmcp::Error::InternalError)
    }

    #[tool(defaults)]
    /// Evaluate strict Luau against the active app.
    async fn script_eval(
        &self,
        script: String,
        timeout_ms: Option<u64>,
        options: Option<ScriptEvalOptions>,
    ) -> ToolResult<CallToolResult> {
        let timeout_ms = timeout_ms.unwrap_or(script::DEFAULT_SCRIPT_TIMEOUT_MS);
        let options = options.unwrap_or_default();
        let source_name = options
            .source_name
            .unwrap_or_else(|| "script.luau".to_string());
        let outcome = script::run_script_eval(
            Arc::clone(&self.inner),
            Arc::clone(&self.runtime),
            script,
            timeout_ms,
            source_name,
            options.args,
        )
        .await;
        Ok(outcome.to_tool_result())
    }

    #[tool(read_only, output_schema = ScriptApiResponse)]
    /// Return shared discovery for the checked Eguidev API.
    async fn script_api(&self, params: ScriptApiQuery) -> ToolResult<CallToolResult> {
        script_api_tool_result(script_api_response(&params))
    }

    #[tool]
    /// Request normal application shutdown.
    async fn app_close(&self) -> ToolResult<CallToolResult> {
        match request_app_shutdown(&self.inner) {
            Ok(path) => Ok(
                CallToolResult::new().with_structured_content(serde_json::json!({
                    "queued": true,
                    "path": path,
                })),
            ),
            Err(message) => Ok(CallToolResult::new()
                .with_is_error(true)
                .with_text_content(message)),
        }
    }
}

/// Request app-owned shutdown or use root-viewport closure as the default.
fn request_app_shutdown(inner: &Inner) -> Result<&'static str, &'static str> {
    let Some(handler) = inner.shutdown_handler() else {
        queue_root_viewport_close(inner);
        return Ok("root_viewport_close");
    };
    catch_unwind(AssertUnwindSafe(|| handler()))
        .map(|()| "application_handler")
        .map_err(|_| "application shutdown handler panicked")
}

/// Queue root-viewport closure through the ordinary UI-thread command path.
fn queue_root_viewport_close(inner: &Inner) {
    inner.queue_command(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use eguidev::DevMcp;

    use super::request_app_shutdown;
    use crate::{registry::Inner, runtime::attach_for_tests};

    #[test]
    fn app_shutdown_defaults_to_root_viewport_close() {
        let inner = Inner::new();
        assert_eq!(request_app_shutdown(&inner), Ok("root_viewport_close"));
        assert!(matches!(
            inner
                .actions
                .drain_commands(egui::ViewportId::ROOT)
                .as_slice(),
            [egui::ViewportCommand::Close]
        ));
    }

    #[test]
    fn app_shutdown_uses_the_registered_application_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let devmcp = attach_for_tests(DevMcp::new().on_shutdown(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        let inner = devmcp.inner_arc().expect("attached runtime");

        assert_eq!(request_app_shutdown(&inner), Ok("application_handler"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            inner
                .actions
                .drain_commands(egui::ViewportId::ROOT)
                .is_empty()
        );
    }

    #[test]
    fn app_shutdown_reports_a_panicking_application_handler() {
        let devmcp = attach_for_tests(DevMcp::new().on_shutdown(|| {
            panic!("shutdown failed");
        }));
        let inner = devmcp.inner_arc().expect("attached runtime");

        assert_eq!(
            request_app_shutdown(&inner),
            Err("application shutdown handler panicked")
        );
        assert!(
            inner
                .actions
                .drain_commands(egui::ViewportId::ROOT)
                .is_empty()
        );
    }
}
