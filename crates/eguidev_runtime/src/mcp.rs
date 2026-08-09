//! Thin MCP adapter for the app-owned script boundary.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

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
    script_definitions,
};

/// App MCP server. Automation policy remains in the script and automation modules.
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
            .configure_presentation(self.presentation_session_id, presentation)
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

    #[tool]
    /// Return the exact checked-in Luau declaration bytes.
    async fn script_api(&self) -> ToolResult<CallToolResult> {
        Ok(CallToolResult::new().with_text_content(script_definitions()))
    }
}
