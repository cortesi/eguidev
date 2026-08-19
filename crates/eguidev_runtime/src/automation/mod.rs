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
use serde::Serialize;
use serde_json::{Value, json};
use tmcp::ToolResult;
#[cfg(test)]
use tmcp::schema::CallToolResult;
use tokio::{
    task::spawn_blocking,
    time::{sleep, timeout},
};

#[cfg(target_os = "macos")]
use crate::macos::{capture_window_image, window_number_for_title};
#[cfg(test)]
use crate::script_definitions;
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
const SCROLL_STABILITY_TOLERANCE: f32 = 0.75;
const WIDGET_SAMPLE_GRID_INSET: f32 = 1.0;
mod action;
mod capture;
mod diagnostic;
mod fixture;
mod layout;
mod query;
mod results;
#[path = "../script/mod.rs"]
pub mod script;
mod types;
mod utils;
mod wait;

use capture::{capture_screenshot, resolve_screenshot_viewport};
#[cfg(test)]
use capture::{
    crop_native_capture_to_viewport, screenshot_timeout_message,
    should_try_native_screenshot_fallback,
};
use layout::*;
pub(crate) use query::filter_viewport_snapshots;
use query::widgets_at_point;
use results::*;
pub use script::{
    FixtureApplication, ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo,
    ScriptEvalOptions, ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation,
    ScriptTiming,
};
use types::{OverlayDebugModeName, OverlayDebugOptionsInput};
pub(crate) use utils::ensure_automation_ready;
use utils::{parse_key_combo, resolve_key_name, *};

pub use crate::error::*;

pub const DEFAULT_SCRIPT_EVAL_TIMEOUT_MS: u64 = script::DEFAULT_SCRIPT_TIMEOUT_MS;

fn scroll_state(widget: &WidgetRegistryEntry) -> Option<crate::ScrollAreaMeta> {
    widget.role_state.as_ref().and_then(RoleState::scroll_state)
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct FixtureApplyOutcome {
    pub params: BTreeMap<String, WidgetValue>,
    pub values: BTreeMap<String, WidgetValue>,
    pub ready: Vec<FixtureTargetSpec>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SettleReport {
    pub settled: bool,
    pub elapsed_ms: u64,
    pub phases: Vec<SettlePhaseStatus>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SettlePhaseStatus {
    pub phase: SettlePhase,
    pub complete: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettlePhase {
    InputDrained,
    CommandsDrained,
    ActionFrameProcessed,
    CleanCapture,
    FreshFrame,
    AppIdle,
}

impl SettlePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::InputDrained => "input_drained",
            Self::CommandsDrained => "commands_drained",
            Self::ActionFrameProcessed => "action_frame_processed",
            Self::CleanCapture => "clean_capture",
            Self::FreshFrame => "fresh_frame",
            Self::AppIdle => "app_idle",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct PixelSample {
    position: Pos2,
    physical: [usize; 2],
    rgba: [u8; 4],
    hex: String,
}

fn reveal_axis(
    current: f32,
    viewport_min: f32,
    viewport_size: f32,
    item_min: f32,
    item_max: f32,
    max_offset: f32,
) -> f32 {
    let content_min = current + (item_min - viewport_min);
    let content_max = current + (item_max - viewport_min);
    let mut target = current;
    if content_min < current {
        target = content_min;
    } else if content_max > current + viewport_size {
        target = content_max - viewport_size;
    }
    target.clamp(0.0, max_offset.max(0.0))
}

fn scroll_area_target_offset(
    scroll_area: &WidgetRegistryEntry,
    target: &WidgetRegistryEntry,
) -> Option<Vec2> {
    let scroll = scroll_state(scroll_area)?;
    let current: egui::Vec2 = scroll.offset.into();
    Some(Vec2 {
        x: reveal_axis(
            current.x,
            scroll_area.rect.min.x,
            scroll.viewport_size.x,
            target.rect.min.x,
            target.rect.max.x,
            scroll.max_offset.x,
        ),
        y: reveal_axis(
            current.y,
            scroll_area.rect.min.y,
            scroll.viewport_size.y,
            target.rect.min.y,
            target.rect.max.y,
            scroll.max_offset.y,
        ),
    })
}

fn egui_pointer_button(button: PointerButton) -> egui::PointerButton {
    match button {
        PointerButton::Primary => egui::PointerButton::Primary,
        PointerButton::Secondary => egui::PointerButton::Secondary,
        PointerButton::Middle => egui::PointerButton::Middle,
    }
}

fn settle_report(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    start_capture: u64,
    start_frame: u64,
    elapsed_ms: u64,
) -> SettleReport {
    let capture = inner.viewports.capture_snapshot(viewport_id);
    let capture_frame = capture.map(|snapshot| snapshot.frame_count);
    let observed_new_capture = capture_frame.is_some_and(|frame| frame > start_capture);
    let target_viewport_closed =
        viewport_id != egui::ViewportId::ROOT && !inner.viewports.is_live_viewport(viewport_id);
    let observed_settle_frame = if viewport_id == egui::ViewportId::ROOT {
        inner.frame_count() > start_frame
    } else {
        observed_new_capture
    };
    let pending_actions = inner.actions.pending_action_count(viewport_id);
    let pending_commands = inner.actions.pending_command_count(viewport_id);
    let last_action_frame = inner.last_action_frame.load(Ordering::Relaxed);
    let processed_last_action = inner.frame_count() > last_action_frame;
    let action_stats = inner.actions.stats(viewport_id);
    let observed_clean_action_frame = action_stats.last_drain_frame.is_none_or(|frame| {
        capture_frame.is_some_and(|capture_frame| capture_frame > frame.saturating_add(1))
    });

    let mut phases = vec![
        SettlePhaseStatus {
            phase: SettlePhase::InputDrained,
            complete: pending_actions == 0,
            detail: if pending_actions == 0 {
                "no pending input actions".to_string()
            } else {
                format!("{pending_actions} input action(s) pending")
            },
        },
        SettlePhaseStatus {
            phase: SettlePhase::CommandsDrained,
            complete: pending_commands == 0,
            detail: if pending_commands == 0 {
                "no pending viewport commands".to_string()
            } else {
                format!("{pending_commands} viewport command(s) pending")
            },
        },
        SettlePhaseStatus {
            phase: SettlePhase::ActionFrameProcessed,
            complete: processed_last_action,
            detail: format!(
                "last action frame {last_action_frame}, current frame {}",
                inner.frame_count()
            ),
        },
        SettlePhaseStatus {
            phase: SettlePhase::CleanCapture,
            complete: observed_clean_action_frame || target_viewport_closed,
            detail: if target_viewport_closed {
                "target viewport closed after action drain".to_string()
            } else {
                format!(
                    "last drain frame {}, latest capture {}",
                    action_stats
                        .last_drain_frame
                        .map(|frame| frame.to_string())
                        .unwrap_or_else(|| "none".to_string()),
                    capture_frame
                        .map(|frame| frame.to_string())
                        .unwrap_or_else(|| "none".to_string())
                )
            },
        },
        SettlePhaseStatus {
            phase: SettlePhase::FreshFrame,
            complete: observed_settle_frame || target_viewport_closed,
            detail: if target_viewport_closed {
                "target viewport closed during wait".to_string()
            } else {
                format!(
                    "start frame {start_frame}, current frame {}, start capture {start_capture}, latest capture {}",
                    inner.frame_count(),
                    capture_frame
                        .map(|frame| frame.to_string())
                        .unwrap_or_else(|| "none".to_string())
                )
            },
        },
    ];

    if let Some(idle) = inner.idle.status() {
        phases.push(SettlePhaseStatus {
            phase: SettlePhase::AppIdle,
            complete: idle.idle,
            detail: idle.detail,
        });
    }

    let settled = phases.iter().all(|phase| phase.complete);
    SettleReport {
        settled,
        elapsed_ms,
        phases,
    }
}

fn settle_timeout_message(timeout_ms: u64, report: &SettleReport) -> String {
    let incomplete = report
        .phases
        .iter()
        .filter(|phase| !phase.complete)
        .map(|phase| format!("{} ({})", phase.phase.as_str(), phase.detail))
        .collect::<Vec<_>>();
    if incomplete.is_empty() {
        format!("Timed out waiting for UI to settle after {timeout_ms}ms")
    } else {
        format!(
            "Timed out waiting for UI to settle after {timeout_ms}ms; incomplete: {}",
            incomplete.join(", ")
        )
    }
}

fn settle_timeout_details(
    elapsed_ms: u64,
    viewport: Option<&ViewportSnapshot>,
    start_capture: u64,
    end_capture: Option<u64>,
    observation: &WaitObservation,
    report: &SettleReport,
) -> Value {
    let mut details = wait_timeout_details(
        "settle",
        elapsed_ms,
        None,
        viewport,
        Some(start_capture),
        end_capture,
        observation,
    );
    if let Some(map) = details.as_object_mut() {
        map.insert("phases".to_string(), json!(report.phases));
    }
    details
}

fn sample_color_image(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    positions: &[Pos2],
) -> Result<Vec<PixelSample>, ToolError> {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "Invalid pixels_per_point for pixel sampling",
        ));
    }
    positions
        .iter()
        .map(|position| sample_color_pixel(image, pixels_per_point, *position))
        .collect()
}

fn sample_color_pixel(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    position: Pos2,
) -> Result<PixelSample, ToolError> {
    let physical_x = (position.x * pixels_per_point).floor();
    let physical_y = (position.y * pixels_per_point).floor();
    if !physical_x.is_finite() || !physical_y.is_finite() {
        return Err(ToolError::new(
            ErrorCode::InvalidArgument,
            "Sample position must be finite",
        ));
    }
    if physical_x < 0.0
        || physical_y < 0.0
        || physical_x >= image.size[0] as f32
        || physical_y >= image.size[1] as f32
    {
        return Err(ToolError::new(
            ErrorCode::InvalidArgument,
            "Sample position is outside the captured image",
        )
        .with_details(json!({
            "position": position,
            "physical": [physical_x, physical_y],
            "image_size": image.size,
            "pixels_per_point": pixels_per_point,
        })));
    }
    let x = physical_x as usize;
    let y = physical_y as usize;
    let pixel = image.pixels[y * image.size[0] + x];
    let rgba = pixel.to_array();
    Ok(PixelSample {
        position,
        physical: [x, y],
        rgba,
        hex: format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            rgba[0], rgba[1], rgba[2], rgba[3]
        ),
    })
}

fn sample_widget_relative_pixels(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    widget: &WidgetRegistryEntry,
    viewport_name: Option<&str>,
    positions: &[Pos2],
) -> Result<Vec<PixelSample>, ToolError> {
    ensure_valid_pixels_per_point(pixels_per_point)?;
    let visible_rect = widget_visible_rect(widget, image, pixels_per_point);
    positions
        .iter()
        .map(|position| {
            let absolute = Pos2 {
                x: widget.rect.min.x + position.x,
                y: widget.rect.min.y + position.y,
            };
            if !position.x.is_finite()
                || !position.y.is_finite()
                || !point_in_rect_exclusive(absolute, widget.rect)
            {
                return Err(widget_sample_error(
                    ErrorCode::InvalidArgument,
                    "Sample position is outside the widget rect",
                    widget,
                    visible_rect,
                    viewport_name,
                    pixels_per_point,
                    json!({
                        "position": position,
                        "absolute_position": absolute,
                    }),
                ));
            }
            sample_widget_absolute_pixel(
                image,
                pixels_per_point,
                widget,
                visible_rect,
                viewport_name,
                *position,
                absolute,
            )
        })
        .collect()
}

fn sample_widget_grid(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    widget: &WidgetRegistryEntry,
    viewport_name: Option<&str>,
    nx: usize,
    ny: usize,
) -> Result<Vec<PixelSample>, ToolError> {
    ensure_valid_pixels_per_point(pixels_per_point)?;
    let visible_rect = widget_visible_rect(widget, image, pixels_per_point).ok_or_else(|| {
        widget_sample_error(
            ErrorCode::NotActionable,
            "Widget is not visible enough to sample",
            widget,
            None,
            viewport_name,
            pixels_per_point,
            json!({}),
        )
    })?;
    let sample_rect = inset_sampling_rect(visible_rect).ok_or_else(|| {
        widget_sample_error(
            ErrorCode::NotActionable,
            "Widget visible area is too small to sample",
            widget,
            Some(visible_rect),
            viewport_name,
            pixels_per_point,
            json!({}),
        )
    })?;
    let mut samples = Vec::with_capacity(nx * ny);
    for y in grid_axis(sample_rect.min.y, sample_rect.max.y, ny) {
        for x in grid_axis(sample_rect.min.x, sample_rect.max.x, nx) {
            let absolute = Pos2 { x, y };
            let relative = Pos2 {
                x: absolute.x - widget.rect.min.x,
                y: absolute.y - widget.rect.min.y,
            };
            samples.push(sample_widget_absolute_pixel(
                image,
                pixels_per_point,
                widget,
                Some(visible_rect),
                viewport_name,
                relative,
                absolute,
            )?);
        }
    }
    Ok(samples)
}

fn sample_widget_absolute_pixel(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    widget: &WidgetRegistryEntry,
    visible_rect: Option<Rect>,
    viewport_name: Option<&str>,
    relative_position: Pos2,
    absolute_position: Pos2,
) -> Result<PixelSample, ToolError> {
    let physical_x = (absolute_position.x * pixels_per_point).floor();
    let physical_y = (absolute_position.y * pixels_per_point).floor();
    if !physical_x.is_finite()
        || !physical_y.is_finite()
        || physical_x < 0.0
        || physical_y < 0.0
        || physical_x >= image.size[0] as f32
        || physical_y >= image.size[1] as f32
    {
        return Err(widget_sample_error(
            ErrorCode::InvalidArgument,
            "Sample position is outside the captured image",
            widget,
            visible_rect,
            viewport_name,
            pixels_per_point,
            json!({
                "position": relative_position,
                "absolute_position": absolute_position,
                "physical": [physical_x, physical_y],
                "image_size": image.size,
            }),
        ));
    }
    let x = physical_x as usize;
    let y = physical_y as usize;
    let pixel = image.pixels[y * image.size[0] + x];
    let rgba = pixel.to_array();
    Ok(PixelSample {
        position: relative_position,
        physical: [x, y],
        rgba,
        hex: format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            rgba[0], rgba[1], rgba[2], rgba[3]
        ),
    })
}

fn ensure_valid_pixels_per_point(pixels_per_point: f32) -> Result<(), ToolError> {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        return Ok(());
    }
    Err(ToolError::new(
        ErrorCode::Internal,
        "Invalid pixels_per_point for pixel sampling",
    ))
}

fn widget_visible_rect(
    widget: &WidgetRegistryEntry,
    image: &egui::ColorImage,
    pixels_per_point: f32,
) -> Option<Rect> {
    if !widget.visible {
        return None;
    }
    let mut rect = widget.rect;
    if let Some(layout) = &widget.layout {
        rect = rect_intersection(rect, layout.clip_rect)?;
    }
    rect_intersection(rect, image_logical_rect(image, pixels_per_point))
}

fn image_logical_rect(image: &egui::ColorImage, pixels_per_point: f32) -> Rect {
    Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 {
            x: image.size[0] as f32 / pixels_per_point,
            y: image.size[1] as f32 / pixels_per_point,
        },
    }
}

fn point_in_rect_exclusive(position: Pos2, rect: Rect) -> bool {
    position.x >= rect.min.x
        && position.y >= rect.min.y
        && position.x < rect.max.x
        && position.y < rect.max.y
}

fn inset_sampling_rect(rect: Rect) -> Option<Rect> {
    let width = rect.max.x - rect.min.x;
    let height = rect.max.y - rect.min.y;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let inset_x = WIDGET_SAMPLE_GRID_INSET.min(width * 0.25);
    let inset_y = WIDGET_SAMPLE_GRID_INSET.min(height * 0.25);
    let rect = Rect {
        min: Pos2 {
            x: rect.min.x + inset_x,
            y: rect.min.y + inset_y,
        },
        max: Pos2 {
            x: rect.max.x - inset_x,
            y: rect.max.y - inset_y,
        },
    };
    rect_is_positive(rect).then_some(rect)
}

fn rect_is_positive(rect: Rect) -> bool {
    (rect.max.x - rect.min.x) > 0.0 && (rect.max.y - rect.min.y) > 0.0
}

fn grid_axis(min: f32, max: f32, count: usize) -> Vec<f32> {
    if count == 1 {
        return vec![(min + max) * 0.5];
    }
    let step = (max - min) / (count - 1) as f32;
    (0..count).map(|index| min + step * index as f32).collect()
}

fn widget_sample_error(
    code: ErrorCode,
    message: &'static str,
    widget: &WidgetRegistryEntry,
    visible_rect: Option<Rect>,
    viewport_name: Option<&str>,
    pixels_per_point: f32,
    extra: Value,
) -> ToolError {
    let mut details = json!({
        "widget_id": widget.id,
        "viewport_id": widget.viewport_id,
        "viewport_name": viewport_name,
        "rect": widget.rect,
        "visible_rect": visible_rect,
        "pixels_per_point": pixels_per_point,
    });
    if let (Some(details), Value::Object(extra)) = (details.as_object_mut(), extra) {
        for (key, value) in extra {
            details.insert(key, value);
        }
    }
    ToolError::new(code, message).with_details(details)
}

fn resolve_viewport_id(
    inner: &Inner,
    viewport_id: Option<String>,
) -> Result<egui::ViewportId, ToolError> {
    inner
        .viewports
        .resolve_viewport_id(viewport_id)
        .map_err(Into::into)
}

fn resolve_widget(
    inner: &Inner,
    viewport_id: Option<&str>,
    target: &WidgetRef,
) -> Result<WidgetRegistryEntry, ToolError> {
    inner
        .widgets
        .resolve_widget(&inner.viewports, viewport_id, target)
        .map_err(Into::into)
}

fn resolve_wait_widget(
    inner: &Inner,
    viewport_id: Option<&str>,
    target: &WidgetRef,
) -> Result<WidgetRegistryEntry, ToolError> {
    match viewport_id {
        Some(viewport_id) => resolve_widget(inner, Some(viewport_id), target),
        None => inner
            .widgets
            .resolve_widget_global(&inner.viewports, target)
            .map_err(Into::into),
    }
}

fn merge_missing_widget_search(details: &mut Value, error: &ToolError) {
    if error.code() != ErrorCode::NotFound {
        return;
    }
    let Some(error_details) = error.details() else {
        return;
    };
    let Some(map) = details.as_object_mut() else {
        return;
    };
    if let Some(selectors) = error_details.get("selectors").cloned() {
        map.insert("selectors".to_string(), selectors);
    }
    if let Some(search) = error_details.get("search").cloned() {
        map.insert("search".to_string(), search);
    }
}

pub struct DevMcpServer {
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
}

pub fn collect_widget_list(
    inner: &Inner,
    viewport_id: Option<String>,
    include_invisible: Option<bool>,
    role: Option<WidgetRole>,
    id_prefix: Option<&str>,
    label: Option<&str>,
    label_contains: Option<&str>,
) -> ToolResult<Vec<WidgetRegistryEntry>> {
    ensure_automation_ready(inner)?;
    let viewport_id = resolve_viewport_id(inner, viewport_id)?;
    let mut widgets = inner.widgets.widget_list(viewport_id);
    let include_invisible = include_invisible.unwrap_or(false);
    if !include_invisible {
        widgets.retain(|entry| entry.visible);
    }
    if let Some(role) = role {
        widgets.retain(|entry| entry.role == role);
    }

    if let Some(prefix) = id_prefix {
        widgets.retain(|entry| entry.id.starts_with(prefix));
    }
    if let Some(label) = label {
        widgets.retain(|entry| entry.label.as_deref() == Some(label));
    }
    if let Some(needle) = label_contains {
        widgets.retain(|entry| {
            entry
                .label
                .as_deref()
                .is_some_and(|label| label.contains(needle))
        });
    }

    Ok(widgets)
}

/// Fraction of the widget that survives clipping, or 1.0 with no layout metadata.
fn widget_visible_fraction(widget: &WidgetRegistryEntry) -> f32 {
    widget
        .layout
        .as_ref()
        .map_or(1.0, |layout| layout.visible_fraction)
}

/// Whether a widget exists in a state that can accept an interaction.
///
/// This one predicate governs pointer admission, the visibility waits, and the
/// poll that `scroll_into_view` runs. Keeping them identical means a wait that
/// passes cannot be followed by a click that fails for the same reason.
pub fn interaction_ready(widget: &WidgetRegistryEntry) -> bool {
    widget.visible && widget_visible_fraction(widget) > 0.0
}

/// Remedy named by the invisible-interaction error and by its `hint` detail.
pub const INTERACTION_READY_HINT: &str =
    "call wait({ actionable = true }) after scroll_into_view(), or check clipping";

fn invisible_interaction_error(
    inner: &Inner,
    widget: &WidgetRegistryEntry,
    viewport_id: egui::ViewportId,
) -> Option<ToolError> {
    if interaction_ready(widget) {
        return None;
    }
    let viewport = viewport_snapshot_for(inner, viewport_id);
    let reason = if !widget.visible {
        "widget is not visible"
    } else {
        "widget visible_fraction is 0"
    };
    Some(
        ToolError::new(
            ErrorCode::NotActionable,
            format!(
                "Cannot interact with widget {:?}: {reason}; {INTERACTION_READY_HINT}",
                widget.id
            ),
        )
        .with_details(json!({
            "reason": "invisible_interaction",
            "hint": INTERACTION_READY_HINT,
            "widget": WidgetState::from(widget),
            "viewport": viewport.as_ref().map(viewport_snapshot_json).unwrap_or_else(|| {
                json!({
                    "id": viewport_id_to_string(viewport_id),
                })
            }),
            "layout": widget.layout.clone(),
        })),
    )
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
    /// Evaluate a Luau script with DevMCP helpers. Scripts are assumed to be strict.
    async fn script_eval(
        &self,
        script: String,
        timeout_ms: Option<u64>,
        options: Option<ScriptEvalOptions>,
    ) -> ToolResult<CallToolResult> {
        let timeout_ms = timeout_ms.unwrap_or(script::DEFAULT_SCRIPT_TIMEOUT_MS);
        let options = options.unwrap_or_default();
        let source_name = options
            .source_name
            .unwrap_or_else(|| "script.luau".to_string());
        let inner = Arc::clone(&self.inner);
        let runtime = Arc::clone(&self.runtime);
        let eval = script::run_script_eval(
            inner,
            runtime,
            script,
            timeout_ms,
            source_name,
            options.args,
        )
        .await;
        Ok(eval.to_tool_result())
    }

    #[cfg(test)]
    /// Return the checked-in Luau definitions for the full scripting API.
    async fn script_api(&self) -> ToolResult<CallToolResult> {
        Ok(CallToolResult::new().with_text_content(script_definitions()))
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering as AtomicOrdering},
        },
        time::Duration,
    };

    use eguidev::{
        DevMcp, FixtureCall, FixtureError, FixtureResponse, FixtureResult, FixtureSpec, FrameGuard,
    };
    use serde_json::{Value, json};
    use tmcp::schema::ContentBlock;
    use tokio::{task::yield_now, time::sleep};

    use super::*;
    use crate::{
        actions::InputAction,
        automation::types::LayoutIssueKind,
        fixtures::FixtureHandler,
        mcp::AppMcpServer,
        overlay::{OverlayDebugConfig, OverlayDebugMode},
        registry::{Inner, viewport_id_to_string},
        runtime::{Runtime, attach_for_tests},
        types::{
            Modifiers, Pos2, Rect, RoleState, Vec2, WidgetLayout, WidgetRange, WidgetRef,
            WidgetRegistryEntry, WidgetRole, WidgetRoleMeta, WidgetValue,
        },
        viewports::InputSnapshot,
        widget_registry::{WidgetMeta, record_widget},
    };

    /// Script deadline for tests that assert an inner wait, fixture, or
    /// completion. The deadline covers Luau compilation, which is slow and
    /// highly variable under parallel test load, so it must stay far above the
    /// inner timeout that the test actually exercises.
    const TEST_SCRIPT_DEADLINE_MS: u64 = 10_000;

    fn discard_output(output: egui::FullOutput) {
        output.drop_without_applying_deltas();
    }

    fn set_runtime_fixture_handler<F>(inner: &Inner, handler: F)
    where
        F: Fn(&FixtureCall) -> FixtureResult + Send + Sync + 'static,
    {
        inner
            .fixtures
            .set_handler(FixtureHandler::Runtime(Arc::new(handler)))
            .expect("fixture handler");
    }

    fn fixture_ok() -> FixtureResult {
        Ok(FixtureResponse::new())
    }

    fn apply_actions(inner: &Inner, raw_input: &mut egui::RawInput) {
        let viewport_id = raw_input.viewport_id;
        let actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
        let base_modifiers = raw_input
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                egui::Event::ModifiersChanged(modifiers) => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let mut current_modifiers = base_modifiers;
        let mut modifiers_changed = false;
        let mut force_focus = false;
        for action in &actions {
            if let InputAction::Key {
                pressed, modifiers, ..
            } = action
            {
                modifiers_changed = true;
                current_modifiers = if *pressed {
                    base_modifiers.plus((*modifiers).into())
                } else {
                    base_modifiers
                };
            }
            if matches!(
                action,
                InputAction::Key { .. } | InputAction::Text { .. } | InputAction::Paste { .. }
            ) {
                force_focus = true;
            }
        }
        if force_focus {
            raw_input.focused = true;
        }
        for action in actions {
            action.apply(raw_input);
        }
        if modifiers_changed {
            raw_input
                .events
                .push(egui::Event::ModifiersChanged(current_modifiers));
        }
    }

    fn widget_ref_id(id: &str) -> WidgetRef {
        WidgetRef {
            id: id.to_string(),
            viewport_id: None,
        }
    }

    fn make_entry(id: &str, native_id: u64, role: WidgetRole) -> WidgetRegistryEntry {
        make_entry_with_rect(
            id,
            native_id,
            role,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            None,
        )
    }

    fn make_generated_entry(native_id: u64, role: WidgetRole) -> WidgetRegistryEntry {
        let mut entry = make_entry(&format!("{native_id:x}"), native_id, role);
        entry.explicit_id = false;
        entry
    }

    fn make_entry_with_rect(
        id: &str,
        native_id: u64,
        role: WidgetRole,
        rect: Rect,
        parent_id: Option<&str>,
    ) -> WidgetRegistryEntry {
        WidgetRegistryEntry {
            id: id.to_string(),
            explicit_id: true,
            native_id,
            viewport_id: "root".to_string(),
            layer_id: "layer".to_string(),
            rect,
            interact_rect: rect,
            role,
            label: None,
            value: None,
            data: None,
            layout: None,
            role_state: None,
            parent_id: parent_id.map(str::to_string),
            enabled: true,
            visible: true,
            focused: false,
        }
    }

    fn make_scroll_entry(
        id: &str,
        native_id: u64,
        offset: Vec2,
        max_offset: Vec2,
    ) -> WidgetRegistryEntry {
        let mut entry = make_entry(id, native_id, WidgetRole::ScrollArea);
        entry.role_state = Some(RoleState::ScrollArea {
            offset,
            viewport_size: Vec2 { x: 100.0, y: 100.0 },
            content_size: Vec2 {
                x: 100.0 + max_offset.x,
                y: 100.0 + max_offset.y,
            },
        });
        entry
    }

    fn capture_test_frame(inner: &Arc<Inner>, ctx: &egui::Context) {
        inner.capture_context(ctx.viewport_id(), ctx);
        inner
            .viewports
            .capture_input_snapshot(ctx, inner.fixture_epoch(), inner.frame_count() + 1);
        inner.advance_frame();
        Runtime::ensure_for_inner(inner)
            .frame_notify()
            .notify_waiters();
    }

    fn drain_test_actions(inner: &Arc<Inner>, viewport_id: egui::ViewportId) {
        let actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
        if !actions.is_empty() {
            inner
                .last_action_frame
                .store(inner.frame_count(), AtomicOrdering::Relaxed);
        }
    }

    fn record_test_snapshot(inner: &Arc<Inner>, viewport_id: egui::ViewportId) {
        inner.viewports.record_input_snapshot(
            viewport_id,
            InputSnapshot {
                pixels_per_point: 1.0,
                pointer_pos: None,
            },
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );
        inner.advance_frame();
        Runtime::ensure_for_inner(inner)
            .frame_notify()
            .notify_waiters();
    }

    fn run_instrumented_test_frame(
        devmcp: &DevMcp,
        ctx: &egui::Context,
        viewport_id: egui::ViewportId,
        live_viewports: &[egui::ViewportId],
    ) {
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        for live_viewport in live_viewports {
            raw_input
                .viewports
                .insert(*live_viewport, Default::default());
        }
        discard_output(ctx.run_ui(raw_input, |ctx| {
            let _guard = FrameGuard::new(devmcp, ctx);
        }));
    }

    fn viewport_snapshot_with_inner_size(inner_size: Vec2) -> ViewportSnapshot {
        ViewportSnapshot {
            viewport_id: viewport_id_to_string(egui::ViewportId::ROOT),
            name: None,
            inner_size,
            outer_pos: None,
            outer_size: Some(inner_size),
            pixels_per_point: 1.0,
            focused: true,
            title: None,
            parent_viewport_id: None,
            minimized: Some(false),
            occluded: Some(false),
            os_minimized: None,
            os_occluded: None,
            maximized: Some(false),
            fullscreen: Some(false),
        }
    }

    #[test]
    fn native_capture_crop_uses_bottom_center_content_region() {
        let pixels = (0..16).map(egui::Color32::from_gray).collect::<Vec<_>>();
        let image = egui::ColorImage {
            size: [4, 4],
            source_size: egui::Vec2::new(4.0, 4.0),
            pixels,
        };
        let snapshot = viewport_snapshot_with_inner_size(Vec2 { x: 2.0, y: 2.0 });

        let cropped = crop_native_capture_to_viewport(image, &snapshot).expect("crop");

        assert_eq!(cropped.size, [2, 2]);
        assert_eq!(
            cropped.pixels,
            vec![
                egui::Color32::from_gray(9),
                egui::Color32::from_gray(10),
                egui::Color32::from_gray(13),
                egui::Color32::from_gray(14),
            ]
        );
    }

    #[test]
    fn native_capture_crop_rejects_smaller_capture() {
        let image = egui::ColorImage {
            size: [1, 2],
            source_size: egui::Vec2::new(1.0, 2.0),
            pixels: vec![egui::Color32::BLACK; 2],
        };
        let snapshot = viewport_snapshot_with_inner_size(Vec2 { x: 2.0, y: 2.0 });

        let error = crop_native_capture_to_viewport(image, &snapshot).expect_err("too small");

        assert!(error.contains("smaller than viewport content"));
    }

    #[test]
    fn root_screenshot_timeout_message_does_not_claim_child_fallback() {
        let message = screenshot_timeout_message(egui::ViewportId::ROOT, "fallback unavailable");

        assert!(message.contains("Native screenshot fallback is only available"));
        assert!(!message.contains("attempted for this child viewport"));
    }

    #[test]
    fn native_screenshot_fallback_waits_for_fresh_child_frame() {
        let child = egui::ViewportId::from_hash_of("child");

        assert!(!should_try_native_screenshot_fallback(child, 10, 10));
        assert_eq!(
            should_try_native_screenshot_fallback(child, 11, 10),
            cfg!(target_os = "macos")
        );
        assert!(!should_try_native_screenshot_fallback(
            egui::ViewportId::ROOT,
            11,
            10
        ));
    }

    fn parse_script_eval_json(result: &CallToolResult) -> Value {
        let content = result.content.first().expect("content");
        match content {
            ContentBlock::Text(text) => serde_json::from_str(&text.text).expect("script eval json"),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tools_list_contains_only_the_app_boundary() {
        use tmcp::{ServerHandler, schema::Cursor, testutils::TestServerContext};

        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let server = AppMcpServer::new(inner, runtime);
        let ctx = TestServerContext::new();
        let tools = server
            .list_tools(ctx.ctx(), None::<Cursor>)
            .await
            .expect("list tools")
            .tools;
        let mut names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["app_close", "script_api", "script_eval"]);
    }

    #[test]
    fn sample_color_image_converts_logical_points_and_preserves_alpha() {
        let image = egui::ColorImage::new(
            [2, 2],
            vec![
                egui::Color32::BLACK,
                egui::Color32::from_rgba_premultiplied(0x11, 0x22, 0x33, 0x44),
                egui::Color32::WHITE,
                egui::Color32::from_rgb(0xaa, 0xbb, 0xcc),
            ],
        );

        let samples =
            sample_color_image(&image, 2.0, &[Pos2 { x: 0.75, y: 0.25 }]).expect("sample");

        assert_eq!(samples[0].physical, [1, 0]);
        assert_eq!(samples[0].rgba, [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(samples[0].hex, "#11223344");
    }

    #[test]
    fn sample_color_image_rejects_out_of_bounds_positions() {
        let image = egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]);

        let error =
            sample_color_image(&image, 1.0, &[Pos2 { x: 1.0, y: 0.0 }]).expect_err("out of bounds");

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("outside"));
    }

    #[test]
    fn sample_widget_pixels_uses_relative_positions() {
        let mut pixels = vec![egui::Color32::BLACK; 100];
        pixels[4 * 10 + 3] = egui::Color32::from_rgb(0x33, 0x66, 0x99);
        let image = egui::ColorImage::new([10, 10], pixels);
        let widget = make_entry_with_rect(
            "canvas",
            1,
            WidgetRole::Image,
            Rect {
                min: Pos2 { x: 2.0, y: 3.0 },
                max: Pos2 { x: 6.0, y: 8.0 },
            },
            None,
        );

        let samples = sample_widget_relative_pixels(
            &image,
            1.0,
            &widget,
            Some("root"),
            &[Pos2 { x: 1.0, y: 1.0 }],
        )
        .expect("sample");

        assert_eq!(samples[0].position, Pos2 { x: 1.0, y: 1.0 });
        assert_eq!(samples[0].physical, [3, 4]);
        assert_eq!(samples[0].hex, "#336699ff");
    }

    #[test]
    fn sample_widget_pixels_reports_widget_bounds() {
        let image = egui::ColorImage::new([10, 10], vec![egui::Color32::BLACK; 100]);
        let widget = make_entry_with_rect(
            "canvas",
            1,
            WidgetRole::Image,
            Rect {
                min: Pos2 { x: 2.0, y: 3.0 },
                max: Pos2 { x: 6.0, y: 8.0 },
            },
            None,
        );

        let error = sample_widget_relative_pixels(
            &image,
            1.0,
            &widget,
            Some("root"),
            &[Pos2 { x: 4.0, y: 0.0 }],
        )
        .expect_err("out of bounds");

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert_eq!(error.details().expect("details")["widget_id"], "canvas");
        assert_eq!(error.details().expect("details")["viewport_name"], "root");
    }

    #[test]
    fn sample_widget_grid_uses_visible_rect_and_single_axis_centers() {
        let image = egui::ColorImage::new([20, 20], vec![egui::Color32::WHITE; 400]);
        let mut widget = make_entry_with_rect(
            "canvas",
            1,
            WidgetRole::Image,
            Rect {
                min: Pos2 { x: 2.0, y: 2.0 },
                max: Pos2 { x: 12.0, y: 12.0 },
            },
            None,
        );
        widget.layout = Some(WidgetLayout {
            desired_size: Vec2 { x: 10.0, y: 10.0 },
            actual_size: Vec2 { x: 10.0, y: 10.0 },
            clip_rect: Rect {
                min: Pos2 { x: 4.0, y: 5.0 },
                max: Pos2 { x: 10.0, y: 9.0 },
            },
            clipped: true,
            overflow: false,
            available_rect: widget.rect,
            visible_fraction: 0.25,
        });

        let samples = sample_widget_grid(&image, 1.0, &widget, Some("root"), 1, 2).expect("grid");

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].position.x, 5.0);
        assert_eq!(samples[0].position.y, 4.0);
        assert_eq!(samples[1].position.x, 5.0);
        assert_eq!(samples[1].position.y, 6.0);
    }

    #[test]
    fn sample_widget_grid_rejects_hidden_widgets() {
        let image = egui::ColorImage::new([10, 10], vec![egui::Color32::WHITE; 100]);
        let mut widget = make_entry("canvas", 1, WidgetRole::Image);
        widget.visible = false;

        let error = sample_widget_grid(&image, 1.0, &widget, Some("root"), 2, 2)
            .expect_err("hidden widget");

        assert_eq!(error.code(), ErrorCode::NotActionable);
    }

    #[tokio::test]
    async fn script_api_returns_checked_in_definitions() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server.script_api().await.expect("script_api");
        let content = result.content.first().expect("content");
        match content {
            ContentBlock::Text(text) => assert_eq!(text.text, script_definitions()),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn script_eval_returns_value_and_logs() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                "eguidev.log(\"hello\")\nreturn 1 + 1".to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"].as_i64(), Some(2));
        assert_eq!(json["logs"][0], "hello");
    }

    #[tokio::test]
    async fn script_eval_exposes_one_frozen_namespace_and_stable_references() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                r#"local first = eguidev.widget("missing")
local second = eguidev.widget("missing")
local widgets = eguidev.widgets()
local namespace = eguidev :: any
return {
    namespace_frozen = table.isfrozen(eguidev),
    args_frozen = table.isfrozen(eguidev.args),
    reference_frozen = table.isfrozen(first),
    collection_frozen = table.isfrozen(widgets),
    stable_identity = first == second,
    missing_state = first:state() == nil,
    hidden_capabilities_absent = namespace.query == nil
        and namespace.action == nil
        and namespace.wait_kernel == nil
        and namespace.capture_kernel == nil
        and namespace.fixture_kernel == nil
        and namespace.diagnostic_kernel == nil
        and namespace.record == nil,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(
            json["value"],
            json!({
                "namespace_frozen": true,
                "args_frozen": true,
                "reference_frozen": true,
                "collection_frozen": true,
                "stable_identity": true,
                "missing_state": true,
                "hidden_capabilities_absent": true,
            })
        );
    }

    #[tokio::test]
    async fn script_eval_rejects_removed_api_globals_before_execution() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval("return widget(\"missing\")".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "typecheck");
    }

    #[tokio::test]
    async fn script_eval_omitted_args_default_to_empty_table() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval("return next(eguidev.args) == nil".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(json["value"], true);
    }

    #[tokio::test]
    async fn script_eval_exposes_scalar_args() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                "return { name = eguidev.args.name, count = eguidev.args.count, ratio = eguidev.args.ratio, enabled = eguidev.args.enabled }"
                    .to_string(),
                None,
                Some(ScriptEvalOptions {
                    source_name: Some("args.luau".to_string()),
                    args: ScriptArgs::from([
                        ("name".to_string(), ScriptArgValue::String("Sky".to_string())),
                        ("count".to_string(), ScriptArgValue::Int(4)),
                        ("ratio".to_string(), ScriptArgValue::Float(1.5)),
                        ("enabled".to_string(), ScriptArgValue::Bool(true)),
                    ]),
                }),
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(json["value"]["name"], "Sky");
        assert_eq!(json["value"]["count"], 4);
        assert_eq!(json["value"]["ratio"], 1.5);
        assert_eq!(json["value"]["enabled"], true);
    }

    #[tokio::test]
    async fn script_eval_preserves_empty_tool_result_arrays() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"return eguidev.root:widgets({ id_prefix = "missing" })"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], json!([]));
    }

    #[tokio::test]
    async fn script_eval_root_widget_list_returns_widget_handles() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut status = make_entry("status", 1, WidgetRole::Label);
        status.label = Some("Ready state".to_string());
        inner.widgets.record_widget(viewport_id, status);
        let mut other = make_entry("other", 2, WidgetRole::Button);
        other.label = Some("Other state".to_string());
        inner.widgets.record_widget(viewport_id, other);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"local widgets = eguidev.root:widgets({ id_prefix = "status" })
local exact = eguidev.root:widgets({ label = "Ready state" })
local contains = eguidev.root:widgets({ label_contains = "state" })
local state = widgets[1]:state()
assert(state ~= nil)
return {
    count = #widgets,
    id = widgets[1].id,
    viewport = state.viewport_id,
    exact = #exact,
    contains = #contains,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(
            json["value"],
            json!({ "count": 1, "id": "status", "viewport": "root", "exact": 1, "contains": 2 })
        );
    }

    #[tokio::test]
    async fn script_eval_widget_global_finds_across_viewports() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let secondary = egui::ViewportId::from_hash_of("script.global.widget.secondary");
        let secondary_id = viewport_id_to_string(secondary);

        inner.viewports.remember_viewport_id(secondary);
        inner.widgets.clear_registry(secondary);
        let mut panel = make_entry("panel", 1, WidgetRole::Label);
        panel.viewport_id = secondary_id.clone();
        inner.widgets.record_widget(secondary, panel);
        inner.widgets.finalize_registry(secondary);

        let result = server
            .script_eval(
                r#"local found = eguidev.widget("panel")
local found_state = found:state()
local maybe_state = eguidev.widget("panel"):state()
local missing = eguidev.widget("missing"):state()
assert(found_state ~= nil and maybe_state ~= nil)
return {
    id = found.id,
    viewport = found_state.viewport_id,
    maybe_viewport = maybe_state.viewport_id,
    missing = missing == nil,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(
            json["value"],
            json!({ "id": "panel", "viewport": secondary_id, "maybe_viewport": secondary_id, "missing": true })
        );
    }

    #[tokio::test]
    async fn script_eval_scoped_widget_handle_keeps_its_viewport() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let secondary = egui::ViewportId::from_hash_of("script.scoped.widget.secondary");
        let secondary_id = viewport_id_to_string(secondary);

        inner.viewports.remember_viewport_id(secondary);
        inner.widgets.clear_registry(secondary);
        let mut panel = make_entry("panel", 1, WidgetRole::Label);
        panel.viewport_id = secondary_id.clone();
        inner.widgets.record_widget(secondary, panel);
        inner.widgets.finalize_registry(secondary);

        let result = server
            .script_eval(
                format!(
                    r#"local viewport = eguidev.viewport("{secondary_id}")
local found = viewport:widget("panel")
local condition: WidgetCondition = {{ visible = true }}
local state = found:wait(condition)
assert(state ~= nil)
return {{ id = found.id, viewport = state.viewport_id }}"#
                ),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:#}");
        assert_eq!(
            json["value"],
            json!({ "id": "panel", "viewport": secondary_id })
        );
    }

    #[tokio::test]
    async fn script_eval_widget_global_reports_ambiguous_matches() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let secondary = egui::ViewportId::from_hash_of("script.global.widget.ambiguous");
        let secondary_id = viewport_id_to_string(secondary);

        inner.viewports.remember_viewport_id(secondary);
        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_generated_entry(0x2a, WidgetRole::Label),
        );
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);
        inner.widgets.clear_registry(secondary);
        let mut panel = make_generated_entry(0x2a, WidgetRole::Label);
        panel.viewport_id = secondary_id;
        inner.widgets.record_widget(secondary, panel);
        inner.widgets.finalize_registry(secondary);

        let result = server
            .script_eval(
                r#"return eguidev.widget("2a"):state()"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "tool");
        assert_eq!(json["error"]["code"], "not_found");
        let candidates = json["error"]["details"]["candidates"]
            .as_array()
            .expect("candidates");
        assert_eq!(candidates.len(), 2);
    }

    #[tokio::test]
    async fn script_eval_widget_global_wait_is_not_root_scoped() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));

        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_entry("basic.submit", 1, WidgetRole::Button),
        );
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 5, poll_interval_ms = 1 })
eguidev.widget("basic.submt"):wait({ present = true })"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout", "{json:?}");
        assert!(json["error"]["details"]["observation"]["target_viewport_id"].is_null());
        let suggestions = json["error"]["details"]["search"]["suggestions"]
            .as_array()
            .expect("suggestions");
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.as_str() == Some("basic.submit"))
        );
    }

    #[tokio::test]
    async fn script_eval_built_in_expect_helpers_return_widget_state() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut status = make_entry("status", 1, WidgetRole::Label);
        status.label = Some("Ready".to_string());
        inner.widgets.record_widget(viewport_id, status);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"local state = eguidev.widget("status"):expect({ label = "Ready", visible = true })
eguidev.widget("missing"):expect({ present = false })
assert(state ~= nil)
return state.label"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], "Ready");
    }

    #[tokio::test]
    async fn script_eval_capture_diff_reports_changed_widgets() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        record_test_snapshot(&inner, viewport_id);
        inner.widgets.clear_registry(viewport_id);
        let mut before = make_entry("status", 1, WidgetRole::Label);
        before.label = Some("Before".to_string());
        inner.widgets.record_widget(viewport_id, before);
        inner.widgets.finalize_registry(viewport_id);

        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("mutate", "Mutate widget.").ready("status"),
        ]);
        let inner_for_fixture = Arc::clone(&inner);
        set_runtime_fixture_handler(&inner, move |_call| {
            inner_for_fixture.widgets.clear_registry(viewport_id);
            let mut after = make_entry_with_rect(
                "status",
                1,
                WidgetRole::Label,
                Rect {
                    min: Pos2 { x: 2.0, y: 0.0 },
                    max: Pos2 { x: 12.0, y: 10.0 },
                },
                None,
            );
            after.label = Some("After".to_string());
            inner_for_fixture.widgets.record_widget(viewport_id, after);
            inner_for_fixture.widgets.finalize_registry(viewport_id);
            fixture_ok()
        });

        let result = server
            .script_eval(
                r#"local before = eguidev.capture()
eguidev.fixture("mutate", nil, { wait = false })
local diff = before:diff({ id_prefix = "status", move_epsilon = 0.1 })
return diff"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["changes"][0]["id"], "status");
        assert_eq!(json["value"]["changes"][0]["change"], "changed");
        let fields = json["value"]["changes"][0]["fields"]
            .as_array()
            .expect("fields");
        assert!(fields.iter().any(|field| field == "rect"));
        assert!(fields.iter().any(|field| field == "label"));
        assert_eq!(json["value"]["viewports_added"], json!([]));
        assert_eq!(json["value"]["viewports_removed"], json!([]));
    }

    #[tokio::test]
    async fn script_eval_capture_diff_filters_invisible_widgets() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        record_test_snapshot(&inner, viewport_id);
        inner.widgets.clear_registry(viewport_id);
        let mut before = make_entry("hidden.status", 1, WidgetRole::Label);
        before.label = Some("Before".to_string());
        before.visible = false;
        inner.widgets.record_widget(viewport_id, before);
        inner.widgets.finalize_registry(viewport_id);

        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("mutate-hidden", "Mutate hidden widget.").ready("hidden.status"),
        ]);
        let inner_for_fixture = Arc::clone(&inner);
        set_runtime_fixture_handler(&inner, move |_call| {
            inner_for_fixture.widgets.clear_registry(viewport_id);
            let mut after = make_entry("hidden.status", 1, WidgetRole::Label);
            after.label = Some("After".to_string());
            after.visible = false;
            inner_for_fixture.widgets.record_widget(viewport_id, after);
            inner_for_fixture.widgets.finalize_registry(viewport_id);
            fixture_ok()
        });

        let result = server
            .script_eval(
                r#"local before = eguidev.capture()
eguidev.fixture("mutate-hidden", nil, { wait = false })
return {
    visible_only = before:diff({ id_prefix = "hidden.status" }),
    include_hidden = before:diff({ id_prefix = "hidden.status", include_invisible = true }),
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["visible_only"]["changes"], json!([]));
        assert_eq!(
            json["value"]["include_hidden"]["changes"][0]["id"],
            "hidden.status"
        );
        assert_eq!(
            json["value"]["include_hidden"]["changes"][0]["change"],
            "changed"
        );
        let fields = json["value"]["include_hidden"]["changes"][0]["fields"]
            .as_array()
            .expect("fields");
        assert!(fields.iter().any(|field| field == "label"));
    }

    #[tokio::test]
    async fn script_eval_root_viewport_state_returns_current_snapshot() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"local state = eguidev.root:state()
assert(state ~= nil)
return { frame = state.frame_count, pixels_per_point = state.pixels_per_point }"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], json!({ "frame": 1, "pixels_per_point": 1 }));
    }

    #[tokio::test]
    async fn script_eval_viewport_finds_viewport_by_title() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let secondary = egui::ViewportId::from_hash_of("secondary.lookup");

        for _ in 0..2 {
            let mut raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            raw_input.viewports.insert(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    title: Some("Root Lookup".to_string()),
                    ..Default::default()
                },
            );
            raw_input.viewports.insert(
                secondary,
                egui::ViewportInfo {
                    title: Some("Secondary Lookup".to_string()),
                    ..Default::default()
                },
            );
            discard_output(ctx.run_ui(raw_input, |_| {}));
        }
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"local exact = eguidev.viewports({ title = "Secondary Lookup" })[1]
local exact_wins = eguidev.viewports({ title = "Secondary Lookup" })[1]
local contains = eguidev.viewports({ title_contains = "Secondary" })[1]
local missing = eguidev.viewports({ title = "Missing" })
return {
    exact = exact ~= nil and exact.id or nil,
    exact_wins = exact_wins ~= nil and exact_wins.id or nil,
    contains = contains ~= nil and contains.id or nil,
    missing = #missing == 0,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        let secondary_id = viewport_id_to_string(secondary);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["exact"], secondary_id);
        assert_eq!(json["value"]["exact_wins"], secondary_id);
        assert_eq!(json["value"]["contains"], secondary_id);
        assert_eq!(json["value"]["missing"], true);
    }

    #[tokio::test]
    async fn script_eval_viewport_finds_viewport_by_name_and_focus() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let secondary = egui::ViewportId::from_hash_of("secondary.named.lookup");

        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                title: Some("Root Lookup".to_string()),
                focused: Some(false),
                ..Default::default()
            },
        );
        raw_input.viewports.insert(
            secondary,
            egui::ViewportInfo {
                title: Some("Secondary Lookup".to_string()),
                focused: Some(true),
                ..Default::default()
            },
        );
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner
            .viewports
            .name_viewport(secondary, "secondary".to_string());
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"local named = eguidev.wait_viewport({ name = "secondary" })
local focused = eguidev.viewports({ focused = true })[1]
local state = named ~= nil and named:state() or nil
return {
    named = named ~= nil and named.id or nil,
    focused = focused ~= nil and focused.id or nil,
    name = state ~= nil and state.name or nil,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        let secondary_id = viewport_id_to_string(secondary);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["named"], secondary_id);
        assert_eq!(json["value"]["focused"], secondary_id);
        assert_eq!(json["value"]["name"], "secondary");
    }

    #[tokio::test]
    async fn script_eval_viewport_errors_on_ambiguous_title() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let first = egui::ViewportId::from_hash_of("duplicate.lookup.first");
        let second = egui::ViewportId::from_hash_of("duplicate.lookup.second");

        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        for viewport_id in [first, second] {
            raw_input.viewports.insert(
                viewport_id,
                egui::ViewportInfo {
                    title: Some("Duplicate Lookup".to_string()),
                    ..Default::default()
                },
            );
        }
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"return #eguidev.viewports({ title = "Duplicate Lookup" })"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);

        assert_eq!(json["success"], true);
        assert_eq!(json["value"], 2);
    }

    #[tokio::test]
    async fn script_eval_wait_viewport_reports_ambiguous_and_absent_filters() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let first = egui::ViewportId::from_hash_of("duplicate.name.first");
        let second = egui::ViewportId::from_hash_of("duplicate.name.second");

        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        raw_input.viewports.insert(
            first,
            egui::ViewportInfo {
                title: Some("Duplicate First".to_string()),
                ..Default::default()
            },
        );
        raw_input.viewports.insert(
            second,
            egui::ViewportInfo {
                title: Some("Duplicate Second".to_string()),
                ..Default::default()
            },
        );
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"local ambiguousOk, ambiguousError = pcall(function()
    return eguidev.wait_viewport(
        { title_contains = "Duplicate" },
        { timeout_ms = 20, poll_interval_ms = 1 }
    )
end)
local absentOk, absentError = pcall(function()
    return eguidev.wait_viewport(
        { name = "missing" },
        { timeout_ms = 20, poll_interval_ms = 1 }
    )
end)
local ambiguous: any = ambiguousError
local absent: any = absentError
return {
    ambiguous_ok = ambiguousOk,
    ambiguous_code = ambiguous.code,
    ambiguous_details = ambiguous.details,
    absent_ok = absentOk,
    absent_code = absent.code,
    absent_details = absent.details,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);

        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(json["value"]["ambiguous_ok"], false, "{json:?}");
        assert_eq!(json["value"]["ambiguous_code"], "not_found", "{json:?}");
        assert_eq!(
            json["value"]["ambiguous_details"]["reason"], "ambiguous",
            "{json:?}"
        );
        assert_eq!(
            json["value"]["ambiguous_details"]["candidates"]
                .as_array()
                .map(Vec::len),
            Some(2),
            "{json:?}"
        );
        assert_eq!(json["value"]["absent_ok"], false, "{json:?}");
        assert_eq!(json["value"]["absent_code"], "not_found", "{json:?}");
        assert_eq!(
            json["value"]["absent_details"]["reason"], "absent",
            "{json:?}"
        );
    }

    #[tokio::test]
    async fn script_eval_widget_get_state_reads_current_snapshot() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Button);
        entry.label = Some("Ready".to_string());
        entry.data = Some(json!({"kind": "status", "ready": true}));
        entry.focused = true;
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"local state = eguidev.widget("status"):state()
assert(state ~= nil)
return {
    role = state.role,
    label = state.label,
    kind = state.data.kind,
    ready = state.data.ready,
    focused = state.focused,
}"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(
            json["value"],
            json!({
                "role": "button",
                "label": "Ready",
                "kind": "status",
                "ready": true,
                "focused": true
            })
        );
    }

    #[tokio::test]
    async fn script_eval_widget_state_matches_luau_type_contract() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut labeled = make_entry("labeled", 1, WidgetRole::Button);
        labeled.label = Some("Ready".to_string());
        labeled.value = Some(WidgetValue::Bool(true));
        inner.widgets.record_widget(viewport_id, labeled);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("container", 2, WidgetRole::Unknown));
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"
local seen = {}
for _, widget in ipairs(eguidev.root:widgets()) do
    local state = widget:state()
    assert(state ~= nil)
    assert(type(state.rect) == "table", "rect must be table")
    assert(type(state.interact_rect) == "table", "interact_rect must be table")
    assert(type(state.role) == "string", "role must be string")
    assert(type(state.value_text) == "string", "value_text must be string")
    assert(type(state.enabled) == "boolean", "enabled must be boolean")
    assert(type(state.visible) == "boolean", "visible must be boolean")
    assert(type(state.focused) == "boolean", "focused must be boolean")
    assert(state.label == nil or type(state.label) == "string", "label must be nil|string")
    assert(state.value == nil or type(state.value) ~= "userdata", "value must not be userdata")
    assert(state.data == nil or type(state.data) == "table", "data must be nil|table")
    assert(state.layout == nil or type(state.layout) == "table", "layout must be nil|table")
    assert(state.scroll_state == nil or type(state.scroll_state) == "table", "scroll_state")
    assert(state.range == nil or type(state.range) == "table", "range")
    assert(state.options == nil or type(state.options) == "table", "options")
    assert(state.selected == nil or type(state.selected) == "boolean", "selected")
    assert(state.indeterminate == nil or type(state.indeterminate) == "boolean", "indeterminate")
    assert(state.multiline == nil or type(state.multiline) == "boolean", "multiline")
    assert(state.password == nil or type(state.password) == "boolean", "password")
    seen[widget.id] = { label = type(state.label), value_text = state.value_text }
end
return seen
"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["labeled"]["label"], "string");
        assert_eq!(json["value"]["labeled"]["value_text"], "true");
        assert_eq!(json["value"]["container"]["label"], "nil");
        assert_eq!(json["value"]["container"]["value_text"], "");
    }

    #[tokio::test]
    async fn script_eval_widget_state_preserves_nested_empty_arrays() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("choice", 1, WidgetRole::ComboBox);
        entry.role_state = Some(RoleState::ComboBox {
            options: Vec::new(),
        });
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"return eguidev.widget("choice"):state()"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);

        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["options"], json!([]));
    }

    #[tokio::test]
    async fn script_eval_preserves_empty_tool_result_arrays_in_multi_returns() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"local viewport = eguidev.root
return viewport:widgets({ id_prefix = "missing" }),
    viewport:widgets({ id_prefix = "also_missing" })"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], json!([[], []]));
    }

    #[tokio::test]
    async fn script_eval_characterizes_integral_float_collapse() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                "return { arg = eguidev.args.unit, literal = 1.0 }".to_string(),
                None,
                Some(ScriptEvalOptions {
                    source_name: Some("integral-float.luau".to_string()),
                    args: ScriptArgs::from([("unit".to_string(), ScriptArgValue::Float(1.0))]),
                }),
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["arg"].as_i64(), Some(1));
        assert_eq!(json["value"]["literal"].as_i64(), Some(1));
    }

    #[tokio::test]
    async fn script_eval_characterizes_nil_options_as_absent() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"return eguidev.root:widgets(nil)"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], json!([]));
    }

    #[tokio::test]
    async fn script_eval_characterizes_nil_inside_returned_arrays() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval("return { 1, nil, 3 }".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], json!([1, null, 3]));
    }

    #[tokio::test]
    async fn script_eval_reports_parse_errors() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval("local x =".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "parse");
    }

    #[tokio::test]
    async fn script_eval_pcall_catches_host_tool_errors() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"
                    local ok, err = pcall(function()
                        return eguidev.widget("missing"):expect({ present = true })
                    end)
                    local caught = err :: any
                    return {
                        ok = ok,
                        err_type = type(caught),
                        err = caught.message,
                        code = caught.code,
                        rendered = tostring(caught),
                        frozen = table.isfrozen(caught),
                    }
                "#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(json["value"]["ok"], false);
        assert_eq!(json["value"]["err_type"], "table");
        assert_eq!(json["value"]["code"], "expectation_failed", "{json:?}");
        assert_eq!(json["value"]["frozen"], true);
        assert!(
            json["value"]["rendered"]
                .as_str()
                .is_some_and(|message| message.starts_with("expectation_failed: ")),
            "{json:?}"
        );
        assert!(
            json["value"]["err"]
                .as_str()
                .is_some_and(|message| message.contains("missing")),
            "{json:?}"
        );
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_closure_matches() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.label = Some("Ready".to_string());
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 50 }) return eguidev.widget("status"):wait(function(w) return w ~= nil and w.label == "Ready" end)"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["label"], "Ready");
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_closure_compares_integer_values() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("choice", 1, WidgetRole::ComboBox);
        entry.value = Some(WidgetValue::Int(2));
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 50 }) return eguidev.widget("choice"):wait(function(widget) return widget ~= nil and widget.value == 2 end)"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(json["value"]["value"], 2);
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_absent() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 30 }) eguidev.widget("missing"):wait({ present = false }) return true"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert_eq!(json["value"], true);
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_visible_matches_existing_widget() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.label = Some("Ready".to_string());
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 50 }) return eguidev.widget("status"):wait({ visible = true })"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["visible"], true);
        assert_eq!(json["value"]["label"], "Ready");
    }

    #[tokio::test]
    async fn script_eval_widget_wait_for_visible_waits_until_visible() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.label = Some("Ready".to_string());
        entry.visible = false;
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let inner_for_update = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            inner_for_update.widgets.clear_registry(viewport_id);
            let mut entry = make_entry("status", 1, WidgetRole::Label);
            entry.label = Some("Ready".to_string());
            entry.visible = true;
            inner_for_update.widgets.record_widget(viewport_id, entry);
            inner_for_update.widgets.finalize_registry(viewport_id);
        });

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 100, poll_interval_ms = 1 })
return eguidev.widget("status"):wait({ visible = true })"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["visible"], true);
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_visible_waits_for_widget_to_appear() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let inner_for_update = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            inner_for_update.widgets.clear_registry(viewport_id);
            let mut entry = make_entry("status", 1, WidgetRole::Label);
            entry.label = Some("Ready".to_string());
            inner_for_update.widgets.record_widget(viewport_id, entry);
            inner_for_update.widgets.finalize_registry(viewport_id);
        });

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 100, poll_interval_ms = 1 }) return eguidev.widget("status"):wait({ visible = true })"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["visible"], true);
        assert_eq!(json["value"]["label"], "Ready");
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_visible_times_out_when_hidden() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.visible = false;
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 30, poll_interval_ms = 1 }) return eguidev.widget("status"):wait({ visible = true })"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert_eq!(json["error"]["details"]["kind"], "widget");
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_closure_times_out() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let entry = make_entry("status", 1, WidgetRole::Label);
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 30 }) return eguidev.widget("status"):wait(function(w) return w ~= nil and w.label == "Never" end)"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert_eq!(json["error"]["details"]["kind"], "widget");
    }

    #[tokio::test]
    async fn script_eval_wait_for_widget_closure_respects_script_timeout() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;
        let entry = make_entry("status", 1, WidgetRole::Label);

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.configure({ timeout_ms = 5000, poll_interval_ms = 250 }) return eguidev.widget("status"):wait(function(w) return w ~= nil and w.label == "Never" end)"#
                    .to_string(),
                Some(50),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert!(
            json["timing"]["total_ms"]
                .as_u64()
                .is_some_and(|elapsed_ms| elapsed_ms < 500),
            "{json:?}"
        );
    }

    #[tokio::test]
    async fn script_eval_pcall_does_not_catch_script_timeout() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;
        let entry = make_entry("status", 1, WidgetRole::Label);

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"
                    local ok = pcall(function()
                        while true do end
                    end)
                    return ok
                "#
                .to_string(),
                Some(50),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false, "{json:?}");
        assert_eq!(json["error"]["type"], "timeout");
        assert!(
            json["timing"]["total_ms"]
                .as_u64()
                .is_some_and(|elapsed_ms| elapsed_ms < 500),
            "{json:?}"
        );
    }

    #[tokio::test]
    async fn script_eval_drag_relative_accepts_positional_from() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "slider",
                1,
                WidgetRole::Slider,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                None,
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"eguidev.widget("slider"):drag_relative(
                    { x = 0.8, y = 0.5 },
                    { from = { x = 0.2, y = 0.5 }, settle = false }
                )"#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);

        let actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
        let Some(InputAction::PointerMove { pos }) = actions.first() else {
            panic!("expected initial pointer move from drag_relative");
        };
        assert!((pos.x - 2.0).abs() < f32::EPSILON);
        assert!((pos.y - 5.0).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn script_eval_widget_at_point_accepts_boolean_all_layers() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "bottom",
                1,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                None,
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "top",
                2,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                None,
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"local widgets = eguidev.root:widgets_at({ x = 5, y = 5 })
                return { count = #widgets, first = widgets[1].id }"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"]["count"], 2);
        assert_eq!(json["value"]["first"], "top");
    }

    #[tokio::test]
    async fn script_eval_widget_show_debug_overlay_preserves_viewport_scope() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let secondary = egui::ViewportId::from_hash_of("secondary");
        let viewport_selector = viewport_id_to_string(secondary);
        let ctx = egui::Context::default();

        {
            let mut raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            raw_input
                .viewports
                .insert(egui::ViewportId::ROOT, Default::default());
            raw_input.viewports.insert(secondary, Default::default());
            discard_output(ctx.run_ui(raw_input, |_| {}));
        }
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );

        inner.widgets.clear_registry(secondary);
        let mut entry = make_entry("overlay", 1, WidgetRole::Button);
        entry.viewport_id = viewport_id_to_string(secondary);
        inner.widgets.record_widget(secondary, entry);
        inner.widgets.finalize_registry(secondary);

        let result = server
            .script_eval(
                format!(
                    "return eguidev.widget(\"overlay\"):show_debug_overlay() -- {viewport_selector}"
                ),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);

        let config = inner.overlays.overlay_debug_config();
        let scope = config.scope.expect("widget-scoped overlay");
        assert_eq!(scope.id, "overlay");
        assert_eq!(
            scope.viewport_id.as_deref(),
            Some(viewport_selector.as_str())
        );
    }

    #[tokio::test]
    async fn script_eval_action_settle_rejects_invalid_types() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                r#"eguidev.root:paste("hello", { settle = "fast" })"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "typecheck");
    }

    #[tokio::test]
    async fn script_eval_click_settle_targets_widget_viewport() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let secondary = egui::ViewportId::from_hash_of("secondary");

        for _ in 0..2 {
            let mut raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            raw_input
                .viewports
                .insert(egui::ViewportId::ROOT, Default::default());
            raw_input.viewports.insert(secondary, Default::default());
            discard_output(ctx.run_ui(raw_input, |_| {}));
        }
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );

        inner.widgets.clear_registry(secondary);
        let mut entry = make_entry("status", 1, WidgetRole::Button);
        entry.viewport_id = viewport_id_to_string(secondary);
        inner.widgets.record_widget(secondary, entry);
        inner.widgets.finalize_registry(secondary);

        let viewport_selector = viewport_id_to_string(secondary);
        let result = server
            .script_eval(
                format!(
                    "eguidev.configure({{ timeout_ms = 10, poll_interval_ms = 1 }})\nreturn eguidev.widget(\"status\"):click() -- {viewport_selector}"
                ),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout", "{json:?}");
        assert_eq!(json["error"]["details"]["kind"], "settle", "{json:?}");
        let observation = &json["error"]["details"]["observation"];
        assert_eq!(observation["target_viewport_id"], viewport_selector);
        assert_eq!(observation["action_queue"]["end_queued_actions"], 3);
        assert_eq!(observation["action_queue"]["end_drained_actions"], 0);
        assert_eq!(observation["action_queue"]["pending_actions"], 3);
        assert!(
            observation["diagnosis"]
                .as_str()
                .expect("diagnosis")
                .contains("No target viewport frames were observed")
        );
    }

    #[tokio::test]
    async fn script_eval_reports_assertion_failures() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                "eguidev.widget(\"missing\"):expect({ present = true }, { timeout_ms = 10, poll_interval_ms = 1 })".to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "eguidev");
        assert_eq!(json["error"]["code"], "expectation_failed");
        assert_eq!(json["assertions"][0]["passed"], false);
    }

    #[tokio::test]
    async fn script_eval_reports_assert_widget_exists() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
        inner.widgets.finalize_registry(viewport_id);

        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                "eguidev.widget(\"status\"):expect({ present = true })".to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["assertions"][0]["passed"], true);
        assert!(
            json["assertions"][0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("expectation"))
        );
    }

    #[tokio::test]
    async fn fixture_apply_applies_handler_without_waiting_for_a_new_frame() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("test_fixture", "Test fixture.").ready("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );
        let server = DevMcpServer::new(Arc::clone(&inner));
        server
            .fixture_apply("test_fixture".to_string(), None)
            .await
            .expect("fixture_apply result");
    }

    #[tokio::test]
    async fn script_fixture_waits_for_preconditions_before_handler() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("delayed", "Delayed fixture.")
                .precondition_value("ready", WidgetValue::Bool(true))
                .ready("status"),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));

        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        let mut ready = make_entry("ready", 1, WidgetRole::Label);
        ready.value = Some(WidgetValue::Bool(false));
        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(viewport_id, ready);
        inner.widgets.finalize_registry(viewport_id);
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(viewport_id, &ctx);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );

        let inner_for_fixture = Arc::clone(&inner);
        let ctx_for_fixture = ctx.clone();
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            let ready = resolve_widget(&inner_for_fixture, None, &widget_ref_id("ready"))
                .expect("precondition widget");
            assert_eq!(ready.value, Some(WidgetValue::Bool(true)));
            handler_called_for_fixture.store(true, AtomicOrdering::Release);
            inner_for_fixture.widgets.clear_registry(viewport_id);
            inner_for_fixture.widgets.record_widget(viewport_id, ready);
            inner_for_fixture
                .widgets
                .record_widget(viewport_id, make_entry("status", 2, WidgetRole::Label));
            inner_for_fixture.widgets.finalize_registry(viewport_id);
            let inner_for_ready = Arc::clone(&inner_for_fixture);
            let ctx_for_ready = ctx_for_fixture.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(10)).await;
                let raw_input = egui::RawInput {
                    viewport_id,
                    ..Default::default()
                };
                discard_output(ctx_for_ready.run_ui(raw_input, |_| {}));
                capture_test_frame(&inner_for_ready, &ctx_for_ready);
            });
            fixture_ok()
        });

        let inner_for_update = Arc::clone(&inner);
        let ctx_for_update = ctx.clone();
        tokio::spawn(async move {
            yield_now().await;
            let mut ready = make_entry("ready", 1, WidgetRole::Label);
            ready.value = Some(WidgetValue::Bool(true));
            inner_for_update.widgets.clear_registry(viewport_id);
            inner_for_update.widgets.record_widget(viewport_id, ready);
            inner_for_update.widgets.finalize_registry(viewport_id);
            let raw_input = egui::RawInput {
                viewport_id,
                ..Default::default()
            };
            discard_output(ctx_for_update.run_ui(raw_input, |_| {}));
            inner_for_update.capture_context(viewport_id, &ctx_for_update);
            capture_test_frame(&inner_for_update, &ctx_for_update);
        });

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                "return eguidev.fixture(\"delayed\", nil, { timeout_ms = 2000, poll_interval_ms = 1 })"
                    .to_string(),
                Some(3_000),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        assert!(handler_called.load(AtomicOrdering::Acquire));
    }

    #[tokio::test]
    async fn fixture_precondition_timeout_does_not_apply_handler() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("blocked", "Blocked fixture.")
                .precondition_value("ready", WidgetValue::Bool(true))
                .ready("status"),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Release);
            fixture_ok()
        });

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                "return eguidev.fixture(\"blocked\", nil, { timeout_ms = 20, poll_interval_ms = 1 })"
                    .to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false, "{json:?}");
        assert_eq!(json["error"]["type"], "eguidev", "{json:?}");
        assert_eq!(json["error"]["code"], "fixture_failed", "{json:?}");
        assert_eq!(
            json["error"]["details"]["details"]["reason"], "precondition_unmet",
            "{json:?}"
        );
        assert_eq!(
            json["error"]["details"]["details"]["widget"], "ready",
            "{json:?}"
        );
        assert!(!handler_called.load(AtomicOrdering::Relaxed));
    }

    #[tokio::test]
    async fn fixture_returns_error_when_handler_fails() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("broken", "Broken fixture.").ready("status"),
        ]);
        inner
            .fixtures
            .set_handler(FixtureHandler::Runtime(Arc::new(|_call| {
                Err(FixtureError::new("fixture_failed", "fixture failed"))
            })))
            .expect("fixture handler");
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture_apply("broken".to_string(), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fixture_returns_error_when_no_handler_registered() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("no_handler", "No handler fixture.").ready("status"),
        ]);
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture_apply("no_handler".to_string(), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn script_eval_fixture_wait_false_succeeds_without_a_new_frame() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("slow", "Slow fixture.").ready("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                "eguidev.fixture(\"slow\", nil, { wait = false })".to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
    }

    #[tokio::test]
    async fn script_eval_returns_sorted_fixtures() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("zeta", "Last fixture.").ready("status"),
            FixtureSpec::new("alpha", "First fixture.").ready("status"),
        ]);
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                r#"local catalog = eguidev.fixtures()
return { first = catalog[1].name, count = #catalog }"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], json!({ "first": "alpha", "count": 2 }));
    }

    #[tokio::test]
    async fn script_eval_wait_for_frames_returns_frame_count() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval("return eguidev.wait_frames(0)".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], 0);
    }

    #[tokio::test]
    async fn script_eval_fixture_timeout_reports_fresh_frame_diagnostics() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("stale", "Stale fixture.").ready("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        let entry = make_entry("status", 1, WidgetRole::Label);
        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner, &ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                "eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 }) eguidev.fixture(\"stale\")"
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert_eq!(json["error"]["details"]["kind"], "frames");
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("timeout message")
                .contains("Timed out waiting for 1 frame"),
            "timeout should name the fresh-frame requirement"
        );
        assert!(
            json["error"]["details"]["observation"]["diagnosis"]
                .as_str()
                .expect("observation diagnosis")
                .contains("No target viewport frames were observed"),
            "timeout details should explain the missing fresh capture"
        );
    }

    #[tokio::test]
    async fn fixture_timeout_reports_zero_frame_observation_once() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("blocked", "Blocked fixture.").ready("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());
        let server = DevMcpServer::new(Arc::clone(&inner));

        let result = server
            .script_eval(
                "eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 }) eguidev.fixture(\"blocked\")"
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        let message = json["error"]["message"].as_str().expect("timeout message");
        assert_eq!(
            message
                .matches("Ensure the app wraps rendered frames")
                .count(),
            1,
            "guidance sentence should appear once"
        );
        let details = &json["error"]["details"];
        assert_eq!(details["observation"]["target_viewport_id"], "root");
        assert_eq!(details["observation"]["frames_observed"], 0);
        assert_eq!(details["observation"]["stalled"], true);
        assert_eq!(
            details["observation"]["diagnosis"],
            "No target viewport frames were observed while waiting."
        );
    }

    #[test]
    fn frame_started_before_fixture_epoch_does_not_satisfy_fixture_capture() {
        let inner = Arc::new(Inner::new());
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));

        inner.begin_frame(viewport_id);
        let fixture_epoch = inner.begin_fixture_epoch();
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("menu.item", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner
                .finish_frame_fixture_epoch(viewport_id)
                .expect("frame start epoch"),
            inner.frame_count() + 1,
        );

        let capture = inner
            .viewports
            .capture_snapshot(viewport_id)
            .expect("capture snapshot");
        assert!(
            capture.fixture_epoch < fixture_epoch,
            "mid-frame captures must not be stamped with the new fixture epoch"
        );
    }

    #[tokio::test]
    async fn fixture_waits_for_multiviewport_anchor_capture() {
        let inner = Arc::new(Inner::new());
        let secondary = egui::ViewportId::from_hash_of("fixture.secondary");
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("multi", "Multi viewport fixture.").ready_in("status", secondary),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Release);
            fixture_ok()
        });

        let root_ctx = egui::Context::default();
        let mut root_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        root_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        root_input.viewports.insert(secondary, Default::default());
        discard_output(root_ctx.run_ui(root_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &root_ctx);
        inner.viewports.update_viewports(&root_ctx);
        capture_test_frame(&inner, &root_ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_for_capture = Arc::clone(&inner);
        let root_ctx_for_capture = root_ctx.clone();
        let handler_called_for_capture = Arc::clone(&handler_called);
        tokio::spawn(async move {
            while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
                sleep(Duration::from_millis(1)).await;
            }
            sleep(Duration::from_millis(10)).await;
            capture_test_frame(&inner_for_capture, &root_ctx_for_capture);
            inner_for_capture.widgets.clear_registry(secondary);
            let mut entry = make_entry("status", 1, WidgetRole::Label);
            entry.viewport_id = viewport_id_to_string(secondary);
            inner_for_capture.widgets.record_widget(secondary, entry);
            inner_for_capture.widgets.finalize_registry(secondary);
            record_test_snapshot(&inner_for_capture, secondary);
        });

        let result = server
            .script_eval(
                "return eguidev.fixture(\"multi\", nil, { timeout_ms = 200, poll_interval_ms = 1 })"
                    .to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
    }

    #[tokio::test]
    async fn fixture_data_anchor_waits_for_the_installed_data() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("viewer.mixed", "Analysed and unanalysed games.").ready_data(
                "status.summary",
                "/analysed",
                3,
            ),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Release);
            fixture_ok()
        });

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));

        // The widget set never changes; only the published data does.
        let publish = |analysed: i64| {
            let mut entry = make_entry("status.summary", 1, WidgetRole::Label);
            entry.data = Some(json!({ "analysed": analysed }));
            inner.widgets.clear_registry(egui::ViewportId::ROOT);
            inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
            inner.widgets.finalize_registry(egui::ViewportId::ROOT);
        };
        publish(0);
        capture_test_frame(&inner, &ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_for_capture = Arc::clone(&inner);
        let handler_called_for_capture = Arc::clone(&handler_called);
        tokio::spawn(async move {
            let capture_ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            discard_output(capture_ctx.run_ui(raw_input, |_| {}));
            while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
                sleep(Duration::from_millis(1)).await;
            }
            sleep(Duration::from_millis(10)).await;
            let mut entry = make_entry("status.summary", 1, WidgetRole::Label);
            entry.data = Some(json!({ "analysed": 3 }));
            inner_for_capture
                .widgets
                .clear_registry(egui::ViewportId::ROOT);
            inner_for_capture
                .widgets
                .record_widget(egui::ViewportId::ROOT, entry);
            inner_for_capture
                .widgets
                .finalize_registry(egui::ViewportId::ROOT);
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        let result = server
            .script_eval(
                "return eguidev.fixture(\"viewer.mixed\", nil, { timeout_ms = 500, poll_interval_ms = 1 })"
                    .to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
    }

    #[tokio::test]
    async fn fixture_data_anchor_times_out_on_unmatched_data() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("viewer.mixed", "Analysed and unanalysed games.").ready_data(
                "status.summary",
                "/analysed",
                3,
            ),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Release);
            fixture_ok()
        });

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        let mut entry = make_entry("status.summary", 1, WidgetRole::Label);
        entry.data = Some(json!({ "analysed": 1 }));
        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner, &ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_for_capture = Arc::clone(&inner);
        let ctx_for_capture = ctx.clone();
        let handler_called_for_capture = Arc::clone(&handler_called);
        tokio::spawn(async move {
            while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
                sleep(Duration::from_millis(1)).await;
            }
            sleep(Duration::from_millis(10)).await;
            capture_test_frame(&inner_for_capture, &ctx_for_capture);
        });
        let result = server
            .script_eval(
                "return eguidev.fixture(\"viewer.mixed\", nil, { timeout_ms = 80, poll_interval_ms = 1 })"
                    .to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false, "{json:?}");
        assert_eq!(json["error"]["type"], "timeout");
        assert!(
            json["error"]["details"]["widget"]["data"]["analysed"] == 1,
            "timeout should preserve the unmatched widget data: {json:?}"
        );
    }

    #[tokio::test]
    async fn fixture_waits_for_scroll_anchor_stability() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("scroll", "Scroll fixture.").ready_scroll_at(
                "scroll",
                Vec2 { x: 0.0, y: 300.0 },
                1.0,
            ),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Release);
            fixture_ok()
        });

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_for_capture = Arc::clone(&inner);
        let handler_called_for_capture = Arc::clone(&handler_called);
        tokio::spawn(async move {
            let capture_ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            discard_output(capture_ctx.run_ui(raw_input, |_| {}));
            while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
                sleep(Duration::from_millis(1)).await;
            }
            sleep(Duration::from_millis(10)).await;
            inner_for_capture
                .widgets
                .clear_registry(egui::ViewportId::ROOT);
            inner_for_capture.widgets.record_widget(
                egui::ViewportId::ROOT,
                make_scroll_entry(
                    "scroll",
                    1,
                    Vec2 { x: 0.0, y: 300.0 },
                    Vec2 { x: 0.0, y: 600.0 },
                ),
            );
            inner_for_capture
                .widgets
                .finalize_registry(egui::ViewportId::ROOT);
            capture_test_frame(&inner_for_capture, &capture_ctx);
            sleep(Duration::from_millis(25)).await;
            inner_for_capture
                .widgets
                .clear_registry(egui::ViewportId::ROOT);
            inner_for_capture.widgets.record_widget(
                egui::ViewportId::ROOT,
                make_scroll_entry(
                    "scroll",
                    1,
                    Vec2 { x: 0.0, y: 300.0 },
                    Vec2 { x: 0.0, y: 600.0 },
                ),
            );
            inner_for_capture
                .widgets
                .finalize_registry(egui::ViewportId::ROOT);
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        let result = server
            .script_eval(
                "return eguidev.fixture(\"scroll\", nil, { timeout_ms = 250, poll_interval_ms = 1 })"
                    .to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
    }

    #[tokio::test]
    async fn wait_for_fresh_capture_waits_for_new_snapshot() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);

        let inner_for_capture = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let capture_ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            discard_output(capture_ctx.run_ui(raw_input, |_| {}));
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        server
            .wait_for_fresh_capture(None, Some(500), Some(1))
            .await
            .expect("wait_for_fresh_capture");
    }

    #[tokio::test]
    async fn shared_scroll_ready_condition_waits_for_stable_scroll_state() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_scroll_entry(
                "scroll",
                1,
                Vec2 { x: 0.0, y: 150.0 },
                Vec2 { x: 0.0, y: 400.0 },
            ),
        );
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner, &ctx);

        let inner_for_capture = Arc::clone(&inner);
        tokio::spawn(async move {
            let capture_ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            discard_output(capture_ctx.run_ui(raw_input, |_| {}));
            sleep(Duration::from_millis(50)).await;
            inner_for_capture
                .widgets
                .clear_registry(egui::ViewportId::ROOT);
            inner_for_capture.widgets.record_widget(
                egui::ViewportId::ROOT,
                make_scroll_entry(
                    "scroll",
                    1,
                    Vec2 { x: 0.0, y: 150.0 },
                    Vec2 { x: 0.0, y: 400.0 },
                ),
            );
            inner_for_capture
                .widgets
                .finalize_registry(egui::ViewportId::ROOT);
            capture_test_frame(&inner_for_capture, &capture_ctx);
            sleep(Duration::from_millis(60)).await;
            inner_for_capture
                .widgets
                .clear_registry(egui::ViewportId::ROOT);
            inner_for_capture.widgets.record_widget(
                egui::ViewportId::ROOT,
                make_scroll_entry(
                    "scroll",
                    1,
                    Vec2 { x: 0.0, y: 150.0 },
                    Vec2 { x: 0.0, y: 400.0 },
                ),
            );
            inner_for_capture
                .widgets
                .finalize_registry(egui::ViewportId::ROOT);
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        let result = server
            .script_eval(
                r#"local state = eguidev.widget("scroll"):wait(
    { scroll_ready = true },
    { timeout_ms = 500, poll_interval_ms = 1 }
)
assert(state ~= nil and state.scroll_state ~= nil)
return state.scroll_state.offset.y"#
                    .to_string(),
                Some(TEST_SCRIPT_DEADLINE_MS),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json}");
        assert_eq!(json["value"], 150.0);
    }

    #[tokio::test]
    async fn fixture_rejects_unregistered_names() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("known", "Known fixture.").ready("status"),
        ]);
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture_apply("unknown".to_string(), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fixtures_are_sorted_for_scripts() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("zeta", "Last fixture.").ready("status"),
            FixtureSpec::new("alpha", "First fixture.").ready("status"),
        ]);
        let specs = inner.fixtures.fixtures_sorted();
        let specs: Vec<_> = specs.into_iter().map(|fixture| fixture.name).collect();
        assert_eq!(specs, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[tokio::test]
    async fn fixture_clears_transient_automation_state_on_apply_boundaries() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("reset", "Reset fixture.").ready("status"),
        ]);
        let applied = Arc::new(AtomicBool::new(false));
        let applied_handler = Arc::clone(&applied);
        set_runtime_fixture_handler(&inner, move |_call| {
            applied_handler.store(true, AtomicOrdering::Relaxed);
            fixture_ok()
        });
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );
        let viewport_id = egui::ViewportId::ROOT;
        inner.queue_action(
            viewport_id,
            InputAction::Text {
                text: "stale".to_string(),
            },
        );
        inner.queue_action_with_timing(
            viewport_id,
            ActionTiming::AfterOneFrame,
            InputAction::Text {
                text: "staged".to_string(),
            },
        );
        inner.queue_command(
            viewport_id,
            egui::ViewportCommand::Title("stale title".to_string()),
        );
        inner.queue_widget_value_update(
            viewport_id,
            "field".to_string(),
            WidgetValue::Text("queued".to_string()),
        );
        inner.set_scroll_override(viewport_id, 7, egui::vec2(1.0, 2.0));
        inner.set_overlay_debug_config(OverlayDebugConfig {
            enabled: true,
            ..Default::default()
        });

        let inner_for_frame = Arc::clone(&inner);
        let runtime_for_frame = Runtime::ensure_for_inner(&inner);
        tokio::spawn(async move {
            for _ in 0..4 {
                yield_now().await;
                inner_for_frame.advance_frame();
                runtime_for_frame.frame_notify().notify_waiters();
            }
        });
        let server = DevMcpServer::new(Arc::clone(&inner));
        server
            .fixture_apply("reset".to_string(), None)
            .await
            .expect("fixture result");

        assert!(applied.load(AtomicOrdering::Relaxed));
        // After fixture application, transient state should be cleared.
        assert!(!inner.actions.has_pending_actions(viewport_id));
        assert!(!inner.actions.has_pending_commands(viewport_id));
        assert!(
            inner
                .take_widget_value_update(viewport_id, "field")
                .is_none()
        );
        assert!(inner.take_scroll_override(viewport_id, 7).is_none());
        assert!(!inner.overlays.overlay_debug_config().enabled);
    }

    #[tokio::test]
    async fn action_drag_relative_updates_slider_value() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let mut value = 42.0_f32;

        inner.widgets.clear_registry(viewport_id);
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "slider".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::Slider {
                            range: WidgetRange {
                                min: 0.0,
                                max: 100.0,
                            },
                        },
                        value: Some(WidgetValue::Float(f64::from(value))),
                        visible,
                        ..Default::default()
                    },
                );
            });
        }));
        inner.widgets.finalize_registry(viewport_id);

        server
            .action_drag_relative(
                None,
                widget_ref_id("slider"),
                Some(Vec2 { x: 0.2, y: 0.5 }),
                Vec2 { x: 0.8, y: 0.5 },
                None,
            )
            .await
            .expect("drag relative");

        // The drag stages one step per frame: land, press, move, release.
        while inner.actions.has_pending_actions(viewport_id) {
            let mut raw_input = egui::RawInput {
                viewport_id,
                ..Default::default()
            };
            apply_actions(&inner, &mut raw_input);
            discard_output(ctx.run_ui(raw_input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
                });
            }));
        }

        assert!(value > 42.0_f32);
    }

    #[tokio::test]
    async fn action_type_clear_replaces_text() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let mut text = "Hello".to_string();

        inner.widgets.clear_registry(viewport_id);
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_multiline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "notes".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::TextEdit {
                            multiline: false,
                            password: false,
                        },
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
        }));
        inner.widgets.finalize_registry(viewport_id);

        server
            .action_type(
                None,
                widget_ref_id("notes"),
                "World".to_string(),
                None,
                Some(true),
            )
            .await
            .expect("type action");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.text_edit_multiline(&mut text);
            });
        }));

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.text_edit_multiline(&mut text);
            });
        }));

        assert_eq!(text, "World");
    }

    #[tokio::test]
    async fn action_type_skips_click_when_widget_already_focused() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("notes", 1, WidgetRole::TextEdit);
        entry.focused = true;
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        server
            .action_type(
                None,
                widget_ref_id("notes"),
                "World".to_string(),
                None,
                None,
            )
            .await
            .expect("action type");

        let actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
        let click_presses = actions
            .iter()
            .filter(|action| matches!(action, InputAction::PointerButton { pressed: true, .. }))
            .count();
        assert_eq!(click_presses, 0);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, InputAction::Text { text } if text == "World"))
        );
    }

    #[test]
    fn interaction_ready_requires_visibility_and_a_visible_fraction() {
        let rect = Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        };
        let clipped_layout = |visible_fraction| WidgetLayout {
            desired_size: Vec2 { x: 10.0, y: 10.0 },
            actual_size: Vec2 { x: 10.0, y: 10.0 },
            clip_rect: rect,
            clipped: true,
            overflow: false,
            available_rect: rect,
            visible_fraction,
        };

        // No layout metadata means nothing is known to clip the widget.
        let plain = make_entry("plain", 1, WidgetRole::Button);
        assert!(interaction_ready(&plain));

        let mut hidden = make_entry("hidden", 1, WidgetRole::Button);
        hidden.visible = false;
        assert!(!interaction_ready(&hidden));

        let mut clipped = make_entry("clipped", 1, WidgetRole::Button);
        clipped.layout = Some(clipped_layout(0.0));
        assert!(!interaction_ready(&clipped));

        let mut partly_clipped = make_entry("partly", 1, WidgetRole::Button);
        partly_clipped.layout = Some(clipped_layout(0.25));
        assert!(interaction_ready(&partly_clipped));
    }

    #[tokio::test]
    async fn action_click_rejects_invisible_widget() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("hidden", 1, WidgetRole::Button);
        entry.visible = false;
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let error = server
            .action_click(None, widget_ref_id("hidden"), None, None, None)
            .await
            .expect_err("hidden widget should not be clicked");

        assert_eq!(error.code, ErrorCode::NotActionable.as_str());
        assert!(error.message.contains("scroll_into_view"));
    }

    #[tokio::test]
    async fn action_click_rejects_fully_clipped_widget() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;
        let rect = Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        };

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("clipped", 1, WidgetRole::Button);
        entry.layout = Some(WidgetLayout {
            desired_size: Vec2 { x: 10.0, y: 10.0 },
            actual_size: Vec2 { x: 10.0, y: 10.0 },
            clip_rect: rect,
            clipped: true,
            overflow: false,
            available_rect: rect,
            visible_fraction: 0.0,
        });
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let error = server
            .action_click(None, widget_ref_id("clipped"), None, None, None)
            .await
            .expect_err("fully clipped widget should not be clicked");

        assert_eq!(error.code, ErrorCode::NotActionable.as_str());
        assert!(error.message.contains("scroll_into_view"));
        let details = error
            .structured
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("details"))
            .expect("details");
        assert_eq!(details["reason"], "invisible_interaction");
        assert_eq!(details["layout"]["visible_fraction"], 0.0);
    }

    #[tokio::test]
    async fn action_focus_updates_widget_focus_after_frame() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let mut text = "Hello".to_string();

        inner.widgets.clear_registry(viewport_id);
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_multiline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "notes".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::TextEdit {
                            multiline: false,
                            password: false,
                        },
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
        }));
        inner.widgets.finalize_registry(viewport_id);

        server
            .action_focus(None, widget_ref_id("notes"))
            .await
            .expect("action focus");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        inner.widgets.clear_registry(viewport_id);
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_multiline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "notes".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::TextEdit {
                            multiline: false,
                            password: false,
                        },
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
        }));
        inner.widgets.finalize_registry(viewport_id);

        let focused = resolve_widget(&inner, None, &widget_ref_id("notes"))
            .expect("widget lookup")
            .focused;
        assert!(focused);
    }

    #[tokio::test]
    async fn viewport_resize_rejects_invalid_sizes_atomically() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .viewport_resize(
                None,
                ResizeOptions {
                    min_size: Some(Vec2 { x: -1.0, y: 480.0 }),
                    ..Default::default()
                },
            )
            .await
            .expect_err("invalid min_inner_size");
        assert_eq!(result.code, ErrorCode::InvalidArgument.as_str());
        assert!(result.message.contains("min_size"));
    }

    #[tokio::test]
    async fn viewport_resize_queues_one_validated_command_batch() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_size = Vec2 { x: 800.0, y: 600.0 };
        let min_inner_size = Vec2 { x: 400.0, y: 300.0 };
        let max_inner_size = Vec2 {
            x: 1200.0,
            y: 900.0,
        };
        let resize_increments = Vec2 { x: 10.0, y: 20.0 };

        server
            .viewport_resize(
                None,
                ResizeOptions {
                    inner_size: Some(inner_size),
                    min_size: Some(min_inner_size),
                    max_size: Some(max_inner_size),
                    increments: Some(resize_increments),
                    resizable: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("viewport resize");

        let commands = inner.actions.drain_commands(egui::ViewportId::ROOT);
        assert_eq!(
            commands,
            vec![
                egui::ViewportCommand::InnerSize(inner_size.into()),
                egui::ViewportCommand::MinInnerSize(min_inner_size.into()),
                egui::ViewportCommand::MaxInnerSize(max_inner_size.into()),
                egui::ViewportCommand::ResizeIncrements(Some(resize_increments.into())),
                egui::ViewportCommand::Resizable(true),
            ]
        );
    }

    #[tokio::test]
    async fn widget_list_respects_visibility() {
        let inner = Arc::new(Inner::new());
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        discard_output(ctx.run_ui(raw_input, |ctx| {
            inner.capture_context(ctx.viewport_id(), ctx);
            inner.widgets.clear_registry(ctx.viewport_id());
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(40.0)
                    .show(ui, |ui| {
                        let visible = ui.button("Visible");
                        let visible_flag = ui.is_rect_visible(visible.rect);
                        record_widget(
                            &inner.widgets,
                            "visible".to_string(),
                            &visible,
                            WidgetMeta {
                                role: WidgetRoleMeta::Button { selected: None },
                                visible: visible_flag,
                                ..Default::default()
                            },
                        );

                        ui.add_space(200.0);

                        let hidden = ui.button("Hidden");
                        let hidden_flag = ui.is_rect_visible(hidden.rect);
                        record_widget(
                            &inner.widgets,
                            "hidden".to_string(),
                            &hidden,
                            WidgetMeta {
                                role: WidgetRoleMeta::Button { selected: None },
                                visible: hidden_flag,
                                ..Default::default()
                            },
                        );
                    });
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        }));

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result: Vec<WidgetRegistryEntry> = server
            .widget_list(None, Some(false), None, None, None, None)
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let tags: Vec<_> = result.iter().map(|entry| entry.id.as_str()).collect();
        assert!(tags.contains(&"visible"));
        assert!(!tags.contains(&"hidden"));

        let result: Vec<WidgetRegistryEntry> = server
            .widget_list(None, Some(true), None, None, None, None)
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let tags: Vec<_> = result.iter().map(|entry| entry.id.as_str()).collect();
        assert!(tags.contains(&"visible"));
        assert!(tags.contains(&"hidden"));
    }

    #[tokio::test]
    async fn widget_lookup_missing_returns_search_details() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        for (index, id) in [
            "basic.abort",
            "basic.account",
            "basic.archive",
            "basic.cancel",
            "basic.delete",
        ]
        .into_iter()
        .enumerate()
        {
            inner.widgets.record_widget(
                viewport_id,
                make_entry(id, index as u64 + 1, WidgetRole::Button),
            );
        }
        inner.widgets.record_widget(
            viewport_id,
            make_entry("basic.submit", 20, WidgetRole::Button),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = resolve_widget(&inner, None, &widget_ref_id("basic.submt"))
            .expect_err("missing widget");
        assert_eq!(result.code(), ErrorCode::NotFound);
        assert!(result.message().contains("basic.submt"));
        let suggestions = result
            .details()
            .and_then(|details| details.get("search"))
            .and_then(|search| search.get("suggestions"))
            .and_then(Value::as_array)
            .expect("suggestions");
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.as_str() == Some("basic.submit"))
        );
    }

    #[tokio::test]
    async fn widget_lookup_missing_reports_exact_match_in_other_viewport() {
        let inner = Arc::new(Inner::new());
        let root = egui::ViewportId::ROOT;
        let secondary = egui::ViewportId::from_hash_of("secondary");

        let ctx = egui::Context::default();
        let mut raw_input = egui::RawInput {
            viewport_id: root,
            ..Default::default()
        };
        raw_input.viewports.insert(root, Default::default());
        raw_input.viewports.insert(secondary, Default::default());
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner
            .viewports
            .name_viewport(secondary, "secondary".to_string());
        inner.viewports.update_viewports(&ctx);

        inner.widgets.clear_registry(root);
        inner.widgets.finalize_registry(root);
        inner.widgets.clear_registry(secondary);
        inner.widgets.record_widget(
            secondary,
            make_entry("viewports.unwired.value", 1, WidgetRole::Slider),
        );
        inner.widgets.finalize_registry(secondary);

        let result = resolve_widget(&inner, None, &widget_ref_id("viewports.unwired.value"))
            .expect_err("root-scoped lookup should miss secondary widget");
        assert_eq!(result.code(), ErrorCode::NotFound);
        let search = result
            .details()
            .and_then(|details| details.get("search"))
            .expect("search details");
        let exact_matches = search
            .get("exact_matches")
            .and_then(Value::as_array)
            .expect("exact matches");
        assert!(exact_matches.iter().any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some("viewports.unwired.value")
                && entry
                    .get("viewport")
                    .and_then(|viewport| viewport.get("name"))
                    .and_then(Value::as_str)
                    == Some("secondary")
        }));
        let suggestions = search
            .get("suggestions")
            .and_then(Value::as_array)
            .expect("suggestions");
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.as_str() == Some("viewports.unwired.value"))
        );
    }

    #[tokio::test]
    async fn widget_lookup_round_trips_widget_list_id() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        let big_id = (1_u64 << 63) + 5;
        assert!(big_id > (1_u64 << 53));

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("big", big_id, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let list: Vec<WidgetRegistryEntry> = server
            .widget_list(None, Some(true), None, None, None, None)
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let entry = list
            .iter()
            .find(|entry| entry.id == "big")
            .expect("big widget");
        assert_eq!(entry.id, "big");

        let fetched =
            resolve_widget(&inner, None, &widget_ref_id(&entry.id)).expect("widget lookup");
        assert_eq!(fetched.id, "big");
    }

    #[tokio::test]
    async fn widget_lookup_fails_on_duplicate_explicit_ids() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "dup",
                1,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                Some("left"),
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "dup",
                2,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                Some("right"),
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = resolve_widget(&inner, None, &widget_ref_id("dup"))
            .expect_err("duplicate explicit ids should block automation");
        assert_eq!(result.code(), ErrorCode::InstrumentationFault);
        let duplicate_ids = result
            .details()
            .and_then(|details| details.get("duplicate_ids"))
            .and_then(|value| value.as_array())
            .expect("duplicate ids");
        assert_eq!(duplicate_ids.len(), 1);
        assert_eq!(
            duplicate_ids[0].get("id").and_then(Value::as_str),
            Some("dup")
        );
        let parent_chain = duplicate_ids[0]
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("parent_chain"))
            .and_then(Value::as_array)
            .expect("parent chain");
        assert!(
            parent_chain
                .iter()
                .any(|parent| parent.as_str() == Some("left"))
        );
    }

    #[tokio::test]
    async fn widget_lookup_generated_duplicates_remain_ambiguous() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut first = make_generated_entry(10, WidgetRole::Button);
        first.id = "dup".to_string();
        inner.widgets.record_widget(viewport_id, first);
        let mut second = make_generated_entry(11, WidgetRole::Button);
        second.id = "dup".to_string();
        inner.widgets.record_widget(viewport_id, second);
        inner.widgets.finalize_registry(viewport_id);

        let result = resolve_widget(&inner, None, &widget_ref_id("dup")).expect_err("ambiguous id");
        assert_eq!(result.code(), ErrorCode::NotFound);
        assert_ne!(result.code(), ErrorCode::InstrumentationFault);
    }

    #[tokio::test]
    async fn widget_lookup_accepts_generated_hex_id() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_generated_entry(5, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let list: Vec<WidgetRegistryEntry> = server
            .widget_list(None, Some(true), None, None, None, None)
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let entry = list
            .iter()
            .find(|entry| entry.id == "5")
            .expect("generated widget");

        let fetched = resolve_widget(&inner, None, &widget_ref_id(&entry.id))
            .expect("widget lookup by generated id");
        assert_eq!(fetched.id, entry.id);
    }

    #[tokio::test]
    async fn input_pointer_button_toggles_checkbox() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let mut checked = false;

        inner.widgets.clear_registry(viewport_id);
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.checkbox(&mut checked, "Enabled");
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "toggle".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::Checkbox {
                            indeterminate: None,
                        },
                        label: Some("Enabled".to_string()),
                        value: Some(WidgetValue::Bool(checked)),
                        visible,
                        ..Default::default()
                    },
                );
            });
        }));
        inner.widgets.finalize_registry(viewport_id);

        let widget = inner
            .widgets
            .widget_list(viewport_id)
            .into_iter()
            .find(|entry| entry.id == "toggle")
            .expect("toggle widget");
        let pos = widget.interact_rect.center();

        server
            .input(None, RawInputEvent::PointerMove { position: pos })
            .await
            .expect("pointer move");
        server
            .input(
                None,
                RawInputEvent::PointerButton {
                    position: pos,
                    button: PointerButton::Primary,
                    action: RawInputAction::Press,
                    modifiers: None,
                },
            )
            .await
            .expect("pointer down");
        server
            .input(
                None,
                RawInputEvent::PointerButton {
                    position: pos,
                    button: PointerButton::Primary,
                    action: RawInputAction::Release,
                    modifiers: None,
                },
            )
            .await
            .expect("pointer up");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.checkbox(&mut checked, "Enabled");
            });
        }));

        assert!(checked);
    }

    #[tokio::test]
    async fn raw_input_queues_exactly_one_event_per_call() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;
        let pos = Pos2 { x: 12.0, y: 24.0 };
        let modifiers = Modifiers {
            shift: true,
            ..Default::default()
        };

        server
            .input(None, RawInputEvent::PointerMove { position: pos })
            .await
            .expect("pointer move");
        server
            .input(
                None,
                RawInputEvent::PointerButton {
                    position: pos,
                    button: PointerButton::Primary,
                    action: RawInputAction::Press,
                    modifiers: Some(modifiers),
                },
            )
            .await
            .expect("pointer button");
        server
            .input(
                None,
                RawInputEvent::Key {
                    key: "Enter".to_string(),
                    action: RawInputAction::Press,
                    modifiers: Some(modifiers),
                },
            )
            .await
            .expect("key input");
        server
            .input(
                None,
                RawInputEvent::Text {
                    text: "Hello".to_string(),
                },
            )
            .await
            .expect("text input");
        server
            .input(
                None,
                RawInputEvent::Scroll {
                    delta: Vec2 { x: 1.0, y: -2.0 },
                    modifiers: Some(modifiers),
                },
            )
            .await
            .expect("scroll input");
        server
            .focus_window("root".to_string())
            .await
            .expect("focus window");

        let queued_actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
        assert_eq!(queued_actions.len(), 5);
        assert!(queued_actions.iter().any(|action| {
            matches!(
                action,
                InputAction::PointerMove { pos: queued_pos }
                    if queued_pos.x == pos.x && queued_pos.y == pos.y
            )
        }));
        assert!(queued_actions.iter().any(|action| {
            matches!(
                action,
                InputAction::PointerButton {
                    pos: queued_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: queued_modifiers,
                } if queued_pos.x == pos.x && queued_pos.y == pos.y && queued_modifiers.shift
            )
        }));
        assert!(queued_actions.iter().any(|action| {
            matches!(
                action,
                InputAction::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    modifiers: queued_modifiers,
                } if queued_modifiers.shift
            )
        }));
        assert!(
            queued_actions
                .iter()
                .any(|action| matches!(action, InputAction::Text { text } if text == "Hello"))
        );
        assert!(queued_actions.iter().any(|action| {
            matches!(
                action,
                InputAction::Scroll {
                    delta,
                    modifiers: queued_modifiers,
                } if delta.x == 1.0 && delta.y == -2.0 && queued_modifiers.shift
            )
        }));

        let pending_commands = inner.actions.drain_commands(viewport_id);
        assert_eq!(pending_commands, vec![egui::ViewportCommand::Focus]);

        let highlight_rect = Rect {
            min: Pos2 { x: 1.0, y: 2.0 },
            max: Pos2 { x: 3.0, y: 4.0 },
        };
        let highlight_result = server
            .show_highlight(None, None, Some(highlight_rect), "#ff0000".to_string())
            .await
            .expect("show_highlight");
        assert_eq!(highlight_result.rect.min.x, 1.0);
        assert_eq!(highlight_result.rect.min.y, 2.0);
        assert_eq!(highlight_result.rect.max.x, 3.0);
        assert_eq!(highlight_result.rect.max.y, 4.0);
        server
            .clear_highlights(None, None)
            .await
            .expect("clear_highlights");
    }

    #[test]
    fn scroll_input_moves_scroll_area() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        let mut raw_input = egui::RawInput {
            viewport_id,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 400.0),
            )),
            ..Default::default()
        };
        inner.queue_action(
            viewport_id,
            InputAction::PointerMove {
                pos: Pos2 { x: 50.0, y: 50.0 },
            },
        );
        inner.queue_action(
            viewport_id,
            InputAction::Scroll {
                delta: Vec2 { x: 0.0, y: -120.0 },
                modifiers: Modifiers::default(),
            },
        );

        apply_actions(&inner, &mut raw_input);

        let ctx = egui::Context::default();
        let mut offset = egui::Vec2::ZERO;
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let output = egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .show(ui, |ui| {
                        for row in 0..100 {
                            ui.label(format!("Row {row}"));
                        }
                    });
                offset = output.state.offset;
            });
        }));

        assert!(
            offset.y > 0.0,
            "expected scroll offset to move, got {offset:?}"
        );
    }

    #[tokio::test]
    async fn widget_list_filters_role_and_id_prefix() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("alpha", 1, WidgetRole::Button));
        inner.widgets.record_widget(
            viewport_id,
            make_entry("filter.match", 2, WidgetRole::Slider),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry("filter.skip", 3, WidgetRole::Button),
        );
        inner.widgets.finalize_registry(viewport_id);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result: Vec<WidgetRegistryEntry> = server
            .widget_list(
                None,
                Some(true),
                Some(WidgetRole::Slider),
                Some("filter.".to_string()),
                None,
                None,
            )
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let tags: Vec<_> = result.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(tags, vec!["filter.match"]);
    }

    #[tokio::test]
    async fn widget_list_filters_label_and_label_contains() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut ready = make_entry("status.ready", 1, WidgetRole::Label);
        ready.label = Some("Ready state".to_string());
        inner.widgets.record_widget(viewport_id, ready);
        let mut busy = make_entry("status.busy", 2, WidgetRole::Label);
        busy.label = Some("Busy state".to_string());
        inner.widgets.record_widget(viewport_id, busy);
        inner.widgets.finalize_registry(viewport_id);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let exact: Vec<WidgetRegistryEntry> = server
            .widget_list(
                None,
                Some(true),
                None,
                None,
                Some("Ready state".to_string()),
                None,
            )
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].id, "status.ready");

        let contains: Vec<WidgetRegistryEntry> = server
            .widget_list(
                None,
                Some(true),
                None,
                None,
                None,
                Some("state".to_string()),
            )
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let tags: Vec<_> = contains.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(tags, vec!["status.ready", "status.busy"]);
    }

    #[tokio::test]
    async fn widget_list_includes_values() {
        let inner = Arc::new(Inner::new());
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        let mut checked = true;
        let mut intensity = 42.0_f32;
        discard_output(ctx.run_ui(raw_input, |ctx| {
            inner.capture_context(ctx.viewport_id(), ctx);
            inner.widgets.clear_registry(ctx.viewport_id());
            egui::CentralPanel::default().show(ctx, |ui| {
                let checkbox = ui.checkbox(&mut checked, "Enabled");
                let checkbox_visible = ui.is_visible() && ui.is_rect_visible(checkbox.rect);
                record_widget(
                    &inner.widgets,
                    "basic.enabled".to_string(),
                    &checkbox,
                    WidgetMeta {
                        role: WidgetRoleMeta::Checkbox {
                            indeterminate: None,
                        },
                        label: Some("Enabled".to_string()),
                        value: Some(WidgetValue::Bool(checked)),
                        visible: checkbox_visible,
                        ..Default::default()
                    },
                );

                let slider = ui.add(egui::Slider::new(&mut intensity, 0.0..=100.0));
                let slider_visible = ui.is_visible() && ui.is_rect_visible(slider.rect);
                record_widget(
                    &inner.widgets,
                    "basic.intensity".to_string(),
                    &slider,
                    WidgetMeta {
                        role: WidgetRoleMeta::Slider {
                            range: WidgetRange {
                                min: 0.0,
                                max: 100.0,
                            },
                        },
                        value: Some(WidgetValue::Float(f64::from(intensity))),
                        visible: slider_visible,
                        ..Default::default()
                    },
                );
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        }));

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result: Vec<WidgetRegistryEntry> = server
            .widget_list(None, Some(true), None, None, None, None)
            .await
            .expect("widget list")
            .structured_as()
            .expect("widget list payload");
        let enabled = result
            .iter()
            .find(|entry| entry.id == "basic.enabled")
            .expect("enabled widget");
        let intensity_entry = result
            .iter()
            .find(|entry| entry.id == "basic.intensity")
            .expect("intensity widget");
        assert!(matches!(enabled.value, Some(WidgetValue::Bool(true))));
        assert!(matches!(
            intensity_entry.value,
            Some(WidgetValue::Float(value)) if (value - 42.0).abs() < f64::EPSILON
        ));
    }

    #[tokio::test]
    async fn action_click_click_count_queues_multiple_clicks() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("click", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        server
            .action_click(None, widget_ref_id("click"), None, None, Some(2))
            .await
            .expect("action click");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        let pointer_buttons = raw_input
            .events
            .iter()
            .filter(|event| matches!(event, egui::Event::PointerButton { .. }))
            .count();
        assert_eq!(pointer_buttons, 4);
    }

    #[tokio::test]
    async fn action_click_can_succeed_without_widget_state_change() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut status = make_entry("status", 1, WidgetRole::Label);
        status.value = Some(WidgetValue::Text("stable".to_string()));
        inner.widgets.record_widget(viewport_id, status);
        inner.widgets.finalize_registry(viewport_id);

        let before = inner
            .widgets
            .resolve_widget(&inner.viewports, None, &widget_ref_id("status"))
            .expect("status before");
        server
            .action_click(None, widget_ref_id("status"), None, None, None)
            .await
            .expect("action click");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        assert!(
            raw_input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::PointerButton { .. }))
        );

        let after = inner
            .widgets
            .resolve_widget(&inner.viewports, None, &widget_ref_id("status"))
            .expect("status after");
        assert_eq!(before.value, after.value);
    }

    #[tokio::test]
    async fn action_key_injects_text_event() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        server
            .action_key(None, egui::Key::A, Modifiers::default(), "a", None)
            .await
            .expect("action key");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        assert!(
            raw_input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(text) if text == "a"))
        );
    }

    #[tokio::test]
    async fn action_paste_queues_paste_event() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        server
            .action_paste(None, "Hello".to_string())
            .await
            .expect("action paste");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        assert!(
            raw_input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Paste(text) if text == "Hello"))
        );
    }

    #[tokio::test]
    async fn action_hover_queues_pointer_move() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("hover", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        server
            .action_hover(None, widget_ref_id("hover"), None, Some(0))
            .await
            .expect("action hover");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        assert!(raw_input.events.iter().any(|event| {
            matches!(event, egui::Event::PointerMoved(pos)
                if (pos.x - 5.0).abs() < f32::EPSILON && (pos.y - 5.0).abs() < f32::EPSILON)
        }));
    }

    #[tokio::test]
    async fn wait_for_widget_state_matches_existing_widget() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.value = Some(WidgetValue::Text("Ready".to_string()));
        entry.label = Some("Ready".to_string());
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .wait_for_widget_state(
                None,
                widget_ref_id("status"),
                Some(50),
                Some(1),
                "predicate",
                |widget| {
                    widget.and_then(|widget| widget.value.as_ref())
                        == Some(&WidgetValue::Text("Ready".to_string()))
                },
            )
            .await
            .expect("wait for widget");
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn wait_for_widget_state_matches_missing_widget() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));

        let result = server
            .wait_for_widget_state(
                None,
                widget_ref_id("missing"),
                Some(50),
                Some(1),
                "predicate",
                |widget| widget.is_none(),
            )
            .await
            .expect("wait for widget");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn wait_for_widget_state_matches_when_widget_becomes_visible() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.visible = false;
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let inner_for_update = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            inner_for_update.widgets.clear_registry(viewport_id);
            let mut entry = make_entry("status", 1, WidgetRole::Label);
            entry.visible = true;
            inner_for_update.widgets.record_widget(viewport_id, entry);
            inner_for_update.widgets.finalize_registry(viewport_id);
        });

        let result = server
            .wait_for_widget_state(
                None,
                widget_ref_id("status"),
                Some(100),
                Some(1),
                "predicate",
                |widget| widget.is_some_and(|widget| widget.visible),
            )
            .await
            .expect("wait for visible widget");
        assert!(result.is_some_and(|widget| widget.visible));
    }

    #[tokio::test]
    async fn wait_for_widget_state_matches_when_widget_appears() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);

        let inner_for_update = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            inner_for_update.widgets.clear_registry(viewport_id);
            inner_for_update
                .widgets
                .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
            inner_for_update.widgets.finalize_registry(viewport_id);
        });

        let result = server
            .wait_for_widget_state(
                None,
                widget_ref_id("status"),
                Some(100),
                Some(1),
                "predicate",
                |widget| widget.is_some(),
            )
            .await
            .expect("wait for appearing widget");
        assert!(result.is_some_and(|widget| widget.visible));
    }

    #[tokio::test]
    async fn wait_for_settle_matches_when_idle() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        // Run one frame so that the input snapshot is populated.
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);
        let inner_for_capture = Arc::clone(&inner);
        let capture_ctx = ctx.clone();
        tokio::spawn(async move {
            yield_now().await;
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        server
            .wait_for_settle(None, Some(50), Some(1))
            .await
            .expect("wait_for_settle");
    }

    #[tokio::test]
    async fn wait_for_settle_requires_a_new_frame() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);

        let error = server
            .wait_for_settle(None, Some(10), Some(1))
            .await
            .expect_err("wait_for_settle should time out");
        assert_eq!(error.code, "timeout");
        let phases = error
            .structured
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("details"))
            .and_then(|details| details.get("phases"))
            .and_then(Value::as_array)
            .expect("settle phases");
        assert!(
            phases
                .iter()
                .any(|phase| phase.get("phase").and_then(Value::as_str) == Some("fresh_frame"))
        );
    }

    #[tokio::test]
    async fn wait_for_settle_reports_busy_runtime_idle_phase() {
        let idle = Arc::new(AtomicBool::new(false));
        let idle_for_provider = Arc::clone(&idle);
        let devmcp = attach_for_tests(
            DevMcp::new()
                .keep_alive(false)
                .on_idle(move || idle_for_provider.load(AtomicOrdering::Relaxed))
                .expect("idle provider"),
        );
        let inner = devmcp.inner_arc().expect("attached inner");
        let runtime = Runtime::for_devmcp(&devmcp).expect("attached runtime");
        let server = DevMcpServer::with_runtime(Arc::clone(&inner), runtime);
        let ctx = egui::Context::default();

        run_instrumented_test_frame(
            &devmcp,
            &ctx,
            egui::ViewportId::ROOT,
            &[egui::ViewportId::ROOT],
        );
        let devmcp_for_frame = devmcp.clone();
        let ctx_for_frame = ctx.clone();
        tokio::spawn(async move {
            yield_now().await;
            run_instrumented_test_frame(
                &devmcp_for_frame,
                &ctx_for_frame,
                egui::ViewportId::ROOT,
                &[egui::ViewportId::ROOT],
            );
        });

        let error = server
            .wait_for_settle(None, Some(20), Some(1))
            .await
            .expect_err("busy app idle provider should block settle");
        let phases = error
            .structured
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("details"))
            .and_then(|details| details.get("phases"))
            .and_then(Value::as_array)
            .expect("settle phases");
        assert!(phases.iter().any(|phase| {
            phase.get("phase").and_then(Value::as_str) == Some("app_idle")
                && phase.get("complete").and_then(Value::as_bool) == Some(false)
        }));
    }

    #[tokio::test]
    async fn wait_for_settle_observes_ui_idle_on_child_viewport() {
        let idle = Arc::new(AtomicBool::new(false));
        let idle_for_provider = Arc::clone(&idle);
        let devmcp = attach_for_tests(
            DevMcp::new()
                .keep_alive(false)
                .on_idle_ui(move |_| idle_for_provider.load(AtomicOrdering::Relaxed))
                .expect("UI idle provider"),
        );
        let inner = devmcp.inner_arc().expect("attached inner");
        let runtime = Runtime::for_devmcp(&devmcp).expect("attached runtime");
        let server = DevMcpServer::with_runtime(Arc::clone(&inner), runtime);
        let root_ctx = egui::Context::default();
        let child_ctx = egui::Context::default();
        let child = egui::ViewportId::from_hash_of("ui-idle-child");
        let live_viewports = [egui::ViewportId::ROOT, child];

        run_instrumented_test_frame(&devmcp, &root_ctx, egui::ViewportId::ROOT, &live_viewports);
        run_instrumented_test_frame(&devmcp, &child_ctx, child, &live_viewports);

        idle.store(true, AtomicOrdering::Relaxed);

        let devmcp_for_frame = devmcp.clone();
        let root_ctx_for_frame = root_ctx.clone();
        let child_ctx_for_frame = child_ctx.clone();
        let frame_task = tokio::spawn(async move {
            yield_now().await;
            run_instrumented_test_frame(
                &devmcp_for_frame,
                &root_ctx_for_frame,
                egui::ViewportId::ROOT,
                &live_viewports,
            );
            run_instrumented_test_frame(
                &devmcp_for_frame,
                &child_ctx_for_frame,
                child,
                &live_viewports,
            );
        });

        let result = server
            .wait_for_settle(Some(viewport_id_to_string(child)), Some(100), Some(1))
            .await;
        frame_task.await.expect("frame task");
        result.expect("child settle should refresh root UI idle state");
    }

    #[tokio::test]
    async fn wait_for_settle_requires_clean_frame_after_action_drain() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);
        inner.queue_action(
            viewport_id,
            InputAction::PointerMove {
                pos: Pos2 { x: 1.0, y: 1.0 },
            },
        );

        let inner_for_capture = Arc::clone(&inner);
        let capture_ctx = ctx.clone();
        tokio::spawn(async move {
            yield_now().await;
            drain_test_actions(&inner_for_capture, viewport_id);
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        let error = server
            .wait_for_settle(None, Some(20), Some(1))
            .await
            .expect_err("wait_for_settle should require a clean post-action frame");
        assert_eq!(error.code, "timeout");
    }

    #[tokio::test]
    async fn wait_for_settle_matches_clean_frame_after_action_drain() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);
        inner.queue_action(
            viewport_id,
            InputAction::PointerMove {
                pos: Pos2 { x: 1.0, y: 1.0 },
            },
        );

        let inner_for_capture = Arc::clone(&inner);
        let capture_ctx = ctx.clone();
        tokio::spawn(async move {
            yield_now().await;
            drain_test_actions(&inner_for_capture, viewport_id);
            capture_test_frame(&inner_for_capture, &capture_ctx);
            yield_now().await;
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        let report = server
            .wait_for_settle(None, Some(100), Some(1))
            .await
            .expect("wait_for_settle");
        assert!(report.settled);
        assert!(
            report
                .phases
                .iter()
                .any(|phase| matches!(phase.phase, SettlePhase::CleanCapture))
        );
        assert!(inner.frame_count() >= 3);
    }

    #[tokio::test]
    async fn wait_for_settle_matches_when_child_viewport_closes_after_action_drain() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::from_hash_of("closing-child");

        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        raw_input.viewports.insert(viewport_id, Default::default());
        discard_output(ctx.run_ui(raw_input, |_| {}));
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, viewport_id);

        inner.queue_action(
            viewport_id,
            InputAction::PointerMove {
                pos: Pos2 { x: 1.0, y: 1.0 },
            },
        );

        let inner_for_capture = Arc::clone(&inner);
        let capture_ctx = ctx.clone();
        tokio::spawn(async move {
            yield_now().await;
            drain_test_actions(&inner_for_capture, viewport_id);
            record_test_snapshot(&inner_for_capture, viewport_id);

            let mut raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            raw_input
                .viewports
                .insert(egui::ViewportId::ROOT, Default::default());
            discard_output(capture_ctx.run_ui(raw_input, |_| {}));
            inner_for_capture.viewports.update_viewports(&capture_ctx);
            record_test_snapshot(&inner_for_capture, egui::ViewportId::ROOT);
        });

        server
            .wait_for_settle(Some(viewport_id_to_string(viewport_id)), Some(100), Some(1))
            .await
            .expect("wait_for_settle should match after child viewport closes");
    }

    #[tokio::test]
    async fn widget_set_value_queues_update() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("field", 1, WidgetRole::TextEdit);
        entry.value = Some(WidgetValue::Text("Before".to_string()));
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        server
            .widget_set_value(
                None,
                widget_ref_id("field"),
                WidgetValue::Text("After".to_string()),
            )
            .await
            .expect("widget set value");

        let updated = inner
            .take_widget_value_update(viewport_id, "field")
            .expect("queued update");
        assert!(matches!(updated, WidgetValue::Text(value) if value == "After"));
    }

    #[tokio::test]
    async fn widget_set_value_accepts_collapsing_header_bool() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("advanced", 1, WidgetRole::CollapsingHeader);
        entry.value = Some(WidgetValue::Bool(false));
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        server
            .widget_set_value(None, widget_ref_id("advanced"), WidgetValue::Bool(true))
            .await
            .expect("widget set value");

        let updated = inner
            .take_widget_value_update(viewport_id, "advanced")
            .expect("queued update");
        assert_eq!(updated, WidgetValue::Bool(true));
    }

    #[tokio::test]
    async fn widget_set_value_reports_unconsumed_custom_override() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        record_test_snapshot(&inner, viewport_id);
        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("custom.value", 1, WidgetRole::Slider);
        entry.value = Some(WidgetValue::Int(0));
        entry.role_state = Some(RoleState::Slider {
            range: WidgetRange {
                min: 0.0,
                max: 10.0,
            },
        });
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        server
            .widget_set_value(None, widget_ref_id("custom.value"), WidgetValue::Int(5))
            .await
            .expect("queue set_value");

        let inner_for_capture = Arc::clone(&inner);
        tokio::spawn(async move {
            yield_now().await;
            record_test_snapshot(&inner_for_capture, viewport_id);
            yield_now().await;
            record_test_snapshot(&inner_for_capture, viewport_id);
        });

        let error = server
            .wait_for_widget_state(
                None,
                widget_ref_id("custom.value"),
                Some(100),
                Some(1),
                "predicate",
                |_| false,
            )
            .await
            .expect_err("unconsumed override should fail");

        assert_eq!(error.code, ErrorCode::InstrumentationFault.as_str());
        assert!(error.message.contains("take_widget_value_override"));
        drop(ctx);
    }

    #[tokio::test]
    async fn script_widget_parent_and_children_follow_hierarchy() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "root",
                1,
                WidgetRole::Window,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 100.0, y: 100.0 },
                },
                None,
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "child",
                2,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                Some("root"),
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "sibling",
                3,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 12.0, y: 0.0 },
                    max: Pos2 { x: 22.0, y: 10.0 },
                },
                Some("root"),
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "grand",
                4,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 12.0 },
                    max: Pos2 { x: 10.0, y: 22.0 },
                },
                Some("child"),
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"
                    local root = eguidev.widget("root")
                    local child = eguidev.widget("child")
                    local child_ids = {}
                    for _, widget in ipairs(root:children()) do
                        table.insert(child_ids, widget.id)
                    end
                    local grand_ids = {}
                    for _, widget in ipairs(child:children()) do
                        table.insert(grand_ids, widget.id)
                    end
                    local grand_parent = eguidev.widget("grand"):parent()
                    return {
                        child_count = #child_ids,
                        child_ids = table.concat(child_ids, ","),
                        grand_count = #grand_ids,
                        grand_ids = table.concat(grand_ids, ","),
                        grand_parent_child_count = grand_parent and #grand_parent:children() or 0,
                    }
                "#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true, "{json:?}");
        let value = &json["value"];
        assert_eq!(value.get("child_count").and_then(Value::as_u64), Some(2));
        assert_eq!(
            value.get("child_ids").and_then(Value::as_str),
            Some("child,sibling")
        );
        assert_eq!(value.get("grand_count").and_then(Value::as_u64), Some(1));
        assert_eq!(
            value.get("grand_ids").and_then(Value::as_str),
            Some("grand")
        );
        assert_eq!(
            value
                .get("grand_parent_child_count")
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[tokio::test]
    async fn script_widget_state_and_wait_for_use_widget_state() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        let mut button = make_entry("status", 1, WidgetRole::Button);
        button.label = Some("Ready".to_string());
        button.value = Some(WidgetValue::Text("Ready".to_string()));
        button.focused = true;
        inner.widgets.record_widget(viewport_id, button);
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .script_eval(
                r#"
                    local widget = eguidev.widget("status")
                    local state = widget:state()
                    local waited = widget:wait(function(current)
                        return current ~= nil
                            and current.focused
                            and current.value ~= nil
                            and current.value == "Ready"
                    end, { timeout_ms = 20, poll_interval_ms = 1 })
                    assert(state ~= nil)
                    return {
                        role = state.role,
                        focused = state.focused,
                        value_text = state.value,
                        waited = waited ~= nil,
                        waited_role = waited ~= nil and waited.role or nil,
                    }
                "#
                .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        let value = &json["value"];
        assert_eq!(value.get("role").and_then(Value::as_str), Some("button"));
        assert_eq!(
            value.get("value_text").and_then(Value::as_str),
            Some("Ready")
        );
        assert_eq!(value.get("focused").and_then(Value::as_bool), Some(true));
        assert_eq!(value.get("waited").and_then(Value::as_bool), Some(true));
        assert_eq!(
            value.get("waited_role").and_then(Value::as_str),
            Some("button")
        );
    }

    #[tokio::test]
    async fn check_layout_reports_overlap() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "first",
                1,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                None,
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "second",
                2,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 5.0, y: 5.0 },
                    max: Pos2 { x: 15.0, y: 15.0 },
                },
                None,
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = server.check_layout(None, None).await.expect("layout check");
        assert!(
            result
                .iter()
                .any(|issue| matches!(issue.kind, LayoutIssueKind::Overlap))
        );
    }

    #[tokio::test]
    async fn text_measure_returns_text() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        let text = "Hello".to_string();
        discard_output(ctx.run_ui(raw_input, |ctx| {
            inner.capture_context(ctx.viewport_id(), ctx);
            inner.widgets.clear_registry(ctx.viewport_id());
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.label(&text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "label".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::Plain(WidgetRole::Label),
                        label: Some(text.clone()),
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        }));

        let result = server
            .text_measure(widget_ref_id("label"))
            .await
            .expect("text measure");
        assert_eq!(result.text, "Hello");
        assert_eq!(result.text.chars().count(), 5);
        assert!(!result.lines.is_empty());
    }

    #[tokio::test]
    async fn script_eval_widget_text_measure_returns_text() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        let text = "Hello".to_string();
        discard_output(ctx.run_ui(raw_input, |ctx| {
            inner.capture_context(ctx.viewport_id(), ctx);
            inner.widgets.clear_registry(ctx.viewport_id());
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.label(&text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "label".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::Plain(WidgetRole::Label),
                        label: Some(text.clone()),
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        }));

        let result = server
            .script_eval(
                r#"return eguidev.widget("label"):text_measure().text"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], "Hello");
    }

    #[tokio::test]
    async fn text_measure_uses_widget_viewport_context() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let secondary = egui::ViewportId::from_hash_of("secondary");
        for _ in 0..2 {
            let mut raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            raw_input
                .viewports
                .insert(egui::ViewportId::ROOT, Default::default());
            raw_input.viewports.insert(secondary, Default::default());
            discard_output(ctx.run_ui(raw_input, |_| {}));
        }
        inner.capture_context(secondary, &ctx);
        inner.viewports.update_viewports(&ctx);

        inner.widgets.clear_registry(secondary);
        let mut entry = make_entry("label.secondary", 1, WidgetRole::Label);
        entry.viewport_id = viewport_id_to_string(secondary);
        entry.value = Some(WidgetValue::Text("Hello".to_string()));
        inner.widgets.record_widget(secondary, entry);
        inner.widgets.finalize_registry(secondary);

        let result = server
            .text_measure(WidgetRef {
                id: "label.secondary".to_string(),
                viewport_id: Some(viewport_id_to_string(secondary)),
            })
            .await
            .expect("text measure");
        assert_eq!(result.text, "Hello");
    }

    #[tokio::test]
    async fn check_layout_text_truncation_uses_scope_viewport_context() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let secondary = egui::ViewportId::from_hash_of("secondary");
        for _ in 0..2 {
            let mut raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            raw_input
                .viewports
                .insert(egui::ViewportId::ROOT, Default::default());
            raw_input.viewports.insert(secondary, Default::default());
            discard_output(ctx.run_ui(raw_input, |_| {}));
        }
        inner.capture_context(secondary, &ctx);
        inner.viewports.update_viewports(&ctx);

        inner.widgets.clear_registry(secondary);
        let mut entry = make_entry_with_rect(
            "label.secondary",
            1,
            WidgetRole::Label,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 20.0, y: 10.0 },
            },
            None,
        );
        entry.viewport_id = viewport_id_to_string(secondary);
        entry.value = Some(WidgetValue::Text("HelloWorld".to_string()));
        inner.widgets.record_widget(secondary, entry);
        inner.widgets.finalize_registry(secondary);

        let result = server
            .check_layout(Some(viewport_id_to_string(secondary)), None)
            .await
            .expect("layout check");
        assert!(result.iter().all(|issue| !issue.widgets.is_empty()));
    }

    #[tokio::test]
    async fn widget_at_point_reports_topmost() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "bottom",
                1,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                None,
            ),
        );
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "top",
                2,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 10.0, y: 10.0 },
                },
                None,
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .widget_at_point_result(Pos2 { x: 5.0, y: 5.0 }, Some(true), None)
            .expect("widget at point");
        assert_eq!(result.widgets.len(), 2);
        assert_eq!(result.widgets[0].id, "top");
    }

    #[tokio::test]
    async fn check_layout_scopes_to_widget_subtree() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("left", 1, WidgetRole::Window));
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "left.row",
                2,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 0.0, y: 0.0 },
                    max: Pos2 { x: 20.0, y: 10.0 },
                },
                Some("left"),
            ),
        );
        inner
            .widgets
            .record_widget(viewport_id, make_entry("right", 3, WidgetRole::Window));
        inner.widgets.record_widget(
            viewport_id,
            make_entry_with_rect(
                "right.row",
                4,
                WidgetRole::Button,
                Rect {
                    min: Pos2 { x: 50.0, y: 0.0 },
                    max: Pos2 { x: 80.0, y: 10.0 },
                },
                Some("right"),
            ),
        );
        inner.widgets.finalize_registry(viewport_id);

        let result = server
            .check_layout(None, Some(widget_ref_id("right.row")))
            .await
            .expect("layout check");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn show_clear_debug_overlay() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;

        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("overlay", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        server
            .show_debug_overlay(None, Some(OverlayDebugModeName::Bounds), None, None)
            .await
            .expect("show debug overlay");
        let config = inner.overlays.overlay_debug_config();
        assert!(config.enabled);
        assert_eq!(config.mode, OverlayDebugMode::Bounds);

        server
            .clear_debug_overlay()
            .await
            .expect("hide debug overlay");
        assert!(!inner.overlays.overlay_debug_config().enabled);
    }

    /// Verify that injecting Enter via action_key causes a singleline TextEdit
    /// to surrender focus, matching the standard egui pattern:
    ///   response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
    /// Uses raw_input.focused = false on non-action frames to prove the
    /// mechanism works even when the window lacks OS focus.
    #[tokio::test]
    async fn action_key_enter_surrenders_focus_on_singleline() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let mut text = String::new();
        let mut enter_detected = false;

        let render_widget =
            |inner: &Inner, ctx: &egui::Context, text: &mut String, raw_input: egui::RawInput| {
                discard_output(ctx.run_ui(raw_input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.text_edit_singleline(text);
                        let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                        record_widget(
                            &inner.widgets,
                            "input".to_string(),
                            &response,
                            WidgetMeta {
                                role: WidgetRoleMeta::TextEdit {
                                    multiline: false,
                                    password: false,
                                },
                                value: Some(WidgetValue::Text(text.clone())),
                                visible,
                                ..Default::default()
                            },
                        );
                        response.lost_focus()
                    });
                }));
                inner.widgets.finalize_registry(viewport_id);
            };

        // Simulate no OS window focus — platform frames report focused=false.
        let no_focus_raw = || egui::RawInput {
            viewport_id,
            focused: false,
            ..Default::default()
        };

        // Frame 1: initial render (unfocused, no OS focus).
        inner.widgets.clear_registry(viewport_id);
        render_widget(&inner, &ctx, &mut text, no_focus_raw());

        // Type text (queues click-to-focus + text input).
        server
            .action_type(
                None,
                widget_ref_id("input"),
                "hello".to_string(),
                None,
                None,
            )
            .await
            .expect("type action");

        // Frame 2: apply click-to-focus.
        let mut raw = no_focus_raw();
        apply_actions(&inner, &mut raw);
        render_widget(&inner, &ctx, &mut text, raw);

        // Frame 3: apply staged text input after focus is established.
        let mut raw = no_focus_raw();
        apply_actions(&inner, &mut raw);
        render_widget(&inner, &ctx, &mut text, raw);
        assert_eq!(text, "hello");

        // Widget should report focused=true via egui memory focus even when
        // raw_input.focused was forced only for the action frame.
        let widget = inner
            .widgets
            .resolve_widget(&inner.viewports, None, &widget_ref_id("input"))
            .expect("widget should exist");
        assert!(widget.focused, "widget should have egui memory focus");

        // Frame 4: no actions, no OS focus — the settle frame.
        // Widget must retain egui memory focus even though the window
        // doesn't have OS focus.
        render_widget(&inner, &ctx, &mut text, no_focus_raw());
        let widget = inner
            .widgets
            .resolve_widget(&inner.viewports, None, &widget_ref_id("input"))
            .expect("widget should exist");
        assert!(
            widget.focused,
            "widget must retain memory focus across settle frame without OS focus"
        );

        // Queue Enter key.
        server
            .action_key(None, egui::Key::Enter, Modifiers::default(), "Enter", None)
            .await
            .expect("key action");

        // Frame 5: apply Enter and detect lost_focus + key_pressed(Enter).
        let mut raw = no_focus_raw();
        apply_actions(&inner, &mut raw);
        discard_output(ctx.run_ui(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_singleline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "input".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRoleMeta::TextEdit {
                            multiline: false,
                            password: false,
                        },
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    enter_detected = true;
                }
            });
        }));
        inner.widgets.finalize_registry(viewport_id);

        assert!(
            enter_detected,
            "Enter key should cause lost_focus + key_pressed(Enter)"
        );
    }
}
