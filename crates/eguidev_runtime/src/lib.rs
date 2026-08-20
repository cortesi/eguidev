//! Native embedded runtime for `eguidev`.
//!
//! `eguidev_runtime` attaches the in-process automation server, script
//! evaluation, screenshots, and smoke runner to an inert [`eguidev::DevMcp`]
//! handle.
//!
//! For `eframe` applications, the most reliable integration pattern is:
//!
//! - choose `eframe::Renderer::Glow` for automation runs when possible
//! - register a fixture handler with [`eguidev::DevMcp::on_fixture_runtime`] or
//!   [`eguidev::DevMcp::on_fixture_ui`]
//! - wrap every frame in [`eguidev::FrameGuard`], which registers an egui
//!   plugin on the first frame to inject input automatically
//!
//! The `wgpu` backend can exhibit idle-frame stalls in some `eframe`
//! integrations, so the demo and examples prefer `Glow`.

#![allow(clippy::missing_docs_in_private_items)]

use std::{env, ffi::OsStr};

#[cfg(target_arch = "wasm32")]
compile_error!("eguidev_runtime is native-only and is not supported on wasm32 targets");

mod automation;
mod dump;
mod egui_diagnostics;
mod error;
#[cfg(target_os = "macos")]
mod macos;
mod mcp;
mod presentation;
mod runtime;
mod screenshots;
mod script_docs;
mod server;
pub mod smoke;

pub(crate) mod actions {
    pub use eguidev::internal::actions::*;
}

pub(crate) mod diagnostics {
    pub use eguidev::internal::diagnostics::*;
}

pub(crate) mod fixtures {
    pub use eguidev::internal::fixtures::*;
}

pub(crate) mod overlay {
    pub use eguidev::internal::overlay::*;
}

pub(crate) mod registry {
    pub use eguidev::internal::registry::*;
}

pub(crate) mod tree {
    pub use eguidev::internal::tree::*;
}

pub(crate) mod types {
    pub use eguidev::internal::types::*;
}

pub(crate) mod ui_ext {
    pub use eguidev::internal::ui_ext::*;
}

pub(crate) mod viewports {
    pub use eguidev::internal::viewports::*;
}

pub(crate) use automation::script;
pub use eguidev::{DevMcp, Rect, ScrollAreaMeta};

/// Return whether this process was launched for Eguidev automation.
pub fn automation_launch() -> bool {
    matches!(
        mcp_endpoint_from(env::var_os(MCP_ADDR_ENV).as_deref()),
        McpEndpoint::Valid(_)
    )
}

#[derive(Debug, PartialEq, Eq)]
enum McpEndpoint {
    Absent,
    Invalid(String),
    Valid(String),
}

fn mcp_endpoint_from(endpoint: Option<&OsStr>) -> McpEndpoint {
    let Some(endpoint) = endpoint else {
        return McpEndpoint::Absent;
    };
    let Some(text) = endpoint.to_str() else {
        return McpEndpoint::Invalid(format!("{MCP_ADDR_ENV} is not valid UTF-8"));
    };
    if text.is_empty() {
        return McpEndpoint::Invalid(format!("{MCP_ADDR_ENV} is empty"));
    }
    McpEndpoint::Valid(text.to_string())
}

/// Keep a background-launched app from taking focus for its whole run.
///
/// On macOS this enables the activation guard: while the automation
/// presentation is background (the default), any activation of the app that
/// was not caused by a user mouse interaction is immediately handed back to
/// the previously frontmost application. Call before the app's event loop
/// starts. On other platforms this is a no-op.
pub fn enable_background_launch_guard() {
    if !automation_launch() {
        return;
    }
    #[cfg(target_os = "macos")]
    macos::enable_background_launch_guard();
}

#[doc(hidden)]
pub use server::MCP_ADDR_ENV;

pub use crate::{
    automation::{
        FixtureApplication, ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo,
        ScriptEvalOptions, ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation,
        ScriptTiming,
        script::{CheckFailure as ScriptCheckError, check_source as check_script_source},
    },
    egui_diagnostics::{
        EguiDiagnostic, EguiDiagnosticBatch, EguiDiagnosticKind, EguiDiagnosticSeverity,
    },
    runtime::{attach, eval_script},
    script_docs::script_definitions,
};

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{McpEndpoint, mcp_endpoint_from};

    #[test]
    fn automation_activation_uses_endpoint_presence() {
        assert_eq!(mcp_endpoint_from(None), McpEndpoint::Absent);
        assert!(matches!(
            mcp_endpoint_from(Some(OsStr::new(""))),
            McpEndpoint::Invalid(_)
        ));
        assert_eq!(
            mcp_endpoint_from(Some(OsStr::new("127.0.0.1:9000"))),
            McpEndpoint::Valid("127.0.0.1:9000".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn automation_activation_rejects_non_utf8_endpoint() {
        use std::os::unix::ffi::OsStrExt;

        assert!(matches!(
            mcp_endpoint_from(Some(OsStr::from_bytes(b"127.0.0.1:\xff"))),
            McpEndpoint::Invalid(_)
        ));
    }
}

#[cfg(test)]
pub(crate) mod widget_registry {
    pub use eguidev::internal::widget_registry::*;
}
