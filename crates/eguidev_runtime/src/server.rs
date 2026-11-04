//! MCP server implementation for DevMCP.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{future::pending, sync::Arc, thread};

use tmcp::Server;
use tokio::runtime::Builder;

use crate::{mcp::AppMcpServer, registry::Inner, runtime::Runtime};

/// The path of the Unix domain socket at which the app serves its MCP server.
/// Edev names a new path for each launch.
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
        let result: tmcp::Result<()> = runtime.block_on(serve(server, addr));
        if let Err(error) = result {
            eprintln!("eguidev: MCP server failed: {error}");
        }
    });
}

/// Serve the app MCP server at the socket path `addr`, mode 0600, until the
/// process ends.
#[cfg(unix)]
async fn serve(server: Server, addr: String) -> tmcp::Result<()> {
    let _server = server.serve_unix(addr).await?;
    pending::<()>().await;
    Ok(())
}

/// Report that the app endpoint needs a Unix domain socket.
#[cfg(not(unix))]
async fn serve(_server: Server, _addr: String) -> tmcp::Result<()> {
    Err(tmcp::Error::InvalidConfiguration(
        "the eguidev app endpoint needs a Unix domain socket".to_string(),
    ))
}

#[cfg(test)]
pub fn reset_start_server_calls() {
    START_SERVER_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub fn start_server_calls() -> usize {
    START_SERVER_CALLS.load(Ordering::Relaxed)
}
