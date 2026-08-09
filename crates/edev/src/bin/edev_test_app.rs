//! Headless stand-in for a managed app, used by edev process-lifecycle tests.
//!
//! It answers the launcher handshake and the `script_eval` readiness probe over
//! a direct TCP endpoint, then stays alive until the supervisor
//! terminates its process group. It opens no window, so lifecycle tests never
//! put a window on the developer's desktop. Only smoketests run a real app.

use std::{env, future::pending};

use async_trait::async_trait;
use serde_json::json;
use tmcp::{
    Arguments, Error as McpError, Server, ServerCtx, ServerHandler,
    schema::{
        CallToolResponse, CallToolResult, ClientCapabilities, Cursor, Implementation,
        InitializeResult, ListToolsResult, ProtocolVersion, TaskMetadata, Tool, ToolSchema,
    },
};
use tokio::runtime::Builder as TokioRuntimeBuilder;

/// MCP server that serves the app surface required by launcher startup.
struct TestApp;

#[async_trait]
impl ServerHandler for TestApp {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        Ok(InitializeResult::new("edev-test-app")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_tools(Some(false)))
    }

    async fn list_tools(
        &self,
        _context: &ServerCtx,
        _cursor: Option<Cursor>,
    ) -> tmcp::Result<ListToolsResult> {
        let tools = vec![Tool::new("script_eval", ToolSchema::default())];
        Ok(ListToolsResult::new().with_tools(tools))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        _arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name != "script_eval" {
            return Err(McpError::ToolNotFound(name));
        }
        let result = CallToolResult::new()
            .with_json_text(json!({
                "success": true,
                "value": true,
                "logs": [],
                "assertions": [],
                "timing": { "compile_ms": 0, "exec_ms": 0, "total_ms": 0 },
            }))
            .map_err(|error| McpError::InternalError(error.to_string()))?;
        Ok(result.into())
    }
}

/// Serve the headless test app at the endpoint selected by Edev.
fn main() -> tmcp::Result<()> {
    let addr = env::var(eguidev_runtime::MCP_ADDR_ENV)
        .map_err(|_| McpError::InternalError("missing EGUIDEV_MCP_ADDR".to_string()))?;
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| McpError::InternalError(error.to_string()))?;
    runtime.block_on(async move {
        let _server = Server::new(|| TestApp).serve_tcp(addr).await?;
        pending::<()>().await;
        #[allow(unreachable_code)]
        Ok(())
    })
}
