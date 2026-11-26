//! Internal egui automation operations used by the private Luau kernel.
//!
//! The public MCP adapter is isolated in `mcp.rs`; this module does not define
//! top-level tools.

#![allow(clippy::multiple_inherent_impl)]

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::codecs::jpeg::JpegEncoder;
#[cfg(test)]
use ruau_script_api::ScriptApiQuery;
use serde::Serialize;
use serde_json::{Value, json};
use tmcp::ToolResult;
#[cfg(test)]
use tmcp::schema::CallToolResult;
use tokio::time::{sleep, timeout};

#[cfg(target_os = "macos")]
use crate::macos::{capture_window_image, window_number_for_title};
#[cfg(test)]
use crate::script_docs::{script_api_response, script_api_tool_result};
use crate::{
    actions::{ActionTiming, InputAction},
    fixtures::FixtureExecution,
    overlay::{
        OverlayDebugConfig, OverlayDebugMode, OverlayDebugOptions, OverlayEntry, parse_color,
        rect_intersection,
    },
    registry::{Inner, viewport_id_to_string},
    runtime::Runtime,
    screenshots::{ScreenshotKind, ScreenshotState},
    tree::collect_subtree,
    types::{
        FixtureCall, FixtureTargetSpec, Modifiers, PointerButton, Pos2, RawInputAction,
        RawInputEvent, Rect, ResizeOptions, RoleState, ScrollAlign, Vec2, WidgetRef,
        WidgetRegistryEntry, WidgetRole, WidgetState, WidgetValue,
    },
    viewports::ViewportSnapshot,
};

pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 16;
const MAX_SAMPLE_GRID_COUNT: u64 = 512;
const MAX_KEY_REPEAT: u32 = 1_000;
const SCROLL_STABILITY_TOLERANCE: f32 = 0.75;
const WIDGET_SAMPLE_GRID_INSET: f32 = 1.0;
mod action;
mod capture;
mod diagnostic;
mod fixture;
mod layout;
mod query;
mod results;
pub mod script;
mod types;
mod utils;
mod wait;

#[cfg(test)]
use action::pointer_ready;
use action::{HoverConfirm, interaction_ready};
use capture::{
    ScreenshotFormat, ScreenshotOptions, capture_native_screenshot, capture_screenshot,
    resolve_screenshot_viewport, viewport_frame,
};
#[cfg(test)]
use capture::{
    crop_native_capture_to_viewport, screenshot_timeout_message,
    should_try_native_screenshot_fallback,
};
#[cfg(test)]
use capture::{sample_color_image, sample_widget_grid, sample_widget_relative_pixels};
use layout::*;
#[cfg(test)]
use query::covering_widget_id;
pub use query::filter_viewport_snapshots;
use query::{collect_widget_list, widgets_at_point};
use results::*;
pub use script::{
    FixtureApplication, ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo,
    ScriptEvalOptions, ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation,
    ScriptModules, ScriptTiming,
};
use types::{OverlayDebugModeName, OverlayDebugOptionsInput};
pub use utils::ensure_automation_ready;
use utils::{parse_key_combo, resolve_key_name, *};
#[cfg(test)]
use wait::{SettlePhase, settle_report};

pub use crate::error::*;

fn scroll_state(widget: &WidgetRegistryEntry) -> Option<crate::ScrollAreaMeta> {
    widget.role_state.as_ref().and_then(RoleState::scroll_state)
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct FixtureApplyOutcome {
    pub params: BTreeMap<String, WidgetValue>,
    pub values: BTreeMap<String, WidgetValue>,
    pub ready: Vec<FixtureTargetSpec>,
}

pub struct DevMcpServer {
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
}

impl DevMcpServer {
    #[cfg(test)]
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        let runtime = Runtime::ensure_for_inner(&inner);
        Self { inner, runtime }
    }

    pub(crate) fn with_runtime(inner: Arc<Inner>, runtime: Arc<Runtime>) -> Self {
        Self { inner, runtime }
    }

    #[cfg(test)]
    /// Evaluate a Luau script with DevMCP helpers. Scripts are assumed to be
    /// strict.
    async fn script_eval(
        &self,
        script: String,
        timeout_ms: Option<u64>,
        options: Option<ScriptEvalOptions>,
    ) -> ToolResult<CallToolResult> {
        let timeout_ms = timeout_ms.unwrap_or(script::DEFAULT_SCRIPT_TIMEOUT_MS);
        let inner = Arc::clone(&self.inner);
        let runtime = Arc::clone(&self.runtime);
        let eval = script::run_script_eval_with_modules(
            inner,
            runtime,
            script,
            timeout_ms,
            options.unwrap_or_default(),
        )
        .await;
        Ok(eval.to_tool_result())
    }

    #[cfg(test)]
    /// Return shared discovery for the checked scripting API.
    async fn script_api(&self, params: ScriptApiQuery) -> ToolResult<CallToolResult> {
        script_api_tool_result(script_api_response(&params))
    }
}

/// Script deadline for tests that do not test the deadline. It covers Luau
/// compilation, which is slow and highly variable under parallel test load,
/// so it must stay far above the inner timeout that a test exercises.
#[cfg(test)]
const TEST_SCRIPT_DEADLINE_MS: u64 = 10_000;

#[cfg(test)]
#[allow(deprecated)]
mod tests;
