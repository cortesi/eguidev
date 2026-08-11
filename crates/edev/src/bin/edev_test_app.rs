//! Headless stand-in for a managed app, used by edev process-lifecycle tests.
//!
//! It answers the launcher handshake and the `script_eval` readiness probe over
//! a direct TCP endpoint, then stays alive until the supervisor
//! terminates its process group. It opens no window, so lifecycle tests never
//! put a window on the developer's desktop. Only smoketests run a real app.

use std::{env, sync::Arc};

use async_trait::async_trait;
use serde_json::json;
use tmcp::{
    Arguments, Error as McpError, Server, ServerCtx, ServerHandler,
    schema::{
        CallToolResponse, CallToolResult, ClientCapabilities, Cursor, Implementation,
        InitializeResult, ListToolsResult, ProtocolVersion, TaskMetadata, Tool, ToolSchema,
    },
};
use tokio::{runtime::Builder as TokioRuntimeBuilder, sync::Notify};

/// MCP server that serves the app surface required by launcher startup.
struct TestApp {
    /// Event that ends the headless process after normal closure.
    shutdown: Arc<Notify>,
    /// Close behavior selected for one lifecycle test.
    close_mode: CloseMode,
}

#[derive(Clone, Copy)]
/// Test-only response behavior for `app_close`.
enum CloseMode {
    /// Accept the request and exit.
    Graceful,
    /// Reject the request.
    Fail,
    /// Accept the request but remain alive.
    Ignore,
}

impl CloseMode {
    /// Parse the test-only close mode from process arguments.
    fn from_env() -> tmcp::Result<Self> {
        let mut args = env::args().skip(1);
        let Some(flag) = args.next() else {
            return Ok(Self::Graceful);
        };
        if flag != "--close-mode" {
            return Err(McpError::InvalidParams(format!("unknown argument: {flag}")));
        }
        let mode = match args.next().as_deref() {
            Some("graceful") => Self::Graceful,
            Some("fail") => Self::Fail,
            Some("ignore") => Self::Ignore,
            Some(value) => {
                return Err(McpError::InvalidParams(format!(
                    "unknown close mode: {value}"
                )));
            }
            None => {
                return Err(McpError::InvalidParams(
                    "--close-mode requires a value".to_string(),
                ));
            }
        };
        if let Some(extra) = args.next() {
            return Err(McpError::InvalidParams(format!(
                "unknown argument: {extra}"
            )));
        }
        Ok(mode)
    }
}

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
        let tools = vec![
            Tool::new("script_eval", ToolSchema::default()),
            Tool::new("app_close", ToolSchema::default()),
        ];
        Ok(ListToolsResult::new().with_tools(tools))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        _arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name == "app_close" {
            return match self.close_mode {
                CloseMode::Graceful => {
                    self.shutdown.notify_one();
                    Ok(CallToolResult::new()
                        .with_structured_content(json!({ "queued": true }))
                        .into())
                }
                CloseMode::Fail => Ok(CallToolResult::new()
                    .with_is_error(true)
                    .with_text_content("test app rejected close")
                    .into()),
                CloseMode::Ignore => Ok(CallToolResult::new()
                    .with_structured_content(json!({ "queued": true }))
                    .into()),
            };
        }
        if name == "script_eval" {
            let result = CallToolResult::new()
                .with_json_text(json!({
                    "success": true,
                    "value": true,
                    "logs": [],
                    "assertions": [],
                    "timing": { "compile_ms": 0, "exec_ms": 0, "total_ms": 0 },
                }))
                .map_err(|error| McpError::InternalError(error.to_string()))?;
            return Ok(result.into());
        }
        Err(McpError::ToolNotFound(name))
    }
}

/// Serve the headless test app at the endpoint selected by Edev.
fn main() -> tmcp::Result<()> {
    let close_mode = CloseMode::from_env()?;
    let addr = env::var(eguidev_runtime::MCP_ADDR_ENV)
        .map_err(|_| McpError::InternalError("missing EGUIDEV_MCP_ADDR".to_string()))?;
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| McpError::InternalError(error.to_string()))?;
    runtime.block_on(async move {
        let shutdown = Arc::new(Notify::new());
        let server_shutdown = Arc::clone(&shutdown);
        let _server = Server::new(move || TestApp {
            shutdown: Arc::clone(&server_shutdown),
            close_mode,
        })
        .serve_tcp(addr)
        .await?;
        shutdown.notified().await;
        Ok(())
    })
}
