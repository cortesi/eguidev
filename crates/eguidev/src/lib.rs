//! Instrumentation for AI-assisted egui development.
//!
//! eguidev captures widget state at frame boundaries and injects input through
//! egui's `raw_input_hook`, keeping automation aligned with the app's real event
//! loop. Coding agents drive the app through Luau scripts executed by
//! [`script_eval`](crate::tools::ScriptEvalRequest); a single script can
//! inspect widgets, queue input, wait for state changes, and return structured
//! results in one round trip.
//!
//! # Quick start
//!
//! Add a [`DevMcp`] handle to your app state, wrap each frame with
//! [`FrameGuard`], and forward raw input to [`raw_input_hook`]. In default
//! builds the handle is inert; enable the `devtools` feature and call
//! [`runtime::attach`] in one bootstrap location for the embedded MCP runtime.
//!
//! ```rust
//! use eframe::{App, egui};
//! use eguidev::{DevMcp, DevUiExt, FrameGuard};
//!
//! struct MyApp {
//!     devmcp: DevMcp,
//!     name: String,
//! }
//!
//! impl MyApp {
//!     fn new() -> Self {
//!         Self {
//!             devmcp: DevMcp::new(),
//!             name: String::new(),
//!         }
//!     }
//! }
//!
//! impl App for MyApp {
//!     fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
//!         let ctx = ui.ctx().clone();
//!         let _guard = FrameGuard::new(&self.devmcp, &ctx);
//!         egui::Frame::central_panel(ui.style()).show(ui, |ui| {
//!             ui.dev_text_edit("app.name", &mut self.name);
//!             if ui.dev_button("app.submit", "Submit").clicked() {
//!                 // ...
//!             }
//!         });
//!     }
//!
//!     fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
//!         eguidev::raw_input_hook(&self.devmcp, ctx, raw_input);
//!     }
//! }
//! ```
//!
//! # Build modes
//!
//! - **Default build**: depend on `eguidev` without `devtools`. [`DevMcp`],
//!   [`FrameGuard`], [`raw_input_hook`], widget tagging, and fixtures all
//!   compile and stay inert. The dependency tree excludes tokio, mlua, tmcp,
//!   image, base64, and glob.
//! - **Dev-capable build**: add an app feature like
//!   `devtools = ["eguidev/devtools"]` and call [`runtime::attach`] in one
//!   bootstrap location. The same binary serves both release and dev runs; the
//!   only `#[cfg]` boundary belongs in that bootstrap path, not in widget code.
//!
//! # Instrumenting widgets
//!
//! Use the [`DevUiExt`] trait for standard widgets. Each `dev_*` method takes
//! an explicit string id and auto-populates role, label, and value metadata:
//!
//! ```rust,ignore
//! ui.dev_button("settings.save", "Save");
//! ui.dev_text_edit("settings.name", &mut name);
//! ui.dev_checkbox("settings.enabled", &mut enabled, "Enabled");
//! ui.dev_slider("settings.level", &mut level, 0.0..=100.0);
//! ```
//!
//! For custom widgets, use [`id`] (geometry only) or [`id_with_meta`] (explicit
//! role/value/label). If you already have an `egui::Response`, use
//! [`track_response_full`] to register it after the fact. Use [`container`] to
//! annotate hierarchy so scripts can traverse parent/child relationships.
//!
//! Widget ids are the one canonical selector in the scripting API. Explicit ids
//! must be unique within a captured frame; duplicates are treated as a hard
//! automation fault.
//!
//! # Fixtures
//!
//! Apps register fixtures with [`DevMcp::fixtures`] and handle requests by
//! polling [`DevMcp::collect_fixture_requests`] in `update`. Each fixture must
//! be independently invokable from any prior state and must leave the app in
//! its declared baseline. Scripts call `fixture("name")` to reset before
//! interacting with the UI.
//!
//! # Smoketests
//!
//! The [`smoke`] module provides a built-in suite runner. A suite is a
//! directory of self-contained `.luau` scripts, executed in lexicographic
//! order by relative path. Each script establishes its own state via
//! `fixture()`, exercises the UI, and asserts outcomes. Run with `edev smoke`.
//!
//! # Scripting reference
//!
//! The canonical Luau API is defined in `eguidev.d.luau`, retrievable at
//! runtime via `script_api` or `edev --script-docs`. It covers viewports,
//! widgets, actions, waits, fixtures, and assertions.

#![allow(clippy::missing_docs_in_private_items)]

// In the default build (no devtools), most internal modules are structurally
// dead: `Inner` has no constructor, so nothing that exists only to serve `Inner` can
// run. Suppress dead-code warnings on those internal modules; the devtools CI
// build still catches genuinely unused code.
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod actions;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod devmcp;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod error;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod fixtures;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod instrument;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod overlay;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod registry;
#[cfg(feature = "devtools")]
pub mod runtime;
#[cfg(feature = "devtools")]
mod screenshots;
#[cfg(feature = "devtools")]
mod script_docs;
#[cfg(feature = "devtools")]
mod server;
#[cfg(feature = "devtools")]
pub mod smoke;
#[cfg(feature = "devtools")]
mod tools;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod tree;
pub(crate) mod types;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod ui_ext;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod viewports;
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
mod widget_registry;

pub use crate::{
    devmcp::{DevMcp, FrameGuard, raw_input_hook},
    fixtures::FixtureRequest,
    instrument::{
        ContainerGuard, ScrollAreaState, container, id, id_with_meta, track_response_full,
    },
    types::{
        FixtureSpec, RoleState, ScrollAreaMeta, WidgetLayout, WidgetRange, WidgetRole, WidgetState,
        WidgetValue,
    },
    ui_ext::{
        ButtonOptions, CheckboxOptions, DevScrollAreaExt, DevUiExt, ProgressBarOptions,
        TextEditOptions,
    },
    widget_registry::WidgetMeta,
};
#[cfg(feature = "devtools")]
pub use crate::{
    script_docs::{render_script_docs_markdown, script_definitions},
    tools::{
        ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo, ScriptEvalOptions,
        ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation, ScriptTiming,
    },
};
