//! Internal egui automation helpers plus the script-first MCP facade.
//!
//! The supported embedded MCP surface stays script-first: `script_eval` and
//! `script_api` are the general automation entry points, while `fixture` and
//! `fixture_apply` exist as structured handoff helpers for `edev fixture`.
//! The broader helper set in this module supports Luau scripts and internal
//! testing, not additional top-level MCP tools.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::codecs::jpeg::JpegEncoder;
use serde::Serialize;
use serde_json::{Value, json};
use tmcp::{
    ServerCtx, ToolResult, mcp_server,
    schema::{CallToolResult, ClientCapabilities, Implementation, InitializeResult},
    tool, tool_result,
};
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
    },
    registry::{Inner, viewport_id_to_string},
    runtime::Runtime,
    screenshots::{ScreenshotKind, ScreenshotState},
    script_definitions_with_preludes,
    tree::collect_subtree,
    types::{
        Anchor, AnchorCheck, FixtureCall, FixtureResponse, FixtureSpec, Modifiers, Pos2, Rect,
        RoleState, Vec2, WidgetRef, WidgetRegistryEntry, WidgetRole, WidgetState, WidgetValue,
    },
    viewports::ViewportSnapshot,
};

pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 16;
const STALLED_FRAME_AGE_MS: u64 = 500;
const SCROLL_STABILITY_TOLERANCE: f32 = 0.75;
mod layout;
mod results;
pub mod script;
mod types;
mod utils;

use layout::*;
use results::*;
pub use script::{
    FixtureApplication, ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo,
    ScriptEvalOptions, ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation,
    ScriptTiming,
};
use types::{OverlayDebugModeName, OverlayDebugOptionsInput, PointerButtonName, ScrollAlign};
use utils::{parse_key_combo, resolve_key_name, *};

pub use crate::error::*;

pub const DEFAULT_SCRIPT_EVAL_TIMEOUT_MS: u64 = script::DEFAULT_SCRIPT_TIMEOUT_MS;

fn scroll_state(widget: &WidgetRegistryEntry) -> Option<crate::ScrollAreaMeta> {
    widget.role_state.as_ref().and_then(RoleState::scroll_state)
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AnchorStatus {
    pub widget_id: String,
    pub viewport_id: Option<String>,
    pub check: String,
    pub satisfied: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub current_state: Option<WidgetState>,
}

#[derive(Debug, Clone, Serialize)]
struct AnchorEvaluationSnapshot {
    fixture: String,
    statuses: Vec<AnchorStatus>,
}

#[derive(Debug, Clone)]
#[tool_result]
pub struct FixtureApplyOutcome {
    pub params: BTreeMap<String, WidgetValue>,
    pub values: BTreeMap<String, WidgetValue>,
    pub anchors: Vec<AnchorStatus>,
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

#[derive(Debug, Clone, Copy)]
struct ScrollSample {
    frame_count: u64,
    max_offset: Vec2,
    stabilized: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AppHealthReport {
    frame_count: u64,
    fixture_epoch: u64,
    keep_alive: bool,
    animations: bool,
    known_viewports: Vec<String>,
    stalled: bool,
    viewports: Vec<ViewportHealthReport>,
}

#[derive(Debug, Clone, Serialize)]
struct ViewportHealthReport {
    viewport_id: String,
    frame_count: Option<u64>,
    last_frame_age_ms: Option<u64>,
    stalled: bool,
    snapshot: Option<ViewportSnapshot>,
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

fn approx_eq_vec2(left: Vec2, right: Vec2, tolerance: f32) -> bool {
    (left.x - right.x).abs() <= tolerance && (left.y - right.y).abs() <= tolerance
}

fn anchor_target(anchor: &Anchor) -> WidgetRef {
    WidgetRef {
        id: Some(anchor.widget_id.clone()),
        viewport_id: anchor.viewport_id.clone(),
    }
}

fn anchor_status(
    anchor: &Anchor,
    satisfied: bool,
    detail: impl Into<String>,
    current_state: Option<WidgetState>,
) -> AnchorStatus {
    anchor_status_with_details(anchor, satisfied, detail, None, current_state)
}

fn anchor_status_with_details(
    anchor: &Anchor,
    satisfied: bool,
    detail: impl Into<String>,
    details: Option<Value>,
    current_state: Option<WidgetState>,
) -> AnchorStatus {
    AnchorStatus {
        widget_id: anchor.widget_id.clone(),
        viewport_id: anchor.viewport_id.clone(),
        check: anchor.check.to_string(),
        satisfied,
        detail: detail.into(),
        details,
        current_state,
    }
}

fn format_anchor_status(status: &AnchorStatus) -> String {
    let marker = if status.satisfied { "✓" } else { "✗" };
    let target = match &status.viewport_id {
        Some(viewport_id) => format!("{} in {}", status.widget_id, viewport_id),
        None => status.widget_id.clone(),
    };
    format!("{marker} {target} {} — {}", status.check, status.detail)
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

fn fixture_error_to_tool(error: eguidev::FixtureError) -> ToolError {
    let code = match error.code.as_str() {
        "timeout" => ErrorCode::Timeout,
        "unknown_param"
        | "missing_param"
        | "invalid_param_type"
        | "invalid_param_choice"
        | "param_below_min"
        | "param_above_max" => ErrorCode::InvalidRef,
        _ => ErrorCode::Internal,
    };
    let mut tool_error = ToolError::new(code, error.message);
    if let Some(details) = error.details {
        tool_error = tool_error.with_details(json!({
            "code": error.code,
            "details": details,
        }));
    } else {
        tool_error = tool_error.with_details(json!({
            "code": error.code,
        }));
    }
    tool_error
}

fn update_scroll_stability(
    scroll_samples: &Mutex<HashMap<(String, String), ScrollSample>>,
    widget: &WidgetRegistryEntry,
    current_sample: ScrollSample,
) -> bool {
    let key = (widget.viewport_id.clone(), widget.id.clone());
    let mut samples = scroll_samples
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match samples.get_mut(&key) {
        Some(previous) => {
            let stabilized = if previous.stabilized {
                approx_eq_vec2(
                    previous.max_offset,
                    current_sample.max_offset,
                    SCROLL_STABILITY_TOLERANCE,
                )
            } else {
                previous.frame_count != current_sample.frame_count
                    && approx_eq_vec2(
                        previous.max_offset,
                        current_sample.max_offset,
                        SCROLL_STABILITY_TOLERANCE,
                    )
            };
            *previous = ScrollSample {
                stabilized,
                ..current_sample
            };
            stabilized
        }
        None => {
            samples.insert(
                key,
                ScrollSample {
                    stabilized: false,
                    ..current_sample
                },
            );
            false
        }
    }
}

fn app_health_report(inner: &Inner) -> AppHealthReport {
    let snapshots = inner.viewports.viewports_snapshot();
    let mut by_viewport = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.viewport_id.clone(), (Some(snapshot), None)))
        .collect::<BTreeMap<_, _>>();

    for health in inner.frame_health_snapshot() {
        let viewport_id = viewport_id_to_string(health.viewport_id);
        by_viewport
            .entry(viewport_id)
            .and_modify(|(_, stored_health)| *stored_health = Some(health))
            .or_insert((None, Some(health)));
    }

    let viewports = by_viewport
        .into_iter()
        .map(|(viewport_id, (snapshot, health))| {
            let last_frame_age_ms = health.map(|health| health.age().as_millis() as u64);
            let stalled = health.is_none()
                || last_frame_age_ms.is_some_and(|age| age >= STALLED_FRAME_AGE_MS);
            ViewportHealthReport {
                viewport_id,
                frame_count: health.map(|health| health.frame_count),
                last_frame_age_ms,
                stalled,
                snapshot,
            }
        })
        .collect::<Vec<_>>();
    let options = inner.automation_options();
    AppHealthReport {
        frame_count: inner.frame_count(),
        fixture_epoch: inner.fixture_epoch(),
        keep_alive: options.keep_alive,
        animations: options.animations,
        known_viewports: viewports
            .iter()
            .map(|viewport| viewport.viewport_id.clone())
            .collect(),
        stalled: viewports.iter().any(|viewport| viewport.stalled),
        viewports,
    }
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
            ErrorCode::InvalidRef,
            "Sample position must be finite",
        ));
    }
    if physical_x < 0.0
        || physical_y < 0.0
        || physical_x >= image.size[0] as f32
        || physical_y >= image.size[1] as f32
    {
        return Err(ToolError::new(
            ErrorCode::InvalidRef,
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
    if error.code != ErrorCode::NotFound {
        return;
    }
    let Some(error_details) = error.details.as_ref() else {
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

fn invisible_interaction_error(
    inner: &Inner,
    widget: &WidgetRegistryEntry,
    viewport_id: egui::ViewportId,
) -> Option<ToolError> {
    let visible_fraction = widget
        .layout
        .as_ref()
        .map(|layout| layout.visible_fraction)
        .unwrap_or(1.0);
    if widget.visible && visible_fraction > 0.0 {
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
            ErrorCode::InvisibleInteraction,
            format!(
                "Cannot interact with widget {:?}: {reason}; call scroll_into_view() or check clipping",
                widget.id
            ),
        )
        .with_details(json!({
            "reason": "invisible_interaction",
            "hint": "call scroll_into_view() or check clipping",
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

#[mcp_server(initialize_fn = initialize)]
impl DevMcpServer {
    #[cfg(test)]
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        let runtime = Runtime::ensure_for_inner(&inner);
        Self { inner, runtime }
    }

    pub(crate) fn with_runtime(inner: Arc<Inner>, runtime: Arc<Runtime>) -> Self {
        Self { inner, runtime }
    }

    fn resolve_widget_for_pointer(
        &self,
        viewport_id: Option<&str>,
        target: &WidgetRef,
    ) -> ToolResult<(WidgetRegistryEntry, egui::ViewportId)> {
        let (widget, viewport_id) = resolve_widget_and_viewport(&self.inner, viewport_id, target)?;
        if let Some(error) = invisible_interaction_error(&self.inner, &widget, viewport_id) {
            return Err(error.into());
        }
        Ok((widget, viewport_id))
    }

    async fn fixture_apply_internal(
        &self,
        name: &str,
        params: BTreeMap<String, WidgetValue>,
        wait_for_anchors: bool,
        timeout_ms: u64,
    ) -> Result<FixtureApplyOutcome, ToolError> {
        let Some(spec) = self.inner.fixtures.fixture(name) else {
            return Err(ToolError::new(
                ErrorCode::NotFound,
                format!("Unknown fixture: {name}"),
            ));
        };
        let params = spec
            .validate_params(params)
            .map_err(fixture_error_to_tool)?;
        let validated_params = params.as_map().clone();
        let call = FixtureCall {
            name: name.to_string(),
            params,
        };

        self.evaluate_preconditions(&spec, timeout_ms).await?;
        self.inner.clear_all();
        self.inner.dismiss_transient_ui(None);
        let fixture_epoch = self.inner.begin_fixture_epoch();
        let result = match self.inner.start_fixture(call) {
            FixtureExecution::Ready(result) => result,
            FixtureExecution::Queued(receiver) => {
                self.inner.request_repaint();
                spawn_blocking(move || receiver.recv_timeout(Duration::from_millis(timeout_ms)))
                    .await
                    .map_err(|error| {
                        ToolError::new(
                            ErrorCode::Internal,
                            format!("fixture wait task failed: {error}"),
                        )
                    })?
            }
        };
        self.inner.dismiss_transient_ui(None);
        let response = result.map_err(fixture_error_to_tool)?;
        if !wait_for_anchors {
            return Ok(FixtureApplyOutcome {
                params: validated_params,
                values: response.values,
                anchors: Vec::new(),
            });
        }
        let anchors = self
            .evaluate_fixture_anchors(&spec, &response, fixture_epoch, timeout_ms)
            .await?;
        Ok(FixtureApplyOutcome {
            params: validated_params,
            values: response.values,
            anchors,
        })
    }

    fn evaluate_anchor(
        &self,
        anchor: &Anchor,
        fixture_epoch: Option<u64>,
        scroll_samples: &Mutex<HashMap<(String, String), ScrollSample>>,
    ) -> Result<AnchorStatus, ToolError> {
        let target = anchor_target(anchor);
        let widget = match resolve_widget(&self.inner, None, &target) {
            Ok(widget) => widget,
            Err(error) if error.code == ErrorCode::NotFound => {
                return Ok(anchor_status_with_details(
                    anchor,
                    false,
                    "widget not found",
                    error.details,
                    None,
                ));
            }
            Err(error) => return Err(error),
        };

        let current_state = WidgetState::from(&widget);
        let viewport_id = match self
            .inner
            .viewports
            .resolve_viewport_id(Some(widget.viewport_id.clone()))
        {
            Ok(viewport_id) => viewport_id,
            Err(_) => {
                return Ok(anchor_status(
                    anchor,
                    false,
                    "viewport has no captured context yet",
                    Some(current_state),
                ));
            }
        };
        let Some(capture) = self.inner.viewports.capture_snapshot(viewport_id) else {
            return Ok(anchor_status(
                anchor,
                false,
                "viewport has no captured snapshot yet",
                Some(current_state),
            ));
        };
        if let Some(fixture_epoch) = fixture_epoch
            && capture.fixture_epoch < fixture_epoch
        {
            return Ok(anchor_status(
                anchor,
                false,
                format!(
                    "waiting for post-fixture capture (current epoch {}, need {})",
                    capture.fixture_epoch, fixture_epoch
                ),
                Some(current_state),
            ));
        }

        let status = match &anchor.check {
            AnchorCheck::Visible => anchor_status(
                anchor,
                widget.visible,
                if widget.visible {
                    "widget is visible".to_string()
                } else {
                    "widget exists but is not visible".to_string()
                },
                Some(current_state),
            ),
            AnchorCheck::Label(expected) => {
                let actual = widget.label.as_deref().unwrap_or("");
                anchor_status(
                    anchor,
                    widget.label.as_deref() == Some(expected.as_str()),
                    format!("current label: {actual:?}"),
                    Some(current_state),
                )
            }
            AnchorCheck::Value(expected) => {
                let matched = widget.value.as_ref() == Some(expected);
                let actual = widget
                    .value
                    .as_ref()
                    .map(WidgetValue::to_text)
                    .unwrap_or_default();
                anchor_status(
                    anchor,
                    matched,
                    format!("current value: {actual:?}"),
                    Some(current_state),
                )
            }
            AnchorCheck::ScrollReady => {
                let Some(scroll) = scroll_state(&widget) else {
                    return Ok(anchor_status(
                        anchor,
                        false,
                        "widget has no scroll_state yet",
                        Some(current_state),
                    ));
                };
                if scroll.max_offset.y <= 0.0 {
                    return Ok(anchor_status(
                        anchor,
                        false,
                        format!(
                            "scroll max_offset is not ready: ({:.1}, {:.1})",
                            scroll.max_offset.x, scroll.max_offset.y
                        ),
                        Some(current_state),
                    ));
                }
                let current_sample = ScrollSample {
                    frame_count: capture.frame_count,
                    max_offset: scroll.max_offset,
                    stabilized: false,
                };
                if update_scroll_stability(scroll_samples, &widget, current_sample) {
                    anchor_status(
                        anchor,
                        true,
                        format!(
                            "scroll stabilized at max_offset ({:.1}, {:.1})",
                            scroll.max_offset.x, scroll.max_offset.y
                        ),
                        Some(current_state),
                    )
                } else {
                    anchor_status(
                        anchor,
                        false,
                        "waiting for scroll initialization to stabilize".to_string(),
                        Some(current_state),
                    )
                }
            }
            AnchorCheck::ScrollAt { offset, tolerance } => {
                let Some(scroll) = scroll_state(&widget) else {
                    return Ok(anchor_status(
                        anchor,
                        false,
                        "widget has no scroll_state yet",
                        Some(current_state),
                    ));
                };
                if scroll.max_offset.y <= 0.0 {
                    return Ok(anchor_status(
                        anchor,
                        false,
                        format!(
                            "scroll max_offset is not ready: ({:.1}, {:.1})",
                            scroll.max_offset.x, scroll.max_offset.y
                        ),
                        Some(current_state),
                    ));
                }
                let current_sample = ScrollSample {
                    frame_count: capture.frame_count,
                    max_offset: scroll.max_offset,
                    stabilized: false,
                };
                let stable = update_scroll_stability(scroll_samples, &widget, current_sample);
                if !stable {
                    return Ok(anchor_status(
                        anchor,
                        false,
                        "waiting for scroll initialization to stabilize".to_string(),
                        Some(current_state),
                    ));
                }
                let matched = approx_eq_vec2(scroll.offset, *offset, *tolerance);
                anchor_status(
                    anchor,
                    matched,
                    format!(
                        "current offset: ({:.1}, {:.1})",
                        scroll.offset.x, scroll.offset.y
                    ),
                    Some(current_state),
                )
            }
        };
        Ok(status)
    }

    async fn evaluate_preconditions(
        &self,
        spec: &FixtureSpec,
        timeout_ms: u64,
    ) -> Result<(), ToolError> {
        self.evaluate_anchor_list(
            &format!("fixture \"{}\" preconditions", spec.name),
            &spec.name,
            &spec.preconditions,
            None,
            timeout_ms,
        )
        .await
        .map(|_| ())
    }

    async fn evaluate_anchors(
        &self,
        spec: &FixtureSpec,
        anchors: &[Anchor],
        fixture_epoch: u64,
        timeout_ms: u64,
    ) -> Result<Vec<AnchorStatus>, ToolError> {
        self.evaluate_anchor_list(
            &format!("fixture \"{}\"", spec.name),
            &spec.name,
            anchors,
            Some(fixture_epoch),
            timeout_ms,
        )
        .await
    }

    async fn evaluate_fixture_anchors(
        &self,
        spec: &FixtureSpec,
        response: &FixtureResponse,
        fixture_epoch: u64,
        timeout_ms: u64,
    ) -> Result<Vec<AnchorStatus>, ToolError> {
        let mut anchors = spec.anchors.clone();
        anchors.extend(response.anchors.clone());
        self.evaluate_anchors(spec, &anchors, fixture_epoch, timeout_ms)
            .await
    }

    async fn evaluate_anchor_list(
        &self,
        label: &str,
        fixture_name: &str,
        anchors: &[Anchor],
        fixture_epoch: Option<u64>,
        timeout_ms: u64,
    ) -> Result<Vec<AnchorStatus>, ToolError> {
        if anchors.is_empty() {
            return Ok(Vec::new());
        }
        let scroll_samples = Arc::new(Mutex::new(HashMap::new()));
        let (matched, state, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            DEFAULT_POLL_INTERVAL_MS,
            Some(egui::ViewportId::ROOT),
            None,
            || async {
                self.inner.request_repaint_all();
                let mut statuses = Vec::with_capacity(anchors.len());
                for anchor in anchors {
                    statuses.push(self.evaluate_anchor(
                        anchor,
                        fixture_epoch,
                        scroll_samples.as_ref(),
                    )?);
                }
                let matched = statuses.iter().all(|status| status.satisfied);
                Ok::<_, ToolError>((
                    matched,
                    Some(AnchorEvaluationSnapshot {
                        fixture: fixture_name.to_string(),
                        statuses,
                    }),
                ))
            },
        )
        .await?;

        if matched {
            return Ok(state.map(|state| state.statuses).unwrap_or_default());
        }

        let snapshot = state.unwrap_or_else(|| AnchorEvaluationSnapshot {
            fixture: fixture_name.to_string(),
            statuses: Vec::new(),
        });
        let status_lines = snapshot
            .statuses
            .iter()
            .map(format_anchor_status)
            .collect::<Vec<_>>();
        let message = if status_lines.is_empty() {
            format!("{label} timed out after {timeout_ms}ms")
        } else {
            format!(
                "{label} timed out after {timeout_ms}ms\n{}",
                status_lines.join("\n")
            )
        };
        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(message, &observation),
        )
        .with_details(json!({
            "fixture": fixture_name,
            "elapsed_ms": elapsed_ms,
            "statuses": snapshot.statuses,
            "observation": observation,
        })))
    }

    async fn initialize(
        &self,
        _context: &ServerCtx,
        _protocol_version: String,
        _capabilities: ClientCapabilities,
        _client_info: Implementation,
    ) -> tmcp::Result<InitializeResult> {
        let version = env!("CARGO_PKG_VERSION").to_string();
        Ok(InitializeResult::new("eguidev")
            .with_version(version)
            .with_tools(Some(true)))
    }

    /// List viewports and their properties.
    async fn viewports_list(
        &self,
        viewport_id: Option<String>,
    ) -> ToolResult<Vec<ViewportSnapshot>> {
        let mut viewports = self.inner.viewports.viewports_snapshot();
        if let Some(filter) = viewport_id {
            let viewport_id =
                viewport_id_to_string(resolve_viewport_id(&self.inner, Some(filter))?);
            viewports.retain(|entry| entry.viewport_id == viewport_id);
        }
        Ok(viewports)
    }

    /// Inject a pointer move event (positions are in egui points).
    async fn input_pointer_move(&self, viewport_id: Option<String>, pos: Pos2) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        Ok(())
    }

    /// Inject a pointer button press or release (positions are in egui points).
    ///
    /// Common click sequence:
    /// 1. `input_pointer_move` to the target position.
    /// 2. `input_pointer_button` with `pressed: true`.
    /// 3. `input_pointer_button` with `pressed: false`.
    async fn input_pointer_button(
        &self,
        viewport_id: Option<String>,
        pos: Pos2,
        button: PointerButtonName,
        pressed: bool,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let pointer_button = button
            .to_pointer_button()
            .ok_or_else(|| ToolError::new(ErrorCode::InvalidRef, "Invalid pointer button"))?;
        let modifiers = modifiers.unwrap_or_default();
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        self.inner.queue_action(
            viewport_id,
            InputAction::PointerButton {
                pos,
                button: pointer_button,
                pressed,
                modifiers,
            },
        );
        Ok(())
    }

    /// Inject a raw key event.
    async fn input_key(
        &self,
        viewport_id: Option<String>,
        key: String,
        pressed: bool,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let parsed_key = resolve_key_name(&key)
            .ok_or_else(|| ToolError::new(ErrorCode::InvalidRef, format!("Unknown key: {key}")))?;
        let modifiers = modifiers.unwrap_or_default();
        self.inner.queue_action(
            viewport_id,
            InputAction::Key {
                key: parsed_key,
                pressed,
                modifiers,
            },
        );
        Ok(())
    }

    /// Press and release a key (optionally repeating), with modifiers.
    ///
    /// `key_name` is the original user-provided key name string, used to derive the text event
    /// (preserving case for single characters like `"a"` vs `"A"`).
    async fn action_key(
        &self,
        viewport_id: Option<String>,
        key: egui::Key,
        modifiers: Modifiers,
        key_name: &str,
        repeat: Option<u32>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let repeat = repeat.unwrap_or(1);
        if repeat == 0 {
            return Err(ToolError::new(ErrorCode::InvalidRef, "Repeat must be at least 1").into());
        }
        let text = if modifiers.ctrl || modifiers.command || modifiers.alt {
            None
        } else {
            printable_key_text(key_name)
        };
        for _ in 0..repeat {
            self.inner.queue_action(
                viewport_id,
                InputAction::Key {
                    key,
                    pressed: true,
                    modifiers,
                },
            );
            if let Some(text) = text.as_deref() {
                self.inner.queue_action(
                    viewport_id,
                    InputAction::Text {
                        text: text.to_string(),
                    },
                );
            }
            self.inner.queue_action(
                viewport_id,
                InputAction::Key {
                    key,
                    pressed: false,
                    modifiers,
                },
            );
        }
        Ok(())
    }

    async fn focus_widget_for_keyboard(
        &self,
        viewport_id: Option<String>,
        target: &WidgetRef,
        timeout_ms: Option<u64>,
    ) -> Result<(WidgetRegistryEntry, egui::ViewportId), ToolError> {
        let (widget, viewport_id) = self
            .resolve_widget_for_pointer(viewport_id.as_deref(), target)
            .map_err(|error| ToolError::new(ErrorCode::InvalidRef, error.message))?;
        if !widget.enabled {
            return Err(ToolError::new(
                ErrorCode::TargetNotFocusable,
                "Target widget is not focusable",
            ));
        }
        if widget.focused {
            return Ok((widget, viewport_id));
        }
        let click_pos = widget.interact_rect.center();
        queue_primary_click(&self.inner, viewport_id, click_pos);
        let Some(timeout_ms) = timeout_ms else {
            return Ok((widget, viewport_id));
        };
        let viewport_id_str = viewport_id_to_string(viewport_id);
        self.wait_for_widget_state(
            Some(viewport_id_str.clone()),
            target.clone(),
            Some(timeout_ms),
            None,
            |widget| widget.is_some_and(|widget| widget.focused),
        )
        .await
        .map_err(|error| ToolError::new(ErrorCode::FocusNotAcquired, error.message))?;
        match resolve_widget(&self.inner, Some(viewport_id_str.as_str()), target) {
            Ok(focused_widget) if focused_widget.focused => Ok((focused_widget, viewport_id)),
            Ok(_) => Err(ToolError::new(
                ErrorCode::FocusNotAcquired,
                "Widget did not retain focus",
            )),
            Err(error) if error.code == ErrorCode::NotFound => Err(ToolError::new(
                ErrorCode::TargetDetached,
                "Target widget detached while focusing",
            )),
            Err(error) => Err(error),
        }
    }

    /// Inject a text event.
    async fn input_text(&self, viewport_id: Option<String>, text: String) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        self.inner
            .queue_action(viewport_id, InputAction::Text { text });
        Ok(())
    }

    /// Paste text into the focused widget.
    async fn action_paste(&self, viewport_id: Option<String>, text: String) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        self.inner
            .queue_action(viewport_id, InputAction::Paste { text });
        Ok(())
    }

    /// Inject a scroll event (delta is in egui points).
    async fn input_scroll(
        &self,
        viewport_id: Option<String>,
        delta: Vec2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let modifiers = modifiers.unwrap_or_default();
        self.inner
            .queue_action(viewport_id, InputAction::Scroll { delta, modifiers });
        Ok(())
    }

    /// Request OS-level focus for a viewport.
    ///
    /// Raises the window and steals keyboard focus from whatever the user is currently working in.
    ///
    /// **WARNING: Do not use this for general app interaction or automation.** Input injection,
    /// clicks, keyboard events, and all other automation actions work correctly without OS focus.
    /// This function exists solely for testing window focus events themselves (e.g. verifying that
    /// your app responds correctly when it gains or loses focus). Using it unnecessarily disrupts
    /// the user's workflow.
    async fn focus_window(&self, viewport_id: String) -> ToolResult<()> {
        let viewport_id = self
            .inner
            .viewports
            .resolve_viewport_id(Some(viewport_id))
            .map_err(ToolError::from)?;
        self.inner
            .queue_command(viewport_id, egui::ViewportCommand::Focus);
        Ok(())
    }

    /// Dismiss transient egui UI state for a viewport.
    async fn viewport_dismiss_popups(&self, viewport_id: Option<String>) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        self.inner.dismiss_transient_ui(Some(viewport_id));
        Ok(())
    }

    /// Request a viewport size change (sizes are in egui points).
    async fn viewport_set_inner_size(
        &self,
        viewport_id: Option<String>,
        inner_size: Vec2,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        ensure_positive_vec2(inner_size, "inner_size")?;
        self.inner.queue_command(
            viewport_id,
            egui::ViewportCommand::InnerSize(inner_size.into()),
        );
        Ok(())
    }

    /// Configure resize constraints for a viewport.
    async fn viewport_set_resize_options(
        &self,
        viewport_id: Option<String>,
        min_size: Option<Vec2>,
        max_size: Option<Vec2>,
        increments: Option<Vec2>,
        resizable: Option<bool>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        if let Some(min_size) = min_size {
            ensure_positive_vec2(min_size, "min_size")?;
            self.inner.queue_command(
                viewport_id,
                egui::ViewportCommand::MinInnerSize(min_size.into()),
            );
        }
        if let Some(max_size) = max_size {
            ensure_positive_vec2(max_size, "max_size")?;
            self.inner.queue_command(
                viewport_id,
                egui::ViewportCommand::MaxInnerSize(max_size.into()),
            );
        }
        if let Some(increments) = increments {
            ensure_positive_vec2(increments, "increments")?;
            self.inner.queue_command(
                viewport_id,
                egui::ViewportCommand::ResizeIncrements(Some(increments.into())),
            );
        }
        if let Some(resizable) = resizable {
            self.inner
                .queue_command(viewport_id, egui::ViewportCommand::Resizable(resizable));
        }
        Ok(())
    }

    async fn wait_for_frame_count(
        &self,
        count: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> ToolResult<u64> {
        let count = count.unwrap_or(1);
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let start_frame = self.inner.frame_count();
        let target_frame = start_frame + count;

        let (matched, _, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            DEFAULT_POLL_INTERVAL_MS,
            Some(egui::ViewportId::ROOT),
            None,
            || async {
                self.inner.request_repaint_all();
                let current = self.inner.frame_count();
                Ok::<_, ToolError>((current >= target_frame, None::<()>))
            },
        )
        .await?;

        let end_frame = self.inner.frame_count();
        if matched {
            return Ok(end_frame);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for {count} frame(s) after {timeout_ms}ms."),
                &observation,
            ),
        )
        .with_details(wait_timeout_details(
            "frames",
            elapsed_ms,
            None,
            None,
            Some(start_frame),
            Some(end_frame),
            &observation,
        ))
        .into())
    }

    /// Wait until the target viewport has produced a fresh captured snapshot.
    async fn wait_for_capture(
        &self,
        viewport_id: Option<String>,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
        let start_capture = self
            .inner
            .viewports
            .capture_snapshot(viewport_id)
            .map(|snapshot| snapshot.frame_count)
            .unwrap_or(0);

        let (matched, _, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            Some(viewport_id),
            None,
            || async {
                self.inner.request_repaint_of(viewport_id);
                let current = self
                    .inner
                    .viewports
                    .capture_snapshot(viewport_id)
                    .map(|snapshot| snapshot.frame_count)
                    .unwrap_or(0);
                Ok::<_, ToolError>((current > start_capture, None::<()>))
            },
        )
        .await?;

        if matched {
            return Ok(());
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for a fresh capture after {timeout_ms}ms"),
                &observation,
            ),
        )
        .with_details(wait_timeout_details(
            "capture",
            elapsed_ms,
            None,
            viewport_snapshot_for(&self.inner, viewport_id).as_ref(),
            Some(start_capture),
            self.inner
                .viewports
                .capture_snapshot(viewport_id)
                .map(|snapshot| snapshot.frame_count),
            &observation,
        ))
        .into())
    }

    /// Wait until the UI has settled: all input actions and viewport commands are drained
    /// and at least one clean frame has been captured after the last input drain, unless
    /// the target child viewport closed while handling the action.
    async fn wait_for_settle(
        &self,
        viewport_id: Option<String>,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> ToolResult<SettleReport> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
        let start_capture = self
            .inner
            .viewports
            .capture_snapshot(viewport_id)
            .map(|snapshot| snapshot.frame_count)
            .unwrap_or(0);
        let start_frame = self.inner.frame_count();
        let start = Instant::now();

        let (matched, report, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            Some(viewport_id),
            None,
            || async {
                self.inner.request_repaint_all();
                let report = settle_report(
                    &self.inner,
                    viewport_id,
                    start_capture,
                    start_frame,
                    start.elapsed().as_millis() as u64,
                );
                Ok::<_, ToolError>((report.settled, Some(report)))
            },
        )
        .await?;

        let mut report = report.unwrap_or_else(|| {
            settle_report(
                &self.inner,
                viewport_id,
                start_capture,
                start_frame,
                elapsed_ms,
            )
        });
        report.elapsed_ms = elapsed_ms;
        if matched {
            return Ok(report);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(settle_timeout_message(timeout_ms, &report), &observation),
        )
        .with_details(settle_timeout_details(
            elapsed_ms,
            viewport_snapshot_for(&self.inner, viewport_id).as_ref(),
            start_capture,
            self.inner
                .viewports
                .capture_snapshot(viewport_id)
                .map(|snapshot| snapshot.frame_count),
            &observation,
            &report,
        ))
        .into())
    }

    /// Wait until a scroll area has initialized and stabilized across captures.
    async fn wait_for_scroll_ready(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> ToolResult<Option<WidgetRegistryEntry>> {
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
        let last_sample = Mutex::new(None);

        let target_viewport = viewport_id
            .clone()
            .and_then(|viewport_id| {
                self.inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            })
            .or(Some(egui::ViewportId::ROOT));
        let (matched, widget, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            None,
            || async {
                let widget = match resolve_widget(&self.inner, viewport_id.as_deref(), &target) {
                    Ok(widget) => widget,
                    Err(error) if error.code == ErrorCode::NotFound => {
                        return Ok::<_, ToolError>((false, None));
                    }
                    Err(error) => return Err(error),
                };
                let viewport_id = self
                    .inner
                    .viewports
                    .resolve_viewport_id(Some(widget.viewport_id.clone()))
                    .map_err(ToolError::from)?;
                self.inner.request_repaint_of(viewport_id);
                let Some(capture) = self.inner.viewports.capture_snapshot(viewport_id) else {
                    return Ok((false, Some(widget)));
                };
                let Some(scroll) = scroll_state(&widget) else {
                    return Ok((false, Some(widget)));
                };
                if scroll.max_offset.y <= 0.0 {
                    return Ok((false, Some(widget)));
                }
                let current = ScrollSample {
                    frame_count: capture.frame_count,
                    max_offset: scroll.max_offset,
                    stabilized: false,
                };
                let matched = match last_sample
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .replace(current)
                {
                    Some(previous) => {
                        previous.frame_count != current.frame_count
                            && approx_eq_vec2(
                                previous.max_offset,
                                current.max_offset,
                                SCROLL_STABILITY_TOLERANCE,
                            )
                    }
                    None => false,
                };
                Ok((matched, Some(widget)))
            },
        )
        .await?;

        if matched {
            return Ok(widget);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for scroll readiness after {timeout_ms}ms"),
                &observation,
            ),
        )
        .with_details(wait_timeout_details(
            "scroll_ready",
            elapsed_ms,
            widget.as_ref(),
            None,
            None,
            None,
            &observation,
        ))
        .into())
    }

    /// Wait for a widget to match a predicate over its current snapshot.
    async fn wait_for_widget_state<F>(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
        mut predicate: F,
    ) -> ToolResult<Option<WidgetRegistryEntry>>
    where
        F: FnMut(Option<&WidgetRegistryEntry>) -> bool,
    {
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        let target_viewport = viewport_id
            .clone()
            .or_else(|| target.viewport_id.clone())
            .and_then(|viewport_id| {
                self.inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            });
        let (matched, widget, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            None,
            || {
                let result = match resolve_wait_widget(&self.inner, viewport_id.as_deref(), &target)
                {
                    Ok(widget) => {
                        if let Ok(resolved_viewport_id) = self
                            .inner
                            .viewports
                            .resolve_viewport_id(Some(widget.viewport_id.clone()))
                        {
                            if let Some(value) = widget.value.as_ref() {
                                self.inner.clear_widget_value_update_if_matches(
                                    resolved_viewport_id,
                                    &widget.id,
                                    value,
                                );
                            }
                            if let Some(error) = self.inner.expired_widget_value_update_error(
                                resolved_viewport_id,
                                Some(&widget.id),
                            ) {
                                Err(error.into())
                            } else {
                                let matched = predicate(Some(&widget));
                                Ok::<_, ToolError>((matched, Some(widget)))
                            }
                        } else {
                            let matched = predicate(Some(&widget));
                            Ok::<_, ToolError>((matched, Some(widget)))
                        }
                    }
                    Err(error) => {
                        if error.code == ErrorCode::NotFound {
                            let matched = predicate(None);
                            Ok((matched, None))
                        } else {
                            Err(error)
                        }
                    }
                };
                async move { result }
            },
        )
        .await?;

        if matched {
            return Ok(widget);
        }

        let mut details = wait_timeout_details(
            "widget",
            elapsed_ms,
            widget.as_ref(),
            None,
            None,
            None,
            &observation,
        );
        if widget.is_none()
            && let Err(error) = resolve_wait_widget(&self.inner, viewport_id.as_deref(), &target)
        {
            merge_missing_widget_search(&mut details, &error);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for widget predicate after {timeout_ms}ms"),
                &observation,
            ),
        )
        .with_details(details)
        .into())
    }

    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    /// List widgets with optional visibility, role, and id-prefix filters.
    async fn widget_list(
        &self,
        viewport_id: Option<String>,
        include_invisible: Option<bool>,
        role: Option<WidgetRole>,
        id_prefix: Option<String>,
        label: Option<String>,
        label_contains: Option<String>,
    ) -> ToolResult {
        let widgets = collect_widget_list(
            &self.inner,
            viewport_id,
            include_invisible,
            role,
            id_prefix.as_deref(),
            label.as_deref(),
            label_contains.as_deref(),
        )?;
        Ok(CallToolResult::structured(widgets).map_err(|error| {
            ToolError::new(
                ErrorCode::Internal,
                format!("Failed to serialize widget list: {error}"),
            )
        })?)
    }

    /// Get a single widget by id (error if not found or ambiguous).
    fn widget_get_result(
        &self,
        viewport_id: Option<&str>,
        target: &WidgetRef,
    ) -> ToolResult<WidgetGetResult> {
        let widget = resolve_widget(&self.inner, viewport_id, target)?;
        Ok(WidgetGetResult { widget })
    }

    /// Directly set a widget's value without simulating input.
    async fn widget_set_value(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        value: WidgetValue,
    ) -> ToolResult<()> {
        let widget = resolve_widget(&self.inner, viewport_id.as_deref(), &target)?;
        validate_widget_value(&widget, &value)?;
        let WidgetRegistryEntry {
            id: widget_id,
            viewport_id: widget_viewport_id,
            ..
        } = widget;
        let viewport_id = self
            .inner
            .viewports
            .resolve_viewport_id(Some(widget_viewport_id))
            .map_err(ToolError::from)?;
        self.inner
            .queue_widget_value_update(viewport_id, widget_id, value);
        Ok(())
    }

    /// Queue a click on a widget without verifying resulting UI state.
    async fn action_click(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        button: Option<PointerButtonName>,
        modifiers: Option<Modifiers>,
        click_count: Option<u8>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        let modifiers = modifiers.unwrap_or_default();
        let button = button.unwrap_or_default();
        let click_count = click_count.unwrap_or(1);
        if !(1..=3).contains(&click_count) {
            return Err(ToolError::new(
                ErrorCode::InvalidRef,
                "click_count must be between 1 and 3",
            )
            .into());
        }
        let pointer_button = button
            .to_pointer_button()
            .ok_or_else(|| ToolError::new(ErrorCode::InvalidRef, "Invalid pointer button"))?;
        queue_click(
            &self.inner,
            viewport_id,
            pos,
            pointer_button,
            modifiers,
            click_count,
        );
        Ok(())
    }

    /// Hover over a widget without clicking.
    async fn action_hover(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        position: Option<Vec2>,
        duration_ms: Option<u64>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = if let Some(position) = position {
            resolve_relative_pos(widget.interact_rect, position)?
        } else {
            widget.interact_rect.center()
        };
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        let duration_ms = duration_ms.unwrap_or(0);
        if duration_ms > 0 {
            let frames = frames_for_duration(duration_ms);
            wait_for_frames(&self.inner, frames, Instant::now(), duration_ms).await?;
        }
        Ok(())
    }

    /// Type into a widget (optionally clearing first).
    async fn action_type(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        text: String,
        enter: Option<bool>,
        clear: Option<bool>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        let queue_for_next_frame = !widget.focused;
        if queue_for_next_frame {
            queue_primary_click(&self.inner, viewport_id, pos);
        }
        let queue_action = |action| {
            if queue_for_next_frame {
                self.inner
                    .queue_action_with_timing(viewport_id, ActionTiming::Next, action);
            } else {
                self.inner.queue_action(viewport_id, action);
            }
        };
        let queue_key_press = |key, modifiers| {
            queue_action(InputAction::Key {
                key,
                pressed: true,
                modifiers,
            });
            queue_action(InputAction::Key {
                key,
                pressed: false,
                modifiers,
            });
        };
        let clear = clear.unwrap_or(false);
        if clear {
            let modifiers = Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            };
            queue_key_press(egui::Key::A, modifiers);
            queue_key_press(egui::Key::Backspace, Modifiers::default());
        }
        queue_action(InputAction::Text { text });
        let enter = enter.unwrap_or(false);
        if enter {
            queue_key_press(egui::Key::Enter, Modifiers::default());
        }
        Ok(())
    }

    /// Focus a widget by clicking on it.
    async fn action_focus(&self, viewport_id: Option<String>, target: WidgetRef) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        queue_primary_click(&self.inner, viewport_id, pos);
        Ok(())
    }

    /// Drag from a widget to an absolute position (points).
    async fn action_drag(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        to: Pos2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let start = widget.interact_rect.center();
        queue_drag(
            &self.inner,
            viewport_id,
            start,
            to,
            modifiers.unwrap_or_default(),
        );
        Ok(())
    }

    /// Drag within a widget using relative coordinates (0..1).
    async fn action_drag_relative(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        from: Option<Vec2>,
        to: Vec2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let start_relative = from.unwrap_or(Vec2 { x: 0.5, y: 0.5 });
        let start = resolve_relative_pos(widget.interact_rect, start_relative)?;
        let end = resolve_relative_pos(widget.interact_rect, to)?;
        queue_drag(
            &self.inner,
            viewport_id,
            start,
            end,
            modifiers.unwrap_or_default(),
        );
        Ok(())
    }

    /// Drag from one widget to another's center.
    async fn action_drag_to_widget(
        &self,
        viewport_id: Option<String>,
        from: WidgetRef,
        to: WidgetRef,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let viewport_id = viewport_id.as_deref();
        let (from_widget, from_viewport) = self.resolve_widget_for_pointer(viewport_id, &from)?;
        let (to_widget, to_viewport) = self.resolve_widget_for_pointer(viewport_id, &to)?;
        if from_viewport != to_viewport {
            return Err(ToolError::new(
                ErrorCode::InvalidRef,
                "Drag endpoints must be in the same viewport",
            )
            .into());
        }
        let viewport_id = from_viewport;
        let start = from_widget.interact_rect.center();
        let end = to_widget.interact_rect.center();
        queue_drag(
            &self.inner,
            viewport_id,
            start,
            end,
            modifiers.unwrap_or_default(),
        );
        Ok(())
    }

    /// Scroll a scroll area.
    async fn action_scroll(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        delta: Vec2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        let mut applied_override = false;
        if widget.role == WidgetRole::ScrollArea {
            let current = widget
                .role_state
                .as_ref()
                .and_then(RoleState::scroll_state)
                .map(|scroll| scroll.offset.into())
                .unwrap_or(egui::Vec2::ZERO);
            let delta_vec: egui::Vec2 = delta.into();
            let mut target = current - delta_vec;
            target.x = target.x.max(0.0);
            target.y = target.y.max(0.0);
            self.inner
                .set_scroll_override(viewport_id, widget.native_id, target);
            applied_override = true;
        }
        if !applied_override {
            self.inner.queue_action(
                viewport_id,
                InputAction::Scroll {
                    delta,
                    modifiers: modifiers.unwrap_or_default(),
                },
            );
        }
        Ok(())
    }

    /// Scroll a scroll area to an absolute offset or alignment.
    async fn action_scroll_to(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        offset: Option<Vec2>,
        align: Option<ScrollAlign>,
    ) -> ToolResult<Vec2> {
        let (widget, viewport_id) =
            resolve_widget_and_viewport(&self.inner, viewport_id.as_deref(), &target)?;
        if widget.role != WidgetRole::ScrollArea {
            return Err(ToolError::new(
                ErrorCode::InvalidRef,
                "Target widget is not a scroll area",
            )
            .into());
        }
        let scroll = widget
            .role_state
            .as_ref()
            .and_then(RoleState::scroll_state)
            .ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidRef,
                    "Scroll metadata unavailable; render the scroll area before scrolling",
                )
            })?;
        if offset.is_some() && align.is_some() {
            return Err(ToolError::new(
                ErrorCode::InvalidRef,
                "Provide either offset or align, not both",
            )
            .into());
        }
        let mut target_offset = if let Some(offset) = offset {
            offset
        } else if let Some(align) = align {
            let y = match align {
                ScrollAlign::Top => 0.0,
                ScrollAlign::Center => scroll.max_offset.y * 0.5,
                ScrollAlign::Bottom => scroll.max_offset.y,
            };
            Vec2 {
                x: scroll.offset.x,
                y,
            }
        } else {
            return Err(
                ToolError::new(ErrorCode::InvalidRef, "Provide either offset or align").into(),
            );
        };
        target_offset.x = target_offset.x.clamp(0.0, scroll.max_offset.x);
        target_offset.y = target_offset.y.clamp(0.0, scroll.max_offset.y);
        let pos = widget.interact_rect.center();
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        self.inner
            .set_scroll_override(viewport_id, widget.native_id, target_offset.into());
        Ok(target_offset)
    }

    /// Scroll ancestor scroll areas so the target widget becomes visible.
    async fn action_scroll_into_view(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            resolve_widget_and_viewport(&self.inner, viewport_id.as_deref(), &target)?;
        let widgets = self.inner.widgets.widget_list(viewport_id);
        let by_id: HashMap<&str, &WidgetRegistryEntry> = widgets
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let mut target_widget = widget;
        let mut parent_id = target_widget.parent_id.clone();

        while let Some(parent_key) = parent_id {
            let Some(parent) = by_id.get(parent_key.as_str()) else {
                break;
            };
            if parent.role == WidgetRole::ScrollArea
                && let Some(offset) = scroll_area_target_offset(parent, &target_widget)
            {
                self.inner
                    .set_scroll_override(viewport_id, parent.native_id, offset.into());
            }
            target_widget = (*parent).clone();
            parent_id = parent.parent_id.clone();
        }

        Ok(())
    }

    /// Analyze layout for common issues.
    async fn check_layout(
        &self,
        viewport_id: Option<String>,
        root: Option<WidgetRef>,
    ) -> ToolResult<Vec<LayoutIssue>> {
        let viewport_id = self.resolve_scope_viewport(viewport_id, root.as_ref())?;
        let mut widgets = self.inner.widgets.widget_list(viewport_id);
        let viewport_id_str = viewport_id_to_string(viewport_id);
        if let Some(root) = root.as_ref() {
            let root = resolve_widget(&self.inner, Some(viewport_id_str.as_str()), root)?;
            widgets = collect_subtree(&widgets, &root);
        }

        let viewport_rect = viewport_rect(&self.inner, viewport_id);
        let mut issues = Vec::new();
        issues.extend(check_zero_size(&widgets));
        issues.extend(check_clipping(&widgets, viewport_rect));
        issues.extend(check_overflow(&widgets, viewport_rect));
        issues.extend(check_overlaps(&widgets, viewport_rect));
        if let Some(viewport_rect) = viewport_rect {
            issues.extend(check_offscreen(&widgets, viewport_rect));
        }
        if let Some(ctx) = self.inner.context_for(viewport_id) {
            issues.extend(check_text_truncation(&ctx, &widgets)?);
        }
        Ok(issues)
    }

    /// Measure text for a text-containing widget.
    async fn text_measure(&self, target: WidgetRef) -> ToolResult<TextMeasure> {
        let (widget, viewport_id) = resolve_widget_and_viewport(&self.inner, None, &target)?;
        let ctx = self.inner.context_for(viewport_id).ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidRef,
                "Context not available for text measurement",
            )
        })?;
        Ok(measure_text(&ctx, &widget)?)
    }

    fn widget_at_point_result(
        &self,
        pos: Pos2,
        all_layers: Option<bool>,
        viewport_id: Option<&str>,
    ) -> ToolResult<WidgetAtPointResult> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id.map(str::to_string))?;
        let widgets = self.inner.widgets.widget_list(viewport_id);
        let mut hits: Vec<WidgetRegistryEntry> = widgets
            .iter()
            .filter(|widget| point_in_rect(pos, widget.interact_rect))
            .cloned()
            .collect();
        let all_layers = all_layers.unwrap_or(false);
        hits.reverse(); // topmost first
        if !all_layers {
            hits.truncate(1);
        }
        Ok(WidgetAtPointResult { widgets: hits })
    }

    /// Show a persistent highlight on a widget or rectangle.
    async fn show_highlight(
        &self,
        viewport_id: Option<String>,
        target: Option<WidgetRef>,
        rect: Option<Rect>,
        color: String,
    ) -> ToolResult<OverlayHighlightResult> {
        let (rect, key) = if let Some(ref target) = target {
            let widget = resolve_widget(&self.inner, viewport_id.as_deref(), target)?;
            (widget.interact_rect, format!("widget:{}", widget.id))
        } else if let Some(rect) = rect {
            let key = format!(
                "rect:{},{},{},{}",
                rect.min.x, rect.min.y, rect.max.x, rect.max.y
            );
            (rect, key)
        } else {
            return Err(ToolError::new(ErrorCode::InvalidRef, "Missing rect or target").into());
        };
        let color = parse_color(&color).ok_or_else(|| {
            ToolError::new(ErrorCode::InvalidRef, format!("Invalid color: {color}"))
        })?;
        self.inner.set_overlay(
            key,
            OverlayEntry {
                rect: egui::Rect::from(rect),
                color,
                stroke_width: 2.0,
            },
        );
        Ok(OverlayHighlightResult { rect })
    }

    /// Hide highlights. If a target widget is given, removes just that widget's
    /// highlight. Otherwise clears all highlights.
    async fn hide_highlight(
        &self,
        viewport_id: Option<String>,
        target: Option<WidgetRef>,
    ) -> ToolResult<()> {
        if let Some(ref target) = target {
            let widget = resolve_widget(&self.inner, viewport_id.as_deref(), target)?;
            self.inner.remove_overlay(&format!("widget:{}", widget.id));
        } else {
            self.inner.clear_overlays();
        }
        Ok(())
    }

    /// Enable the persistent debug overlay with a fresh configuration.
    async fn show_debug_overlay(
        &self,
        _viewport_id: Option<String>,
        mode: Option<OverlayDebugModeName>,
        scope: Option<WidgetRef>,
        options: Option<OverlayDebugOptionsInput>,
    ) -> ToolResult<()> {
        let mut config = OverlayDebugConfig {
            enabled: true,
            mode: mode.map(Into::into).unwrap_or(OverlayDebugMode::Bounds),
            scope,
            options: OverlayDebugOptions::default(),
        };
        if let Some(input) = options {
            apply_overlay_debug_options(&mut config.options, input)?;
        }
        self.inner.set_overlay_debug_config(config);
        Ok(())
    }

    /// Disable the persistent debug overlay.
    async fn hide_debug_overlay(&self) -> ToolResult<()> {
        let config = OverlayDebugConfig::default();
        self.inner.set_overlay_debug_config(config);
        Ok(())
    }

    /// Capture a viewport once and sample exact RGBA pixels at logical positions.
    async fn viewport_sample_pixels(
        &self,
        viewport_id: Option<String>,
        positions: Vec<Pos2>,
    ) -> ToolResult<Vec<PixelSample>> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let pixels_per_point = self
            .inner
            .viewports
            .input_snapshot(viewport_id)
            .map(|snapshot| snapshot.pixels_per_point)
            .unwrap_or(1.0);
        let image = capture_screenshot_image(&self.inner, &self.runtime, viewport_id).await?;
        sample_color_image(&image, pixels_per_point, &positions).map_err(Into::into)
    }

    #[tool(defaults)]
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

    fn resolve_scope_viewport(
        &self,
        viewport_id: Option<String>,
        scope: Option<&WidgetRef>,
    ) -> Result<egui::ViewportId, ToolError> {
        if let Some(scope) = scope {
            let widget = resolve_widget(&self.inner, viewport_id.as_deref(), scope)?;
            return resolve_viewport_id(&self.inner, Some(widget.viewport_id));
        }
        resolve_viewport_id(&self.inner, viewport_id)
    }

    #[tool]
    /// Report automation frame health for the attached app.
    async fn health(&self) -> ToolResult<CallToolResult> {
        Ok(
            CallToolResult::structured(app_health_report(&self.inner)).map_err(|error| {
                ToolError::new(
                    ErrorCode::Internal,
                    format!("Failed to serialize health report: {error}"),
                )
            })?,
        )
    }

    #[tool]
    /// Return the checked-in Luau definitions for the full scripting API.
    async fn script_api(&self) -> ToolResult<CallToolResult> {
        let preludes = self.inner.script_preludes.preludes();
        Ok(CallToolResult::new().with_text_content(script_definitions_with_preludes(&preludes)))
    }

    #[tool(defaults)]
    /// Apply an app-defined fixture without waiting for readiness.
    async fn fixture_apply(
        &self,
        name: String,
        params: Option<BTreeMap<String, WidgetValue>>,
    ) -> ToolResult<FixtureApplyOutcome> {
        Ok(self
            .fixture_apply_internal(
                &name,
                params.unwrap_or_default(),
                false,
                DEFAULT_WAIT_TIMEOUT_MS,
            )
            .await?)
    }

    #[tool(defaults)]
    /// Navigate to an app-defined fixture by name and wait for readiness anchors.
    async fn fixture(
        &self,
        name: String,
        params: Option<BTreeMap<String, WidgetValue>>,
        timeout_ms: Option<u64>,
    ) -> ToolResult<FixtureApplyOutcome> {
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        Ok(self
            .fixture_apply_internal(&name, params.unwrap_or_default(), true, timeout_ms)
            .await?)
    }
}

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_WAIT_TIMEOUT: Duration = Duration::from_millis(500);

enum ScreenshotWaitOutcome {
    Ready,
    TryNativeFallback,
}

fn resolve_screenshot_viewport(
    inner: &Inner,
    viewport_id: Option<String>,
) -> Result<egui::ViewportId, ToolError> {
    if let Some(viewport_id) = viewport_id {
        return resolve_viewport_id(inner, Some(viewport_id));
    }
    Ok(egui::ViewportId::ROOT)
}

async fn capture_screenshot(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
    kind: ScreenshotKind,
) -> Result<String, ToolError> {
    let state = capture_screenshot_state(inner, runtime, viewport_id, kind).await?;
    build_screenshot_data(&state)
}

async fn capture_screenshot_image(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
) -> Result<Arc<egui::ColorImage>, ToolError> {
    let state =
        capture_screenshot_state(inner, runtime, viewport_id, ScreenshotKind::Viewport).await?;
    build_screenshot_image(&state)
}

async fn capture_screenshot_state(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
    kind: ScreenshotKind,
) -> Result<ScreenshotState, ToolError> {
    // Best-effort wake-up before sending the screenshot command. Some idle windows won't
    // produce a frame until a command is queued, so only treat this as fatal if context
    // capture is not ready yet.
    let event_loop_ready = ensure_event_loop_active(inner, runtime, viewport_id).await;
    let has_snapshot = inner.viewports.has_viewport_snapshot(viewport_id);
    if !inner.has_context() {
        if let Err(error) = event_loop_ready {
            return Err(error);
        }
        return Err(ToolError::new(
            ErrorCode::InvalidRef,
            "Viewport context not ready for screenshots",
        )
        .with_details(screenshot_error_details(inner, runtime, viewport_id)));
    }
    if !has_snapshot {
        event_loop_ready?;
        return Err(
            ToolError::new(ErrorCode::InvalidRef, "Viewport not ready for screenshots")
                .with_details(screenshot_error_details(inner, runtime, viewport_id)),
        );
    }

    let start_frame = inner
        .viewports
        .capture_snapshot(viewport_id)
        .map(|snapshot| snapshot.frame_count)
        .unwrap_or(0);
    let request_id = inner.next_request_id();
    let kind_snapshot = kind.clone();
    runtime.insert_screenshot(request_id, ScreenshotState::pending(kind));
    inner.queue_command(
        viewport_id,
        egui::ViewportCommand::Screenshot(egui::UserData::new(request_id)),
    );
    runtime.record_screenshot_request(inner, request_id, viewport_id, &kind_snapshot);
    inner.request_repaint_of(viewport_id);
    await_screenshot(
        inner,
        runtime,
        request_id,
        viewport_id,
        &kind_snapshot,
        start_frame,
    )
    .await
}

async fn ensure_event_loop_active(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
) -> Result<(), ToolError> {
    let initial_frame = inner
        .viewports
        .capture_snapshot(viewport_id)
        .map(|snapshot| snapshot.frame_count)
        .unwrap_or(0);

    // Wait for at least one frame to process. Use a short poll interval with
    // periodic repaint requests so we recover when the event loop stalls.
    let frame_wait = async {
        loop {
            let current_frame = inner
                .viewports
                .capture_snapshot(viewport_id)
                .map(|snapshot| snapshot.frame_count)
                .unwrap_or(0);
            if current_frame > initial_frame {
                return;
            }
            let notified = runtime.frame_notify().notified();
            inner.request_repaint_of(viewport_id);
            let poll = Duration::from_millis(DEFAULT_POLL_INTERVAL_MS);
            drop(timeout(poll, notified).await);
        }
    };

    if timeout(FRAME_WAIT_TIMEOUT, frame_wait).await.is_err() {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "Window event loop not responding. The window may be minimized or hidden. \
For eframe apps, prefer Renderer::Glow for automation; Wgpu backends can stall idle frames.",
        )
        .with_details(screenshot_error_details(inner, runtime, viewport_id)));
    }

    Ok(())
}

async fn await_screenshot(
    inner: &Inner,
    runtime: &Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
    start_frame: u64,
) -> Result<ScreenshotState, ToolError> {
    let notify = match runtime.screenshot_state(request_id) {
        Some(state) => state.notify(),
        None => {
            return Err(
                ToolError::new(ErrorCode::InvalidRef, "Unknown request id").with_details(
                    screenshot_request_details(inner, runtime, request_id, viewport_id, kind),
                ),
            );
        }
    };

    let wait_loop = async {
        let mut last_command_frame = start_frame.saturating_add(1);
        let outcome = loop {
            if let Some(state) = runtime.screenshot_state(request_id) {
                if state.is_ready() {
                    break ScreenshotWaitOutcome::Ready;
                }
            } else {
                return Err(ToolError::new(ErrorCode::InvalidRef, "Unknown request id")
                    .with_details(screenshot_request_details(
                        inner,
                        runtime,
                        request_id,
                        viewport_id,
                        kind,
                    )));
            }

            let current_frame = inner
                .viewports
                .capture_snapshot(viewport_id)
                .map(|snapshot| snapshot.frame_count)
                .unwrap_or(0);
            if should_try_native_screenshot_fallback(viewport_id, current_frame, start_frame) {
                break ScreenshotWaitOutcome::TryNativeFallback;
            }
            if current_frame > last_command_frame {
                inner.queue_command(
                    viewport_id,
                    egui::ViewportCommand::Screenshot(egui::UserData::new(request_id)),
                );
                last_command_frame = current_frame;
            }
            if current_frame > start_frame {
                inner.request_repaint_of(viewport_id);
            }
            tokio::select! {
                _ = notify.notified() => {}
                _ = runtime.frame_notify().notified() => {}
                _ = sleep(Duration::from_millis(DEFAULT_POLL_INTERVAL_MS)) => {
                    inner.request_repaint_of(viewport_id);
                }
            }
        };
        Ok::<_, ToolError>(outcome)
    };

    match timeout(SCREENSHOT_TIMEOUT, wait_loop).await {
        Ok(Ok(ScreenshotWaitOutcome::Ready)) => {}
        Ok(Ok(ScreenshotWaitOutcome::TryNativeFallback)) => {
            runtime.take_screenshot(request_id);
            let end_frame = inner.frame_count();
            runtime.log_screenshot(
                inner,
                format!(
                    "native fallback after fresh child frame request_id={request_id} viewport={} \
                     start_frame={start_frame} end_frame={end_frame}",
                    viewport_id_to_string(viewport_id),
                ),
            );
            return native_screenshot_fallback(inner, viewport_id, kind)
                .inspect(|_| {
                    runtime.log_screenshot(
                        inner,
                        format!(
                            "native fallback succeeded request_id={request_id} viewport={}",
                            viewport_id_to_string(viewport_id),
                        ),
                    );
                })
                .map_err(|fallback_error| {
                    runtime.log_screenshot(
                        inner,
                        format!(
                            "native fallback failed request_id={request_id} viewport={} error={}",
                            viewport_id_to_string(viewport_id),
                            fallback_error,
                        ),
                    );
                    ToolError::new(
                        ErrorCode::Internal,
                        screenshot_timeout_message(viewport_id, &fallback_error),
                    )
                    .with_details(screenshot_timeout_details(
                        &ScreenshotTimeoutContext {
                            inner,
                            runtime,
                            request_id,
                            viewport_id,
                            kind,
                            start_frame,
                            end_frame,
                            fallback_error: &fallback_error,
                        },
                    ))
                });
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            runtime.take_screenshot(request_id);
            let end_frame = inner.frame_count();
            runtime.log_screenshot(
                inner,
                format!(
                    "timeout request_id={request_id} viewport={} start_frame={start_frame} \
                 end_frame={end_frame}",
                    viewport_id_to_string(viewport_id),
                ),
            );
            match native_screenshot_fallback(inner, viewport_id, kind) {
                Ok(state) => {
                    runtime.log_screenshot(
                        inner,
                        format!(
                            "native fallback succeeded request_id={request_id} viewport={}",
                            viewport_id_to_string(viewport_id),
                        ),
                    );
                    return Ok(state);
                }
                Err(fallback_error) => {
                    runtime.log_screenshot(
                        inner,
                        format!(
                            "native fallback failed request_id={request_id} viewport={} error={}",
                            viewport_id_to_string(viewport_id),
                            fallback_error,
                        ),
                    );
                    return Err(ToolError::new(
                        ErrorCode::Internal,
                        screenshot_timeout_message(viewport_id, &fallback_error),
                    )
                    .with_details(screenshot_timeout_details(
                        &ScreenshotTimeoutContext {
                            inner,
                            runtime,
                            request_id,
                            viewport_id,
                            kind,
                            start_frame,
                            end_frame,
                            fallback_error: &fallback_error,
                        },
                    )));
                }
            }
        }
    }

    runtime.take_screenshot(request_id).ok_or_else(|| {
        ToolError::new(ErrorCode::InvalidRef, "Unknown request id").with_details(
            screenshot_request_details_with_frames(
                inner,
                runtime,
                request_id,
                viewport_id,
                kind,
                start_frame,
                inner.frame_count(),
            ),
        )
    })
}

fn should_try_native_screenshot_fallback(
    viewport_id: egui::ViewportId,
    current_frame: u64,
    start_frame: u64,
) -> bool {
    native_fallback_applies(viewport_id) && current_frame > start_frame
}

fn build_screenshot_data(state: &ScreenshotState) -> Result<String, ToolError> {
    let image = build_screenshot_image(state)?;
    encode_jpeg(&image)
}

fn build_screenshot_image(state: &ScreenshotState) -> Result<Arc<egui::ColorImage>, ToolError> {
    let Some(image) = state.image() else {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "Screenshot missing image",
        ));
    };
    let image = match &state.kind {
        ScreenshotKind::Viewport => image,
        ScreenshotKind::Widget {
            rect,
            pixels_per_point,
        } => crop_image(&image, *rect, *pixels_per_point)?,
    };
    Ok(image)
}

#[cfg(target_os = "macos")]
fn native_screenshot_fallback(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
) -> Result<ScreenshotState, String> {
    if viewport_id == egui::ViewportId::ROOT {
        return Err("native fallback is only used for child viewports".to_string());
    }
    let snapshot = viewport_snapshot_for(inner, viewport_id)
        .ok_or_else(|| "viewport snapshot was unavailable".to_string())?;
    let title = snapshot
        .title
        .as_deref()
        .ok_or_else(|| "viewport has no title to match a native window".to_string())?;
    let window_number = window_number_for_title(title)?;
    let mut state = ScreenshotState::pending(kind.clone());
    let image = crop_native_capture_to_viewport(capture_window_image(window_number)?, &snapshot)?;
    state.mark_ready(Arc::new(image));
    Ok(state)
}

#[cfg(not(target_os = "macos"))]
fn native_screenshot_fallback(
    _inner: &Inner,
    _viewport_id: egui::ViewportId,
    _kind: &ScreenshotKind,
) -> Result<ScreenshotState, String> {
    Err("native fallback is only available on macOS".to_string())
}

fn screenshot_error_details(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
) -> Value {
    let snapshots = inner.viewports.viewports_snapshot();
    let known_viewports = snapshots
        .iter()
        .map(|snapshot| snapshot.viewport_id.clone())
        .collect::<Vec<_>>();
    serde_json::json!({
        "viewport_id": viewport_id_to_string(viewport_id),
        "has_context": inner.has_context(),
        "known_viewports": known_viewports,
        "frame_count": inner.frame_count(),
        "has_snapshot": inner.viewports.has_viewport_snapshot(viewport_id),
        "debug": runtime.screenshot_debug_snapshot(inner),
    })
}

fn screenshot_request_details(
    inner: &Inner,
    runtime: &Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
) -> Value {
    screenshot_request_details_with_frames(inner, runtime, request_id, viewport_id, kind, 0, 0)
}

struct ScreenshotTimeoutContext<'a> {
    inner: &'a Inner,
    runtime: &'a Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &'a ScreenshotKind,
    start_frame: u64,
    end_frame: u64,
    fallback_error: &'a str,
}

fn screenshot_timeout_details(context: &ScreenshotTimeoutContext<'_>) -> Value {
    let mut details = screenshot_request_details_with_frames(
        context.inner,
        context.runtime,
        context.request_id,
        context.viewport_id,
        context.kind,
        context.start_frame,
        context.end_frame,
    );
    if let Some(map) = details.as_object_mut() {
        map.insert(
            "native_fallback".to_string(),
            json!({
                "attempted": native_fallback_applies(context.viewport_id),
                "error": context.fallback_error,
            }),
        );
    }
    details
}

fn screenshot_timeout_message(viewport_id: egui::ViewportId, fallback_error: &str) -> String {
    let base = "Screenshot timed out waiting for a screenshot event. The screenshot command may \
                not have reached the viewport or the frame did not render.";
    if native_fallback_applies(viewport_id) {
        return format!(
            "{base} A macOS native fallback was attempted for this child viewport and failed: \
             {fallback_error}."
        );
    }
    format!(
        "{base} Native screenshot fallback is only available for child viewports on macOS: \
         {fallback_error}."
    )
}

fn native_fallback_applies(viewport_id: egui::ViewportId) -> bool {
    cfg!(target_os = "macos") && viewport_id != egui::ViewportId::ROOT
}

fn crop_native_capture_to_viewport(
    image: egui::ColorImage,
    snapshot: &ViewportSnapshot,
) -> Result<egui::ColorImage, String> {
    let target_width = scaled_viewport_pixels(snapshot.inner_size.x, snapshot.pixels_per_point)?;
    let target_height = scaled_viewport_pixels(snapshot.inner_size.y, snapshot.pixels_per_point)?;
    if target_width == 0 || target_height == 0 {
        return Err("viewport content size is empty".to_string());
    }
    if image.size == [target_width, target_height] {
        return Ok(image);
    }
    if image.size[0] < target_width || image.size[1] < target_height {
        return Err(format!(
            "native capture {}x{} is smaller than viewport content {}x{}",
            image.size[0], image.size[1], target_width, target_height
        ));
    }

    let x0 = (image.size[0] - target_width) / 2;
    let y0 = image.size[1] - target_height;
    let mut pixels = Vec::with_capacity(target_width * target_height);
    for y in y0..(y0 + target_height) {
        let row_start = y * image.size[0] + x0;
        pixels.extend_from_slice(&image.pixels[row_start..row_start + target_width]);
    }
    Ok(egui::ColorImage {
        size: [target_width, target_height],
        source_size: egui::Vec2::new(target_width as f32, target_height as f32),
        pixels,
    })
}

fn scaled_viewport_pixels(size: f32, pixels_per_point: f32) -> Result<usize, String> {
    let pixels = size * pixels_per_point;
    if !pixels.is_finite() || pixels < 0.0 {
        return Err("viewport content size is not finite".to_string());
    }
    Ok(pixels.round() as usize)
}

fn screenshot_request_details_with_frames(
    inner: &Inner,
    runtime: &Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
    start_frame: u64,
    end_frame: u64,
) -> Value {
    let kind_details = match kind {
        ScreenshotKind::Viewport => serde_json::json!({ "kind": "viewport" }),
        ScreenshotKind::Widget {
            rect,
            pixels_per_point,
        } => serde_json::json!({
            "kind": "widget",
            "rect": rect,
            "pixels_per_point": pixels_per_point,
        }),
    };
    serde_json::json!({
        "request_id": request_id,
        "viewport_id": viewport_id_to_string(viewport_id),
        "kind": kind_details,
        "start_frame": start_frame,
        "end_frame": end_frame,
        "debug": runtime.screenshot_debug_snapshot(inner),
    })
}

fn encode_jpeg(image: &egui::ColorImage) -> Result<String, ToolError> {
    const JPEG_QUALITY: u8 = 80;
    let width = image.size[0] as u32;
    let height = image.size[1] as u32;
    let mut bytes = Vec::with_capacity((width * height * 3) as usize);
    for pixel in &image.pixels {
        let [r, g, b, a] = pixel.to_array();
        if a == 255 {
            bytes.extend_from_slice(&[r, g, b]);
        } else {
            let alpha = u16::from(a);
            let inv = 255_u16.saturating_sub(alpha);
            let r = ((u16::from(r) * alpha) + 255 * inv) / 255;
            let g = ((u16::from(g) * alpha) + 255 * inv) / 255;
            let b = ((u16::from(b) * alpha) + 255 * inv) / 255;
            bytes.extend_from_slice(&[r as u8, g as u8, b as u8]);
        }
    }
    let mut jpeg_data = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut jpeg_data, JPEG_QUALITY);
    image::ImageEncoder::write_image(
        encoder,
        &bytes,
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|error| ToolError::new(ErrorCode::Internal, format!("JPEG encode failed: {error}")))?;
    Ok(STANDARD.encode(jpeg_data))
}

fn crop_image(
    image: &egui::ColorImage,
    rect: Rect,
    pixels_per_point: f32,
) -> Result<Arc<egui::ColorImage>, ToolError> {
    let width = image.size[0] as i32;
    let height = image.size[1] as i32;
    let min_x = (rect.min.x * pixels_per_point).round() as i32;
    let min_y = (rect.min.y * pixels_per_point).round() as i32;
    let max_x = (rect.max.x * pixels_per_point).round() as i32;
    let max_y = (rect.max.y * pixels_per_point).round() as i32;
    let x0 = min_x.clamp(0, width);
    let y0 = min_y.clamp(0, height);
    let x1 = max_x.clamp(0, width);
    let y1 = max_y.clamp(0, height);
    let crop_width = (x1 - x0).max(0) as usize;
    let crop_height = (y1 - y0).max(0) as usize;
    if crop_width == 0 || crop_height == 0 {
        return Err(ToolError::new(
            ErrorCode::InvalidRef,
            "Widget rect is empty",
        ));
    }
    let mut pixels = Vec::with_capacity(crop_width * crop_height);
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = (y as usize) * image.size[0] + x as usize;
            if let Some(pixel) = image.pixels.get(idx) {
                pixels.push(*pixel);
            }
        }
    }
    Ok(Arc::new(egui::ColorImage {
        size: [crop_width, crop_height],
        source_size: egui::Vec2::new(crop_width as f32, crop_height as f32),
        pixels,
    }))
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering as AtomicOrdering},
        },
        time::{Duration, Instant},
    };

    use eguidev::{
        DevMcp, FixtureCall, FixtureError, FixtureResponse, FixtureResult, FixtureSpec, FrameGuard,
        ScriptPrelude,
    };
    use serde_json::{Value, json};
    use tmcp::schema::ContentBlock;
    use tokio::{task::yield_now, time::sleep};

    use super::*;
    use crate::{
        actions::InputAction,
        fixtures::FixtureHandler,
        overlay::{OverlayDebugConfig, OverlayDebugMode},
        registry::{Inner, viewport_id_to_string},
        runtime::{Runtime, attach_for_tests},
        tools::types::LayoutIssueKind,
        types::{
            Modifiers, Pos2, Rect, RoleState, Vec2, WidgetLayout, WidgetRange, WidgetRef,
            WidgetRegistryEntry, WidgetRole, WidgetValue,
        },
        viewports::InputSnapshot,
        widget_registry::{WidgetMeta, record_widget},
    };

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
        let base_modifiers = raw_input.modifiers;
        let mut current_modifiers = base_modifiers;
        let mut force_focus = false;
        for action in &actions {
            if let InputAction::Key {
                pressed, modifiers, ..
            } = action
            {
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
        raw_input.modifiers = current_modifiers;
        if force_focus {
            raw_input.focused = true;
        }
        for action in actions {
            action.apply(raw_input);
        }
    }

    fn widget_ref_id(id: &str) -> WidgetRef {
        WidgetRef {
            id: Some(id.to_string()),
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
        drop(ctx.run_ui(raw_input, |ctx| {
            let _guard = FrameGuard::new(devmcp, ctx);
        }));
    }

    fn viewport_snapshot_with_inner_size(inner_size: Vec2) -> ViewportSnapshot {
        ViewportSnapshot {
            viewport_id: viewport_id_to_string(egui::ViewportId::ROOT),
            name: None,
            inner_size,
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
    async fn tools_list_contains_script_and_fixture_handoff_tools() {
        use tmcp::{ServerHandler, schema::Cursor, testutils::TestServerContext};

        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let ctx = TestServerContext::new();
        let tools = server
            .list_tools(ctx.ctx(), None::<Cursor>)
            .await
            .expect("list tools")
            .tools;
        let mut names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "fixture",
                "fixture_apply",
                "health",
                "script_api",
                "script_eval"
            ]
        );
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

        assert_eq!(error.code, ErrorCode::InvalidRef);
        assert!(error.message.contains("outside"));
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
            .script_eval("log(\"hello\")\nreturn 1 + 1".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"].as_i64(), Some(2));
        assert_eq!(json["logs"][0], "hello");
    }

    #[tokio::test]
    async fn script_eval_omitted_args_default_to_empty_table() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval("return next(args) == nil".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], true);
    }

    #[tokio::test]
    async fn script_eval_exposes_scalar_args() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .script_eval(
                "return { name = args.name, count = args.count, ratio = args.ratio, enabled = args.enabled }"
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
        assert_eq!(json["success"], true);
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
                r#"return root():widget_list({ id_prefix = "missing" })"#.to_string(),
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
                r#"local widgets = root():widget_list({ id_prefix = "status" })
local exact = root():widget_list({ label = "Ready state" })
local contains = root():widget_list({ label_contains = "state" })
return {
    count = #widgets,
    id = widgets[1].id,
    viewport = widgets[1].viewport_id,
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
                r#"local found = widget("panel")
local maybe = try_widget("panel")
local missing = try_widget("missing")
return {
    id = found.id,
    viewport = found.viewport_id,
    maybe_viewport = maybe ~= nil and maybe.viewport_id or nil,
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
            .script_eval(r#"return widget("2a")"#.to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "tool");
        assert_eq!(json["error"]["code"], "ambiguous");
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
                r#"configure({ timeout_ms = 5, poll_interval_ms = 1 })
widget("basic.submt")"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
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
                r#"local state = expect("status", { label = "Ready", visible = true })
expect_absent("missing")
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
            FixtureSpec::new("mutate", "Mutate widget.").anchor("status"),
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
                r#"local before = capture()
fixture_raw("mutate")
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
            FixtureSpec::new("mutate-hidden", "Mutate hidden widget.").anchor("hidden.status"),
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
                r#"local before = capture()
fixture_raw("mutate-hidden")
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
    async fn script_eval_runs_app_prelude_and_script_api_lists_it() {
        let devmcp = attach_for_tests(
            DevMcp::new()
                .script_prelude(ScriptPrelude {
                    namespace: "demo".to_string(),
                    source: "function demo.answer() return 42 end".to_string(),
                    declarations: "declare demo: { answer: () -> number }".to_string(),
                })
                .expect("script prelude"),
        );
        let inner = devmcp.inner_arc().expect("attached inner");
        let server = DevMcpServer::new(inner);

        let api = server.script_api().await.expect("script_api");
        assert!(
            api.text()
                .expect("text")
                .contains("declare demo: { answer: () -> number }")
        );

        let result = server
            .script_eval("return demo.answer()".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], 42);
    }

    #[tokio::test]
    async fn script_eval_reports_app_prelude_failures() {
        let devmcp = attach_for_tests(
            DevMcp::new()
                .script_prelude(ScriptPrelude {
                    namespace: "demo".to_string(),
                    source: "error(\"boom\")".to_string(),
                    declarations: "declare demo: {}".to_string(),
                })
                .expect("script prelude"),
        );
        let inner = devmcp.inner_arc().expect("attached inner");
        let server = DevMcpServer::new(inner);

        let result = server
            .script_eval("return true".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "prelude");
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("message")
                .contains("app prelude demo")
        );
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
                r#"local state = root():state()
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
            drop(ctx.run_ui(raw_input, |_| {}));
        }
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"local exact = viewport({ title = "Secondary Lookup" })
local exact_wins = viewport({ title = "Secondary Lookup", title_contains = "Root" })
local contains = viewport({ title_contains = "Secondary" })
local missing = viewport({ title = "Missing" })
return {
    exact = exact ~= nil and exact.id or nil,
    exact_wins = exact_wins ~= nil and exact_wins.id or nil,
    contains = contains ~= nil and contains.id or nil,
    missing = missing == nil,
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
        drop(ctx.run_ui(raw_input, |_| {}));
        inner
            .viewports
            .name_viewport(secondary, "secondary".to_string());
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"local named = viewport({ name = "secondary" })
local focused = viewport({ focused = true })
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
        drop(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"return viewport({ title = "Duplicate Lookup" })"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);

        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "runtime");
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("message")
                .contains("multiple viewports matched title")
        );
    }

    #[tokio::test]
    async fn script_eval_viewport_errors_on_duplicate_names() {
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
        drop(ctx.run_ui(raw_input, |_| {}));
        inner
            .viewports
            .name_viewport(first, "duplicate".to_string());
        inner
            .viewports
            .name_viewport(second, "duplicate".to_string());
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.viewports.update_viewports(&ctx);
        record_test_snapshot(&inner, egui::ViewportId::ROOT);

        let result = server
            .script_eval(
                r#"return viewport({ name = "duplicate" })"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);

        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["code"], ErrorCode::ViewportNameFault.as_str());
        assert_eq!(json["error"]["details"]["reason"], "viewport_name_faults");
        let duplicate_viewports = json["error"]["details"]["duplicate_names"][0]["viewports"]
            .as_array()
            .expect("duplicate viewport contexts");
        assert_eq!(duplicate_viewports.len(), 2);
        assert!(
            duplicate_viewports.iter().any(|viewport| {
                viewport.get("title").and_then(Value::as_str) == Some("Duplicate First")
                    && viewport.get("id").and_then(Value::as_str).is_some()
                    && viewport.get("name").is_some()
                    && viewport.get("parent_viewport_id").is_some()
                    && viewport.get("focused").is_some()
            }),
            "duplicate fault should include first viewport context"
        );
        assert!(
            duplicate_viewports.iter().any(|viewport| {
                viewport.get("title").and_then(Value::as_str) == Some("Duplicate Second")
                    && viewport.get("id").and_then(Value::as_str).is_some()
                    && viewport.get("name").is_some()
                    && viewport.get("parent_viewport_id").is_some()
                    && viewport.get("focused").is_some()
            }),
            "duplicate fault should include second viewport context"
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
                r#"local state = root():widget_get("status"):state()
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
for _, widget in ipairs(root():widget_list()) do
    local state = widget:state()
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
                r#"return root():widget_get("choice"):state()"#.to_string(),
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
                r#"local viewport = root()
return viewport:widget_list({ id_prefix = "missing" }),
    viewport:widget_list({ id_prefix = "also_missing" })"#
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
                "return { arg = args.unit, literal = 1.0 }".to_string(),
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
                r#"configure(nil)
return root():widget_list(nil)"#
                    .to_string(),
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
                        return root():widget_get("missing")
                    end)
                    return {
                        ok = ok,
                        err_type = type(err),
                        err = tostring(err),
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
        assert_eq!(json["value"]["err_type"], "string");
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
                r#"configure({ timeout_ms = 50 }) return root():wait_for_widget("status", function(w) return w.label == "Ready" end)"#
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
                r#"configure({ timeout_ms = 50 }) return root():wait_for_widget("choice", function(widget) return widget.value == 2 end)"#
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
                r#"configure({ timeout_ms = 30 }) root():wait_for_widget_absent("missing") return true"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
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
                r#"configure({ timeout_ms = 50 }) return root():wait_for_widget_visible("status")"#
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
                r#"configure({ timeout_ms = 100, poll_interval_ms = 1 })
local widget = root():widget_get("status")
return widget:wait_for_visible()"#
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
                r#"configure({ timeout_ms = 100, poll_interval_ms = 1 }) return root():wait_for_widget_visible("status")"#
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
                r#"configure({ timeout_ms = 30, poll_interval_ms = 1 }) return root():wait_for_widget_visible("status")"#
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert_eq!(json["error"]["details"]["kind"], "widget_visible");
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
                r#"configure({ timeout_ms = 30 }) return root():wait_for_widget("status", function(w) return w.label == "Never" end)"#
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

        let started = Instant::now();
        let result = server
            .script_eval(
                r#"configure({ timeout_ms = 5000, poll_interval_ms = 250 }) return root():wait_for_widget("status", function(w) return w.label == "Never" end)"#
                    .to_string(),
                Some(50),
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert!(started.elapsed() < Duration::from_millis(500));
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

        let started = Instant::now();
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
        assert!(started.elapsed() < Duration::from_millis(500));
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
                r#"root():widget_get("slider"):drag_relative(
                    { x = 0.8, y = 0.5 },
                    { x = 0.2, y = 0.5 },
                    { settle = false }
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
                r#"local widgets = root():widget_at_point({ x = 5, y = 5 }, true)
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
            drop(ctx.run_ui(raw_input, |_| {}));
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
                    "for _, vp in ipairs(viewports()) do if vp.id == \"{viewport_selector}\" then return vp:widget_get(\"overlay\"):show_debug_overlay() end end"
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
        assert_eq!(scope.id.as_deref(), Some("overlay"));
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
                r#"root():paste("hello", { settle = "fast" })"#.to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "type_error");
        assert_eq!(json["error"]["message"], "settle must be a boolean");
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
            drop(ctx.run_ui(raw_input, |_| {}));
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
                    "configure({{ timeout_ms = 10, poll_interval_ms = 1 }})\nfor _, vp in ipairs(viewports()) do if vp.id == \"{viewport_selector}\" then return vp:widget_get(\"status\"):click() end end"
                ),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert_eq!(json["error"]["details"]["kind"], "settle");
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
            .script_eval("assert(false, \"nope\")".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "assertion");
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
            .script_eval("assert_widget_exists(\"status\")".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["assertions"][0]["passed"], true);
        assert_eq!(json["assertions"][0]["message"], "widget exists");
        assert_eq!(json["assertions"][0]["location"], "script.luau:1");
    }

    #[tokio::test]
    async fn fixture_apply_applies_handler_without_waiting_for_a_new_frame() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("test_fixture", "Test fixture.").anchor("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        drop(ctx.run_ui(raw_input, |_| {}));
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
    async fn fixture_apply_waits_for_preconditions_before_handler() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("delayed", "Delayed fixture.")
                .precondition_value("ready", WidgetValue::Bool(true))
                .anchor("status"),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Relaxed);
            fixture_ok()
        });

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
        drop(ctx.run_ui(raw_input, |_| {}));
        inner.capture_context(viewport_id, &ctx);
        inner.viewports.capture_input_snapshot(
            &ctx,
            inner.fixture_epoch(),
            inner.frame_count() + 1,
        );

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
            drop(ctx_for_update.run_ui(raw_input, |_| {}));
            inner_for_update.capture_context(viewport_id, &ctx_for_update);
            inner_for_update.viewports.capture_input_snapshot(
                &ctx_for_update,
                inner_for_update.fixture_epoch(),
                inner_for_update.frame_count() + 1,
            );
        });

        let server = DevMcpServer::new(Arc::clone(&inner));
        server
            .fixture_apply("delayed".to_string(), None)
            .await
            .expect("fixture_apply result");
        assert!(handler_called.load(AtomicOrdering::Relaxed));
    }

    #[tokio::test]
    async fn fixture_precondition_timeout_does_not_apply_handler() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("blocked", "Blocked fixture.")
                .precondition_value("ready", WidgetValue::Bool(true))
                .anchor("status"),
        ]);
        let handler_called = Arc::new(AtomicBool::new(false));
        let handler_called_for_fixture = Arc::clone(&handler_called);
        set_runtime_fixture_handler(&inner, move |_call| {
            handler_called_for_fixture.store(true, AtomicOrdering::Relaxed);
            fixture_ok()
        });

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture("blocked".to_string(), None, Some(20)).await;
        assert!(result.is_err());
        assert!(!handler_called.load(AtomicOrdering::Relaxed));
    }

    #[tokio::test]
    async fn fixture_returns_error_when_handler_fails() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("broken", "Broken fixture.").anchor("status"),
        ]);
        inner
            .fixtures
            .set_handler(FixtureHandler::Runtime(Arc::new(|_call| {
                Err(FixtureError::new("fixture_failed", "fixture failed"))
            })))
            .expect("fixture handler");
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture("broken".to_string(), None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fixture_returns_error_when_no_handler_registered() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("no_handler", "No handler fixture.").anchor("status"),
        ]);
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture("no_handler".to_string(), None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn script_eval_fixture_raw_succeeds_without_a_new_frame() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("slow", "Slow fixture.").anchor("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval("fixture_raw(\"slow\")".to_string(), Some(20), None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
    }

    #[tokio::test]
    async fn script_eval_returns_sorted_fixtures() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("zeta", "Last fixture.").anchor("status"),
            FixtureSpec::new("alpha", "First fixture.").anchor("status"),
        ]);
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                r#"local catalog = fixtures()
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
            .script_eval("return wait_for_frames(0)".to_string(), None, None)
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], true);
        assert_eq!(json["value"], 0);
    }

    #[tokio::test]
    async fn script_eval_fixture_timeout_reports_stale_snapshot_diagnostics() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("stale", "Stale fixture.").anchor("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        drop(ctx.run_ui(raw_input, |_| {}));
        let entry = make_entry("status", 1, WidgetRole::Label);
        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner, &ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .script_eval(
                "configure({ timeout_ms = 20, poll_interval_ms = 1 }) fixture(\"stale\")"
                    .to_string(),
                None,
                None,
            )
            .await
            .expect("script eval");
        let json = parse_script_eval_json(&result);
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["type"], "timeout");
        assert_eq!(json["error"]["details"]["fixture"], "stale");
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("timeout message")
                .contains("status visible"),
            "timeout should include per-anchor diagnostics"
        );
        assert!(
            json["error"]["details"]["statuses"][0]["detail"]
                .as_str()
                .expect("status detail")
                .contains("post-fixture capture"),
            "timeout details should explain the stale capture"
        );
    }

    #[tokio::test]
    async fn fixture_timeout_reports_zero_frame_observation_once() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("blocked", "Blocked fixture.").anchor("status"),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());
        let server = DevMcpServer::new(Arc::clone(&inner));

        let result = server
            .script_eval(
                "configure({ timeout_ms = 20, poll_interval_ms = 1 }) fixture(\"blocked\")"
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
        drop(ctx.run_ui(raw_input, |_| {}));

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
            FixtureSpec::new("multi", "Multi viewport fixture.").anchor_in("status", secondary),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());

        let root_ctx = egui::Context::default();
        let mut root_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        root_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        root_input.viewports.insert(secondary, Default::default());
        drop(root_ctx.run_ui(root_input, |_| {}));
        inner.capture_context(egui::ViewportId::ROOT, &root_ctx);
        inner.viewports.update_viewports(&root_ctx);
        capture_test_frame(&inner, &root_ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_for_capture = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(20)).await;
            inner_for_capture.widgets.clear_registry(secondary);
            let mut entry = make_entry("status", 1, WidgetRole::Label);
            entry.viewport_id = viewport_id_to_string(secondary);
            inner_for_capture.widgets.record_widget(secondary, entry);
            inner_for_capture.widgets.finalize_registry(secondary);
            record_test_snapshot(&inner_for_capture, secondary);
        });

        server
            .fixture("multi".to_string(), None, Some(200))
            .await
            .expect("fixture");
    }

    #[tokio::test]
    async fn fixture_waits_for_scroll_anchor_stability() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("scroll", "Scroll fixture.").anchor_scroll_at(
                "scroll",
                Vec2 { x: 0.0, y: 300.0 },
                1.0,
            ),
        ]);
        set_runtime_fixture_handler(&inner, |_call| fixture_ok());

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        drop(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);

        let server = DevMcpServer::new(Arc::clone(&inner));
        let inner_for_capture = Arc::clone(&inner);
        tokio::spawn(async move {
            let capture_ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            drop(capture_ctx.run_ui(raw_input, |_| {}));
            sleep(Duration::from_millis(20)).await;
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

        server
            .fixture("scroll".to_string(), None, Some(250))
            .await
            .expect("fixture");
    }

    #[tokio::test]
    async fn wait_for_capture_waits_for_new_snapshot() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        drop(ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner, &ctx);

        let inner_for_capture = Arc::clone(&inner);
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let capture_ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                viewport_id: egui::ViewportId::ROOT,
                ..Default::default()
            };
            drop(capture_ctx.run_ui(raw_input, |_| {}));
            capture_test_frame(&inner_for_capture, &capture_ctx);
        });

        server
            .wait_for_capture(None, Some(500), Some(1))
            .await
            .expect("wait_for_capture");
    }

    #[tokio::test]
    async fn wait_for_scroll_ready_waits_for_stable_scroll_state() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        drop(ctx.run_ui(raw_input, |_| {}));
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
            drop(capture_ctx.run_ui(raw_input, |_| {}));
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
            .wait_for_scroll_ready(None, widget_ref_id("scroll"), Some(500), Some(1))
            .await
            .expect("wait_for_scroll_ready")
            .expect("widget snapshot");
        let scroll = scroll_state(&result).expect("scroll metadata");
        assert_eq!(scroll.offset.y, 150.0);
    }

    #[tokio::test]
    async fn fixture_rejects_unregistered_names() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("known", "Known fixture.").anchor("status"),
        ]);
        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server.fixture("unknown".to_string(), None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fixtures_are_sorted_for_scripts() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("zeta", "Last fixture.").anchor("status"),
            FixtureSpec::new("alpha", "First fixture.").anchor("status"),
        ]);
        let specs = inner.fixtures.fixtures_sorted();
        let specs: Vec<_> = specs.into_iter().map(|fixture| fixture.name).collect();
        assert_eq!(specs, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[tokio::test]
    async fn fixture_clears_transient_automation_state_on_apply_boundaries() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("reset", "Reset fixture.").anchor("status"),
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
        drop(ctx.run_ui(raw_input, |_| {}));
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
            ActionTiming::Next,
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
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "slider".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRole::Slider,
                        value: Some(WidgetValue::Float(f64::from(value))),
                        visible,
                        ..Default::default()
                    },
                );
            });
        });
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

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
            });
        });

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
            });
        });

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
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_multiline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "notes".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRole::TextEdit,
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
        });
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
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.text_edit_multiline(&mut text);
            });
        });

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.text_edit_multiline(&mut text);
            });
        });

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

        assert_eq!(error.code, ErrorCode::InvisibleInteraction.as_str());
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

        assert_eq!(error.code, ErrorCode::InvisibleInteraction.as_str());
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
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_multiline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "notes".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRole::TextEdit,
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
        });
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
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_multiline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "notes".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRole::TextEdit,
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
        });
        inner.widgets.finalize_registry(viewport_id);

        let focused = server
            .widget_get_result(None, &widget_ref_id("notes"))
            .expect("widget get")
            .widget
            .focused;
        assert!(focused);
    }

    #[tokio::test]
    async fn viewport_set_inner_size_rejects_invalid_sizes() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(inner);
        let result = server
            .viewport_set_resize_options(None, Some(Vec2 { x: -1.0, y: 480.0 }), None, None, None)
            .await
            .expect_err("invalid min_inner_size");
        assert_eq!(result.code, ErrorCode::InvalidRef.as_str());
        assert!(result.message.contains("min_size"));
    }

    #[tokio::test]
    async fn viewport_set_inner_size_queues_commands() {
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
            .viewport_set_inner_size(None, inner_size)
            .await
            .expect("viewport set inner size");
        server
            .viewport_set_resize_options(
                None,
                Some(min_inner_size),
                Some(max_inner_size),
                Some(resize_increments),
                Some(true),
            )
            .await
            .expect("viewport set resize options");

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
        let _output = ctx.run_ui(raw_input, |ctx| {
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
                                role: WidgetRole::Button,
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
                                role: WidgetRole::Button,
                                visible: hidden_flag,
                                ..Default::default()
                            },
                        );
                    });
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        });

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
    async fn widget_get_missing_returns_error() {
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

        let server = DevMcpServer::new(inner);
        let result = server
            .widget_get_result(None, &widget_ref_id("basic.submt"))
            .expect_err("missing widget");
        assert_eq!(result.code, ErrorCode::NotFound.as_str());
        assert!(result.message.contains("basic.submt"));
        let suggestions = result
            .structured
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("details"))
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
    async fn widget_get_missing_reports_exact_match_in_other_viewport() {
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
        drop(ctx.run_ui(raw_input, |_| {}));
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

        let server = DevMcpServer::new(inner);
        let result = server
            .widget_get_result(None, &widget_ref_id("viewports.unwired.value"))
            .expect_err("root-scoped lookup should miss secondary widget");
        assert_eq!(result.code, ErrorCode::NotFound.as_str());
        let search = result
            .structured
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("details"))
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
    async fn widget_get_round_trips_widget_list_id() {
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

        let fetched = server
            .widget_get_result(None, &widget_ref_id(&entry.id))
            .expect("widget get");
        assert_eq!(fetched.widget.id, "big");
    }

    #[tokio::test]
    async fn widget_get_fails_on_duplicate_explicit_ids() {
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

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .widget_get_result(None, &widget_ref_id("dup"))
            .expect_err("duplicate explicit ids should block automation");
        assert_eq!(result.code, ErrorCode::DuplicateWidgetId.as_str());
        let duplicate_ids = result
            .structured
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("details"))
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
    async fn widget_get_generated_duplicates_remain_ambiguous() {
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

        let server = DevMcpServer::new(Arc::clone(&inner));
        let result = server
            .widget_get_result(None, &widget_ref_id("dup"))
            .expect_err("ambiguous id");
        assert_eq!(result.code, ErrorCode::Ambiguous.as_str());
        assert_ne!(result.code, ErrorCode::DuplicateWidgetId.as_str());
    }

    #[tokio::test]
    async fn widget_get_accepts_generated_hex_id() {
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

        let fetched = server
            .widget_get_result(None, &widget_ref_id(&entry.id))
            .expect("widget get by generated id");
        assert_eq!(fetched.widget.id, entry.id);
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
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.checkbox(&mut checked, "Enabled");
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "toggle".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRole::Checkbox,
                        label: Some("Enabled".to_string()),
                        value: Some(WidgetValue::Bool(checked)),
                        visible,
                        ..Default::default()
                    },
                );
            });
        });
        inner.widgets.finalize_registry(viewport_id);

        let widget = inner
            .widgets
            .widget_list(viewport_id)
            .into_iter()
            .find(|entry| entry.id == "toggle")
            .expect("toggle widget");
        let pos = widget.interact_rect.center();

        server
            .input_pointer_move(None, pos)
            .await
            .expect("pointer move");
        server
            .input_pointer_button(None, pos, PointerButtonName::Primary, true, None)
            .await
            .expect("pointer down");
        server
            .input_pointer_button(None, pos, PointerButtonName::Primary, false, None)
            .await
            .expect("pointer up");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        let _output = ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.checkbox(&mut checked, "Enabled");
            });
        });

        assert!(checked);
    }

    #[tokio::test]
    async fn input_tools_queue_actions_and_commands() {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;
        let pos = Pos2 { x: 12.0, y: 24.0 };
        let modifiers = Modifiers {
            shift: true,
            ..Default::default()
        };

        server
            .input_pointer_move(None, pos)
            .await
            .expect("pointer move");
        server
            .input_pointer_button(None, pos, PointerButtonName::Primary, true, Some(modifiers))
            .await
            .expect("pointer button");
        server
            .input_key(None, "Enter".to_string(), true, Some(modifiers))
            .await
            .expect("key input");
        server
            .input_text(None, "Hello".to_string())
            .await
            .expect("text input");
        server
            .input_scroll(None, Vec2 { x: 1.0, y: -2.0 }, Some(modifiers))
            .await
            .expect("scroll input");
        server
            .focus_window("root".to_string())
            .await
            .expect("focus window");

        let queued_actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
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
            .hide_highlight(None, None)
            .await
            .expect("hide_highlight");
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
        let _output = ctx.run_ui(raw_input, |ctx| {
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
        });

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
        let _output = ctx.run_ui(raw_input, |ctx| {
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
                        role: WidgetRole::Checkbox,
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
                        role: WidgetRole::Slider,
                        value: Some(WidgetValue::Float(f64::from(intensity))),
                        visible: slider_visible,
                        ..Default::default()
                    },
                );
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        });

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
            .wait_for_widget_state(None, widget_ref_id("status"), Some(50), Some(1), |widget| {
                widget.and_then(|widget| widget.value.as_ref())
                    == Some(&WidgetValue::Text("Ready".to_string()))
            })
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
        drop(ctx.run_ui(raw_input, |_| {}));
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
        drop(ctx.run_ui(raw_input, |_| {}));
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
        drop(ctx.run_ui(raw_input, |_| {}));
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
        drop(ctx.run_ui(raw_input, |_| {}));
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
        drop(ctx.run_ui(raw_input, |_| {}));
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
            drop(capture_ctx.run_ui(raw_input, |_| {}));
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
                |_| false,
            )
            .await
            .expect_err("unconsumed override should fail");

        assert_eq!(error.code, ErrorCode::OverrideNotConsumed.as_str());
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
                    local vp = root()
                    local root = vp:widget_get("root")
                    local child = vp:widget_get("child")
                    local child_ids = {}
                    for _, widget in ipairs(root:children()) do
                        table.insert(child_ids, widget.id)
                    end
                    local grand_ids = {}
                    for _, widget in ipairs(child:children()) do
                        table.insert(grand_ids, widget.id)
                    end
                    local grand_parent = vp:widget_get("grand"):parent()
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
                    local widget = root():widget_get("status")
                    local state = widget:state()
                    local waited = widget:wait_for(function(current)
                        return current ~= nil
                            and current.focused
                            and current.value ~= nil
                            and current.value == "Ready"
                    end, { timeout_ms = 20, poll_interval_ms = 1 })
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
        let _output = ctx.run_ui(raw_input, |ctx| {
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
                        role: WidgetRole::Label,
                        label: Some(text.clone()),
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        });

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
        let _output = ctx.run_ui(raw_input, |ctx| {
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
                        role: WidgetRole::Label,
                        label: Some(text.clone()),
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
            });
            inner.widgets.finalize_registry(ctx.viewport_id());
        });

        let result = server
            .script_eval(
                r#"return root():widget_get("label"):text_measure().text"#.to_string(),
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
            drop(ctx.run_ui(raw_input, |_| {}));
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
                id: Some("label.secondary".to_string()),
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
            drop(ctx.run_ui(raw_input, |_| {}));
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
    async fn show_hide_debug_overlay() {
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
            .hide_debug_overlay()
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
                let output = ctx.run_ui(raw_input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.text_edit_singleline(text);
                        let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                        record_widget(
                            &inner.widgets,
                            "input".to_string(),
                            &response,
                            WidgetMeta {
                                role: WidgetRole::TextEdit,
                                value: Some(WidgetValue::Text(text.clone())),
                                visible,
                                ..Default::default()
                            },
                        );
                        response.lost_focus()
                    });
                });
                inner.widgets.finalize_registry(viewport_id);
                output
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
        let _output = ctx.run_ui(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.text_edit_singleline(&mut text);
                let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                record_widget(
                    &inner.widgets,
                    "input".to_string(),
                    &response,
                    WidgetMeta {
                        role: WidgetRole::TextEdit,
                        value: Some(WidgetValue::Text(text.clone())),
                        visible,
                        ..Default::default()
                    },
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    enter_detected = true;
                }
            });
        });
        inner.widgets.finalize_registry(viewport_id);

        assert!(
            enter_detected,
            "Enter key should cause lost_focus + key_pressed(Enter)"
        );
    }
}
