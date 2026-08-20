//! MCP server implementation for DevMCP.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{future::pending, sync::Arc, thread};

use tmcp::Server;
use tokio::runtime::Builder;

use crate::{mcp::AppMcpServer, registry::Inner, runtime::Runtime};

/// Optional loopback address used for a directly connectable app MCP server.
pub const MCP_ADDR_ENV: &str = "EGUIDEV_MCP_ADDR";

#[cfg(test)]
static START_SERVER_CALLS: AtomicUsize = AtomicUsize::new(0);

#[allow(clippy::needless_pass_by_value)]
pub fn start_server(inner: Arc<Inner>, runtime_state: Arc<Runtime>, addr: String) {
    if cfg!(test) {
        #[cfg(test)]
        START_SERVER_CALLS.fetch_add(1, Ordering::Relaxed);
        drop(inner);
        drop(runtime_state);
        drop(addr);
        return;
    }

    thread::spawn(move || {
        let runtime = Builder::new_current_thread().enable_all().build();
        let Ok(runtime) = runtime else {
            eprintln!("eguidev: failed to start tokio runtime");
            return;
        };
        let server =
            Server::new(move || AppMcpServer::new(Arc::clone(&inner), Arc::clone(&runtime_state)));
        let result: tmcp::Result<()> = runtime.block_on(async move {
            let _server = server.serve_tcp(addr).await?;
            pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });
        if let Err(error) = result {
            eprintln!("eguidev: MCP server failed: {error}");
        }
    });
}

#[cfg(test)]
pub fn reset_start_server_calls() {
    START_SERVER_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub fn start_server_calls() -> usize {
    START_SERVER_CALLS.load(Ordering::Relaxed)
}
