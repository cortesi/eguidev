//! Embedded runtime attachment for DevMCP automation.

use std::sync::Arc;

use crate::{DevMcp, registry::Inner, server::start_server};

/// Attach the embedded runtime to an inert `DevMcp` handle.
///
/// The returned handle owns an active automation runtime and starts the embedded
/// MCP server on its own thread.
pub fn attach(devmcp: DevMcp) -> DevMcp {
    attach_internal(devmcp, true)
}

fn attach_internal(devmcp: DevMcp, should_start_server: bool) -> DevMcp {
    if devmcp.is_enabled() {
        return devmcp;
    }

    let inner = Arc::new(Inner::new());
    let devmcp = devmcp.activate(Arc::clone(&inner));
    if should_start_server {
        start_server(inner);
    }
    devmcp
}

#[cfg(test)]
pub(crate) fn attach_for_tests(devmcp: DevMcp) -> DevMcp {
    attach_internal(devmcp, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{reset_start_server_calls, start_server_calls};

    #[test]
    fn inactive_handle_does_not_start_server() {
        reset_start_server_calls();
        let _devmcp = DevMcp::new();
        assert_eq!(start_server_calls(), 0);
    }

    #[test]
    fn attach_starts_server_once() {
        reset_start_server_calls();
        let _devmcp = attach(DevMcp::new());
        assert_eq!(start_server_calls(), 1);
    }
}
