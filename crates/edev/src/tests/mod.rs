//! Tests of the launcher.

#[cfg(unix)]
use std::process::Stdio;
use std::{future::poll_fn, task::Poll};

use async_trait::async_trait;
use eguidev_runtime::{
    ScriptArgValue, ScriptArgs, ScriptErrorInfo, ScriptImageInfo,
    smoke::{SuiteConfig, SuiteRunMode},
};
use tempfile::TempDir;
use tmcp::{
    Client, Server, ServerCtx, ServerHandle, ServerHandler,
    schema::{
        CallToolResponse, CallToolResult, ContentBlock, Cursor, ImageContent, InitializeResult,
        TaskMetadata,
    },
    testutils::{TestServerContext, make_duplex_pair},
};
#[cfg(unix)]
use tokio::process::Command;
use tokio::{sync::oneshot, time::timeout};

use super::*;
use crate::{
    command::{dump_script_args, eval_output_value, run_eval_script},
    failure_bundle::{
        BUNDLE_COLLECTION_SCRIPT, BUNDLE_DIAGNOSTICS_SCRIPT, bundle_meta, failure_text,
        replace_dir, stable_hash8,
    },
};

mod command;
mod lifecycle;
mod mcp_server;
mod smoke_run;

fn make_state(tempdir: &TempDir) -> State {
    let config = test_config(tempdir.path().to_path_buf());
    let registry = InstanceRegistry::register(&config).expect("instance registry");
    State::new(config, registry)
}

fn successful_script_eval_result() -> CallToolResult {
    CallToolResult::new()
        .with_json_text(serde_json::json!({
            "success": true,
            "value": true,
            "logs": [],
            "assertions": [],
            "timing": {
                "compile_ms": 0,
                "exec_ms": 0,
                "total_ms": 0
            }
        }))
        .expect("script eval json")
}

fn successful_app_close_result() -> CallToolResult {
    CallToolResult::new().with_structured_content(serde_json::json!({
        "queued": true,
    }))
}

fn successful_outcome(value: &serde_json::Value) -> ScriptEvalOutcome {
    parse_script_eval_outcome(
        &CallToolResult::new()
            .with_json_text(serde_json::json!({
                "success": true,
                "value": value,
                "logs": [],
                "assertions": [],
                "timing": {
                    "compile_ms": 0,
                    "exec_ms": 0,
                    "total_ms": 0
                }
            }))
            .expect("script eval json"),
    )
    .expect("script eval outcome")
}

fn dump_config(tempdir: &TempDir) -> DumpConfig {
    DumpConfig {
        launch: test_config(tempdir.path().to_path_buf()),
        fixture: None,
        params: BTreeMap::new(),
        viewport: None,
        wait_for_initial_capture: true,
        json: false,
        out: None,
        timeout: None,
    }
}

struct MockServer;

#[async_trait]
impl ServerHandler for MockServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        Ok(InitializeResult::new("mock"))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        _arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name == "app_close" {
            Ok(successful_app_close_result().into())
        } else if name == "script_eval" {
            Ok(successful_script_eval_result().into())
        } else {
            Err(McpError::ToolNotFound(name))
        }
    }
}

struct RecordingEvalServer {
    requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
}

struct BundleSmokeServer {
    requests: Arc<Mutex<Vec<ScriptEvalRequest>>>,
}

#[async_trait]
impl ServerHandler for RecordingEvalServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        Ok(InitializeResult::new("recording"))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name == "app_close" {
            return Ok(successful_app_close_result().into());
        }
        if name != "script_eval" {
            return Err(McpError::ToolNotFound(name));
        }
        let request = arguments
            .ok_or_else(|| McpError::InternalError("missing arguments".to_string()))?
            .deserialize::<ScriptEvalRequest>()
            .map_err(|error| McpError::InternalError(error.to_string()))?;
        self.requests
            .lock()
            .expect("requests lock poisoned")
            .push(request.clone());
        Ok(CallToolResult::new()
            .with_json_text(serde_json::json!({
                "success": true,
                "value": {
                    "script": request.script,
                    "timeout_ms": request.timeout_ms,
                },
                "logs": [],
                "assertions": [],
                "timing": {
                    "compile_ms": 0,
                    "exec_ms": 0,
                    "total_ms": 0
                }
            }))
            .expect("script eval json")
            .into())
    }
}

#[async_trait]
impl ServerHandler for BundleSmokeServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        Ok(InitializeResult::new("bundle-smoke"))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name == "app_close" {
            return Ok(successful_app_close_result().into());
        }
        if name != "script_eval" {
            return Err(McpError::ToolNotFound(name));
        }
        let request = arguments
            .ok_or_else(|| McpError::InternalError("missing arguments".to_string()))?
            .deserialize::<ScriptEvalRequest>()
            .map_err(|error| McpError::InternalError(error.to_string()))?;
        self.requests
            .lock()
            .expect("requests lock poisoned")
            .push(request.clone());
        if request.script == BUNDLE_COLLECTION_SCRIPT {
            let result = CallToolResult::new()
                .with_json_text(serde_json::json!({
                    "success": true,
                    "value": {
                        "tree": {
                            "viewports": []
                        },
                        "text": "viewport root\n",
                        "shots": [{
                            "viewport_id": "root",
                            "name": "root",
                            "image": {
                                "type": "image_ref",
                                "id": "image-0"
                            }
                        }],
                        "errors": []
                    },
                    "images": [{
                        "id": "image-0",
                        "content_index": 1,
                        "kind": "viewport",
                        "viewport_id": "root"
                    }],
                    "logs": [],
                    "assertions": [],
                    "timing": {
                        "compile_ms": 0,
                        "exec_ms": 0,
                        "total_ms": 0
                    }
                }))
                .expect("collection json")
                .with_content(ContentBlock::Image(
                    ImageContent::new("", "image/jpeg").with_data_bytes(b"jpeg"),
                ));
            return Ok(result.into());
        }
        if request.script == BUNDLE_DIAGNOSTICS_SCRIPT {
            return Ok(CallToolResult::new()
                .with_json_text(serde_json::json!({
                    "success": true,
                    "value": {
                        "values": {
                            "demo.runtime": {
                                "ready": true
                            }
                        },
                        "errors": {}
                    },
                    "logs": [],
                    "assertions": [],
                    "timing": {
                        "compile_ms": 0,
                        "exec_ms": 0,
                        "total_ms": 0
                    }
                }))
                .expect("diagnostics json")
                .into());
        }

        Ok(CallToolResult::new()
            .with_json_text(serde_json::json!({
                "success": false,
                "logs": ["before failure"],
                "assertions": [{
                    "passed": false,
                    "message": "boom",
                    "location": "10_fail.luau:1"
                }],
                "fixtures": [{
                    "name": "basic.default",
                    "params": {
                        "offset": 180
                    }
                }],
                "timing": {
                    "compile_ms": 0,
                    "exec_ms": 1,
                    "total_ms": 1
                },
                "error": {
                    "type": "assertion",
                    "message": "boom",
                    "code": "assertion_failed",
                    "details": {
                        "widget": "basic.status"
                    }
                }
            }))
            .expect("failure json")
            .into())
    }
}

struct FailingServer;

#[async_trait]
impl ServerHandler for FailingServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        Ok(InitializeResult::new("mock"))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        _arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name == "app_close" {
            Ok(successful_app_close_result().into())
        } else if name == "script_eval" {
            Err(McpError::InternalError("boom".to_string()))
        } else {
            Err(McpError::ToolNotFound(name))
        }
    }
}

/// Serve `factory` over an in-memory stream, and wrap a connected client as a
/// managed app.
async fn make_app<C, G>(factory: G) -> (AppProcess, ServerHandle)
where
    C: ServerHandler + 'static,
    G: Fn() -> C + Send + Sync + 'static,
{
    let ((server_reader, server_writer), (client_reader, client_writer)) = {
        let (sr, sw, cr, cw) = make_duplex_pair();
        ((sr, sw), (cr, cw))
    };

    let server = Server::new(factory);
    let handle = ServerHandle::from_stream(server, server_reader, server_writer)
        .await
        .expect("server handle");

    let mut client = Client::new("test", "0.1.0");
    client
        .connect_stream_raw(client_reader, client_writer)
        .await
        .expect("connect");
    client.init().await.expect("init");

    let app = AppProcess {
        child: None,
        process_group_id: None,
        supervisor_pid: None,
        supervisor_exit_task: None,
        app_launch: None,
        app_record: None,
        ownership_writer: None,
        client: Arc::new(AsyncMutex::new(client)),
        mcp_endpoint: "/nonexistent/edev-test.sock".to_string(),
        mcp_socket: None,
        stdout_task: None,
        stderr_task: None,
        stderr_buffer: Arc::new(Mutex::new(Vec::new())),
        stdout_buffer: Arc::new(Mutex::new(Vec::new())),
        log_state: LogState::new(false),
        shutdown_grace: Duration::from_secs(30),
    };

    (app, handle)
}

/// Answer each readiness probe with a script timeout.
struct ProbeTimeoutServer;

#[async_trait]
impl ServerHandler for ProbeTimeoutServer {
    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: ProtocolVersion,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        Ok(InitializeResult::new("probe-timeout"))
    }

    async fn call_tool(
        &self,
        _context: &ServerCtx,
        name: String,
        _arguments: Option<Arguments>,
        _task: Option<TaskMetadata>,
    ) -> tmcp::Result<CallToolResponse> {
        if name == "app_close" {
            return Ok(successful_app_close_result().into());
        }
        Ok(CallToolResult::new()
            .with_json_text(serde_json::json!({
                "success": false,
                "logs": [],
                "assertions": [],
                "timing": { "compile_ms": 0, "exec_ms": 0, "total_ms": 0 },
                "error": {
                    "type": "timeout",
                    "message": "Script timed out after 120000ms",
                    "code": "timeout",
                },
            }))
            .expect("timeout outcome")
            .into())
    }
}
