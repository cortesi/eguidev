//! Eguidev MCP launcher binary.

use std::process;

/// Entry point for the eguidev MCP launcher. The exit code of an error comes
/// from `EdevError::exit_code`.
#[tokio::main]
async fn main() {
    if let Err(error) = edev::run().await {
        eprintln!("edev: {error}");
        process::exit(i32::from(error.exit_code()));
    }
}
