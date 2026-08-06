//! Headless stand-in for a managed app, used by edev process-lifecycle tests.
//!
//! It answers the launcher handshake and the `script_eval` readiness probe over
//! stdio, then stays alive until the transport closes or the supervisor
//! terminates its process group. It opens no window, so lifecycle tests never
//! put a window on the developer's desktop. Only smoketests run a real app.

use async_trait::async_trait;
use serde_json::json;
use tmcp::{
    Arguments, Error as McpError, Server, ServerCtx, ServerHandler,
    schema::{
        CallToolResponse, CallToolResult, ClientCapabilities, Cursor, Implementation,
        InitializeResult, ListToolsResult, ProtocolVersion, TaskMetadata, Tool, ToolSchema,
    },
};

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

/// Serve the headless test app over stdio until the transport closes.
fn main() -> tmcp::Result<()> {
    Server::new(|| TestApp).serve_stdio_blocking()
}
