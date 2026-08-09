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

/// Keep a background-launched app from taking focus for its whole run.
///
/// On macOS this enables the activation guard: while the automation
/// presentation is background (the default), any activation of the app that
/// was not caused by a user mouse interaction is immediately handed back to
/// the previously frontmost application. Call before the app's event loop
/// starts. On other platforms this is a no-op.
pub fn enable_background_launch_guard() {
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
    },
    egui_diagnostics::{
        EguiDiagnostic, EguiDiagnosticBatch, EguiDiagnosticKind, EguiDiagnosticSeverity,
    },
    runtime::{attach, eval_script},
    script_docs::script_definitions,
};

#[cfg(test)]
pub(crate) mod widget_registry {
    pub use eguidev::internal::widget_registry::*;
}
