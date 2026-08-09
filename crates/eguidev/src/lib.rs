//! Cross-target instrumentation for AI-assisted egui development.
//!
//! `eguidev` captures widget state at frame boundaries and injects input
//! through an egui plugin registered on the first instrumented frame, keeping
//! automation aligned with the app's real event loop. This crate is the
//! instrumentation half of the automation stack: it is valid for native and
//! `wasm32` builds, and it intentionally does not ship the embedded script
//! runtime, MCP server, or screenshot machinery.
//!
//! # Quick start
//!
//! Add a [`DevMcp`] handle to your app state and wrap each frame with
//! [`FrameGuard`]. Input injection is automatic from there: the first
//! [`FrameGuard`] registers an egui plugin that drains queued input into
//! every pass of every viewport, so apps do not need any raw-input wiring.
//! The handle stays inert until a native app opts into `eguidev_runtime` and
//! attaches the embedded runtime in one bootstrap location.
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
//! }
//! ```
//!
//! # Build modes
//!
//! - **Cross-target instrumentation**: depend on `eguidev` only. [`DevMcp`],
//!   [`FrameGuard`], widget tagging, and fixtures all compile for native and
//!   `wasm32` targets.
//! - **Native embedded runtime**: add an app-local feature that enables the
//!   optional `eguidev_runtime` dependency, then call
//!   `eguidev_runtime::attach(devmcp)` in one bootstrap location. Keep that
//!   branch local to startup code instead of pushing `#[cfg]` into widget code.
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
//! Custom widgets have their own recording primitives. Pick by what you hold:
//!
//! | You hold                                    | Use                        |
//! | ------------------------------------------- | -------------------------- |
//! | a closure that draws and returns a response  | [`track_widget`]           |
//! | the same, and a role to declare              | [`track_widget_with_meta`] |
//! | an `egui::Response` already                  | [`track_response`]         |
//! | a painter-drawn rect                         | [`publish_rect_meta`]      |
//! | a painter-drawn rect with children           | [`publish_rect_container`] |
//! | a scope whose widget is recorded elsewhere   | [`begin_container`]        |
//! | a scope and its widget together              | [`container`]              |
//!
//! `track_*` records something that has an `egui::Response`, so enabled,
//! focused, and visible state come from egui. `publish_rect_*` publishes
//! painter-drawn geometry, which has no response, so [`WidgetMeta`] states that
//! metadata instead.
//!
//! Set [`WidgetMeta::visible`] on every widget you record by hand. It defaults
//! to `false`, and an invisible widget is hidden from the default lookups.
//!
//! Custom widgets that should accept scripted `set_value(...)` calls must also
//! consume queued overrides before rendering:
//!
//! ```rust,ignore
//! let id = "settings.mode";
//! if let Some(crate::WidgetValue::Int(index)) = take_widget_value_override(ui, id) {
//!     *selected = index as usize;
//! }
//!
//! let response = render_custom_mode_picker(ui, selected);
//! track_response(
//!     id,
//!     &response,
//!     WidgetMeta {
//!         role: WidgetRoleMeta::ComboBox {
//!             options: mode_labels.clone(),
//!         },
//!         value: Some(WidgetValue::Int(*selected as i64)),
//!         visible: ui.is_visible() && ui.is_rect_visible(response.rect),
//!         ..Default::default()
//!     },
//! );
//! ```
//!
//! Only the variant the script sends is consumed. A `set_value` whose type does
//! not match the pattern above is dropped, so match the variant your role
//! publishes.
//!
//! [`WidgetRoleMeta`] carries the metadata that each role requires, so a
//! recorded scroll area always has a content size and a recorded slider always
//! has a range. Use [`WidgetRoleMeta::Plain`] only for roles with no metadata,
//! such as a label or a separator.
//!
//! Painter-drawn output has no `egui::Response`. Publish a rect with
//! [`publish_rect_meta`], and publish a painter-drawn hierarchy with
//! [`publish_rect_container`] so the children nest under their canvas instead
//! of overlapping it.
//!
//! Widget ids are the one canonical selector in the scripting API. Explicit ids
//! must be unique within a captured frame; duplicates are treated as a hard
//! automation fault.
//!
//! # Fixtures
//!
//! Apps register fixtures with [`DevMcp::fixtures`] and exactly one handler:
//! [`DevMcp::on_fixture_runtime`] for automation-thread setup or
//! [`DevMcp::on_fixture_ui`] for setup that must run on the egui UI thread.
//! Registering a second handler, of either kind, is a configuration error.
//! A handler reaches app state by closing over it, usually an
//! `Arc<Mutex<AppState>>` shared with the app's render code.
//! A fixture without preconditions is an independently invokable baseline. A
//! fixture with preconditions is a transition that callers invoke only after
//! establishing its declared entry state. Both kinds leave the app in a state
//! described by ready conditions. Use typed fixture params for controlled
//! variants and handler-returned values or ready for dynamic fixture outcomes.
//! Scripts call `eguidev.fixture("name", params?)` to apply the named fixture, wait for a
//! fresh capture, verify static and returned ready conditions, and receive the
//! returned values. Widget and viewport references resolve fresh across fixture
//! boundaries. Pass `{ wait = false }` only when a script intentionally needs
//! the state before ready conditions are satisfied.
//!
//! # Scripting reference
//!
//! The canonical Luau API, direct script evaluation helpers, and the smoketest
//! runner all live in `eguidev_runtime`. The app serves the checked-in
//! definitions through `script_api`; `edev docs` prints the same bytes.

#![allow(clippy::missing_docs_in_private_items)]

mod actions;
mod devmcp;
mod diagnostics;
mod error;
mod fixtures;
mod helpers;
mod idle;
mod instrument;
mod overlay;
mod presentation;
mod registry;
mod tree;
pub(crate) mod types;
mod ui_ext;
mod viewports;
mod widget_registry;

pub use crate::{
    devmcp::{AutomationOptions, DevMcp, FrameGuard, clear_viewport, frame_scope},
    diagnostics::{DevMcpConfigError, DiagnosticError, DiagnosticResult},
    helpers::{TypedFixture, TypedFixtures, value_anchor, viewport_frame},
    instrument::{
        ContainerGuard, ScrollAreaState, begin_container, capture_layout, container, name_viewport,
        publish_rect_container, publish_rect_meta, track_response, track_widget,
        track_widget_with_meta,
    },
    types::{
        ActionOptions, ClickOptions, DragOptions, FixtureCall, FixtureError, FixtureParam,
        FixtureParams, FixtureResponse, FixtureResult, FixtureSpec, FixtureTargetSpec,
        HoverOptions, KeyOptions, PaintExpectation, ParamKind, PointerButton, Pos2, RawInputAction,
        RawInputEvent, Rect, RelationExpectation, ResizeOptions, RoleState, ScrollAlign,
        ScrollAreaMeta, ScrollToOptions, TypeTextOptions, Vec2, ViewportCondition,
        ViewportNameError, ViewportSel, ViewportSelParseError, WaitOptions, WidgetCondition,
        WidgetExpectation, WidgetLayout, WidgetRange, WidgetRef, WidgetRole, WidgetRoleMeta,
        WidgetState, WidgetValue,
    },
    ui_ext::{
        ButtonOptions, CheckboxOptions, DevScrollAreaExt, DevUiExt, ProgressBarOptions,
        TextEditOptions, take_widget_value_override,
    },
    widget_registry::{WidgetDataError, WidgetMeta},
};
#[doc(hidden)]
pub mod internal {
    pub mod actions {
        pub use crate::actions::{ActionQueue, ActionQueueStats, ActionTiming, InputAction};
    }

    pub mod devmcp {
        pub use crate::devmcp::{AutomationOptions, RuntimeHooks};
    }

    pub mod diagnostics {
        pub use crate::diagnostics::{DiagnosticExecution, DiagnosticRegistry};
    }

    pub mod fixtures {
        pub use crate::fixtures::{FixtureExecution, FixtureHandler};
    }

    pub mod idle {
        pub use crate::idle::{IdleRegistry, IdleStatus};
    }

    pub mod error {
        pub use crate::error::{ErrorCode, ToolError};
    }

    pub mod overlay {
        pub use crate::overlay::{
            OverlayDebugConfig, OverlayDebugMode, OverlayDebugOptions, OverlayEntry,
            OverlayManager, parse_color, rect_intersection, rect_size,
        };
    }

    pub mod presentation {
        pub use crate::presentation::{
            EXPERIMENTAL_PRESENTATION_CAPABILITY, Presentation, PresentationStatus,
        };
    }

    pub mod registry {
        pub use crate::registry::{Inner, lock, viewport_id_to_string};
    }

    pub mod tree {
        pub use crate::tree::collect_subtree;
    }

    pub mod types {
        pub use crate::types::{
            ActionOptions, ClickOptions, DragOptions, FixtureCall, FixtureError, FixtureParam,
            FixtureParams, FixtureResponse, FixtureResult, FixtureSpec, FixtureTargetSpec,
            HoverOptions, KeyOptions, Modifiers, PaintExpectation, ParamKind, PointerButton, Pos2,
            RawInputAction, RawInputEvent, Rect, RelationExpectation, ResizeOptions, RoleState,
            ScrollAlign, ScrollAreaMeta, ScrollToOptions, TypeTextOptions, Vec2, ViewportCondition,
            ViewportNameError, ViewportSel, ViewportSelParseError, WaitOptions, WidgetCondition,
            WidgetExpectation, WidgetLayout, WidgetRange, WidgetRef, WidgetRegistryEntry,
            WidgetRole, WidgetRoleMeta, WidgetState, WidgetValue,
        };
    }

    pub mod ui_ext {
        pub use crate::ui_ext::parse_color_hex;
    }

    pub mod viewports {
        pub use crate::viewports::{
            FrameHealth, InputSnapshot, PlatformViewportState, ViewportSnapshot, ViewportState,
        };
    }

    pub mod widget_registry {
        pub use crate::widget_registry::{
            WidgetDataError, WidgetMeta, WidgetRegistry, record_widget,
        };
    }
}
