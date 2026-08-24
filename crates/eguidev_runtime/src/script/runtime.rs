#![allow(clippy::needless_pass_by_value, clippy::result_large_err)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use eguidev::AutomationOptions;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tmcp::ToolResult;
use tokio::{task::spawn_blocking, time::timeout};

use super::{
    super::{
        DEFAULT_POLL_INTERVAL_MS, DEFAULT_WAIT_TIMEOUT_MS, DevMcpServer, ErrorCode,
        MAX_SAMPLE_GRID_COUNT, OverlayDebugOptionsInput, SCROLL_STABILITY_TOLERANCE, ToolError,
        capture_screenshot, collect_widget_list, interaction_ready, parse_key_combo,
        resolve_screenshot_viewport, resolve_widget_and_viewport, viewport_snapshot_for,
        wait_timeout_details, wait_timeout_message,
    },
    parse::{
        map_has_any, map_value, parse_modifiers, parse_optional_bool, parse_optional_f32,
        parse_optional_string, parse_optional_u8, parse_optional_u32, parse_optional_u64,
        parse_optional_u64_val, parse_optional_vec2, parse_overlay_mode, parse_pointer_button,
        parse_pos2, parse_scroll_align, parse_vec2, parse_widget_ref, parse_widget_role,
        widget_value_from_dynamic,
    },
    types::{
        FixtureApplication, ImageCapture, ScriptAssertion, ScriptErrorInfo, ScriptImageKind,
        ScriptLocation, ScriptPosition, ScriptResult,
    },
};
use crate::{
    diagnostics::DiagnosticExecution,
    dump::{DumpOptions, build_tree_dump, dump_text},
    egui_diagnostics::{DiagnosticSelection, EguiDiagnosticBatch},
    registry::{Inner, viewport_id_to_string},
    runtime::Runtime,
    screenshots::ScreenshotKind,
    types::{
        Modifiers, RawInputEvent, Rect, ResizeOptions, Vec2, WidgetRef, WidgetRegistryEntry,
        WidgetState, WidgetValue,
    },
    viewports::ViewportSnapshot,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScriptCapture {
    frame: u64,
    #[serde(rename = "__widgets")]
    widgets: Vec<ScriptCapturedWidget>,
    #[serde(rename = "__viewports")]
    viewports: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScriptCapturedWidget {
    id: String,
    viewport_id: String,
    state: WidgetState,
}

#[derive(Debug, Serialize)]
struct WidgetDelta {
    id: String,
    viewport_id: String,
    change: &'static str,
    fields: Vec<&'static str>,
    before: Option<WidgetState>,
    after: Option<WidgetState>,
}

#[derive(Debug, Serialize)]
struct TreeDiff {
    changes: Vec<WidgetDelta>,
    viewports_added: Vec<String>,
    viewports_removed: Vec<String>,
}

#[derive(Clone, Debug)]
struct DiffOptions {
    move_epsilon: f32,
    include_invisible: bool,
    id_prefix: Option<String>,
}

pub(super) struct ScriptRuntime {
    pub(super) server: DevMcpServer,
    logs: Mutex<Vec<String>>,
    assertions: Mutex<Vec<ScriptAssertion>>,
    fixtures: Mutex<Vec<FixtureApplication>>,
    images: Mutex<Vec<ImageCapture>>,
    image_counter: AtomicUsize,
    source_name: String,
    started_at: Instant,
    deadline: Option<Instant>,
    script_timeout_ms: u64,
    config_timeout_ms: Mutex<Option<u64>>,
    config_poll_interval_ms: Mutex<Option<u64>>,
    config_settle: Mutex<Option<bool>>,
    egui_diagnostic_start: u64,
    dismissed_egui_diagnostics: Mutex<BTreeSet<u64>>,
    targeted_viewports: Mutex<BTreeSet<String>>,
    saved_automation_options: AutomationOptions,
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

fn resolve_viewport_id(
    inner: &Inner,
    viewport_id: Option<String>,
) -> Result<egui::ViewportId, ToolError> {
    inner
        .viewports
        .resolve_viewport_id(viewport_id)
        .map_err(Into::into)
}

fn dump_options(
    _pos: ScriptPosition,
    options: Option<&Map<String, Value>>,
) -> Result<DumpOptions, String> {
    let Some(options) = options else {
        return Ok(DumpOptions::default());
    };
    serde_json::from_value(Value::Object(options.clone()))
        .map_err(|error| format!("invalid dump options: {error}"))
}

fn fixture_params(params: Option<Value>) -> Result<BTreeMap<String, WidgetValue>, String> {
    let Some(params) = params else {
        return Ok(BTreeMap::new());
    };
    if params.is_null() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_value(params).map_err(|error| format!("invalid fixture params: {error}"))
}

impl ScriptRuntime {
    #[cfg(test)]
    pub(super) fn new(
        inner: Arc<Inner>,
        runtime: Arc<Runtime>,
        source_name: String,
        timeout_ms: u64,
    ) -> Self {
        let started_at = Instant::now();
        Self::new_started_at(inner, runtime, source_name, timeout_ms, started_at)
    }

    pub(super) fn new_started_at(
        inner: Arc<Inner>,
        runtime: Arc<Runtime>,
        source_name: String,
        timeout_ms: u64,
        started_at: Instant,
    ) -> Self {
        let deadline = started_at.checked_add(Duration::from_millis(timeout_ms));
        let egui_diagnostic_start = runtime.egui_diagnostics().tail_sequence();
        let saved_automation_options = inner.automation_options();
        Self {
            server: DevMcpServer::with_runtime(inner, runtime),
            logs: Mutex::new(Vec::new()),
            assertions: Mutex::new(Vec::new()),
            fixtures: Mutex::new(Vec::new()),
            images: Mutex::new(Vec::new()),
            image_counter: AtomicUsize::new(0),
            source_name,
            started_at,
            deadline,
            script_timeout_ms: timeout_ms,
            config_timeout_ms: Mutex::new(None),
            config_poll_interval_ms: Mutex::new(None),
            config_settle: Mutex::new(None),
            egui_diagnostic_start,
            dismissed_egui_diagnostics: Mutex::new(BTreeSet::new()),
            targeted_viewports: Mutex::new(BTreeSet::new()),
            saved_automation_options,
        }
    }

    pub(super) fn restore_automation_options(&self) {
        self.server
            .inner
            .set_automation_options(self.saved_automation_options);
    }

    pub(super) fn record_targeted_viewport(&self, viewport_id: impl Into<String>) {
        self.targeted_viewports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(viewport_id.into());
    }

    fn record_viewport_selector(&self, selector: Option<String>) {
        if let Ok(viewport_id) = resolve_viewport_id(&self.server.inner, selector) {
            self.record_targeted_viewport(viewport_id_to_string(viewport_id));
        }
    }

    fn record_fixture_viewports(&self, name: &str) {
        if let Some(spec) = self.server.inner.fixtures.fixture(name) {
            for ready in spec.preconditions.iter().chain(&spec.ready) {
                self.record_viewport_selector(ready.viewport_id.clone());
            }
        }
    }

    fn targeted_viewports(&self) -> BTreeSet<String> {
        self.targeted_viewports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn diagnostic_selection(&self, viewport_id: Option<&str>) -> DiagnosticSelection {
        let dismissed = self
            .dismissed_egui_diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.server.runtime.egui_diagnostics().select(
            self.egui_diagnostic_start,
            &dismissed,
            viewport_id,
        )
    }

    fn dismiss_diagnostic_sequences(&self, sequences: impl IntoIterator<Item = u64>) {
        self.dismissed_egui_diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(sequences);
    }

    fn diagnostic_barrier_error(&self, pending: &[String]) -> ScriptErrorInfo {
        let completions = pending
            .iter()
            .map(|viewport_id| {
                let completion = self
                    .server
                    .runtime
                    .egui_diagnostics()
                    .output_completion(viewport_id);
                (
                    viewport_id.clone(),
                    completion.map_or(Value::Null, |completion| {
                        serde_json::json!({
                            "sequence": completion.sequence,
                            "frame": completion.frame,
                        })
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        ScriptErrorInfo {
            error_type: "timeout".to_string(),
            message: format!(
                "Timed out waiting for completed egui diagnostic output from {}",
                pending.join(", ")
            ),
            location: None,
            backtrace: None,
            code: Some("diagnostic_barrier_timeout".to_string()),
            details: Some(serde_json::json!({
                "pending_viewports": pending,
                "last_completions": completions,
            })),
        }
    }

    async fn await_diagnostic_outputs(&self, viewport_ids: &BTreeSet<String>) -> ScriptResult<()> {
        if viewport_ids.is_empty() || !self.server.runtime.diagnostic_barriers_enabled() {
            return Ok(());
        }
        let journal = self.server.runtime.egui_diagnostics();
        let mut targets = Vec::new();
        for viewport_id in viewport_ids {
            let resolved = resolve_viewport_id(&self.server.inner, Some(viewport_id.clone()))
                .map_err(|error| {
                    self.tool_error(ScriptPosition::default(), tmcp::ToolError::from(error))
                })?;
            if !self.server.inner.viewports.is_live_viewport(resolved) {
                continue;
            }
            if self.server.inner.context_for(resolved).is_none() {
                continue;
            }
            targets.push((
                resolved,
                viewport_id.clone(),
                journal
                    .output_completion(viewport_id)
                    .map(|completion| completion.sequence),
            ));
            self.server.inner.request_repaint_of(resolved);
            if resolved != egui::ViewportId::ROOT {
                self.server.inner.request_repaint_of(egui::ViewportId::ROOT);
            }
        }

        loop {
            let notified = journal.completion_notify().notified();
            let pending = targets
                .iter()
                .filter_map(|(resolved, viewport_id, prior_sequence)| {
                    if !self.server.inner.viewports.is_live_viewport(*resolved) {
                        return None;
                    }
                    let completed =
                        journal
                            .output_completion(viewport_id)
                            .is_some_and(|completion| {
                                prior_sequence.is_none_or(|prior| completion.sequence > prior)
                            });
                    (!completed).then_some(viewport_id.clone())
                })
                .collect::<Vec<_>>();
            if pending.is_empty() {
                return Ok(());
            }
            match self.deadline {
                Some(deadline) => {
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .unwrap_or_default();
                    if remaining.is_zero() {
                        return Err(self.diagnostic_barrier_error(&pending));
                    }
                    if timeout(remaining, notified).await.is_err() {
                        return Err(self.diagnostic_barrier_error(&pending));
                    }
                }
                None => notified.await,
            }
        }
    }

    pub(super) async fn egui_diagnostics(
        &self,
        pos: ScriptPosition,
        viewport_id: String,
    ) -> ScriptResult<Value> {
        let resolved = resolve_viewport_id(&self.server.inner, Some(viewport_id))
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let viewport_id = viewport_id_to_string(resolved);
        self.record_targeted_viewport(viewport_id.clone());
        self.await_diagnostic_outputs(&BTreeSet::from([viewport_id.clone()]))
            .await?;
        let selection = self.diagnostic_selection(Some(&viewport_id));
        self.dismiss_diagnostic_sequences(selection.sequences);
        self.to_json(pos, selection.batch)
    }

    pub(super) async fn clear_egui_diagnostics(
        &self,
        pos: ScriptPosition,
        viewport_id: String,
    ) -> ScriptResult<()> {
        let resolved = resolve_viewport_id(&self.server.inner, Some(viewport_id))
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let viewport_id = viewport_id_to_string(resolved);
        self.record_targeted_viewport(viewport_id.clone());
        self.await_diagnostic_outputs(&BTreeSet::from([viewport_id.clone()]))
            .await?;
        let selection = self.diagnostic_selection(Some(&viewport_id));
        self.dismiss_diagnostic_sequences(selection.sequences);
        Ok(())
    }

    pub(super) async fn finish_egui_diagnostics(
        &self,
    ) -> (EguiDiagnosticBatch, Option<ScriptErrorInfo>) {
        let barrier_error = self
            .await_diagnostic_outputs(&self.targeted_viewports())
            .await
            .err();
        let batch = self.diagnostic_selection(None).batch;
        (batch, barrier_error)
    }

    pub(super) fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    pub(super) fn configure(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<()> {
        if let Some(timeout_ms) = parse_optional_u64(options, "timeout_ms")
            .map_err(|error| self.type_error(pos, error.message))?
        {
            *self
                .config_timeout_ms
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(timeout_ms);
        }
        if let Some(poll_interval_ms) = parse_optional_u64(options, "poll_interval_ms")
            .map_err(|error| self.type_error(pos, error.message))?
        {
            *self
                .config_poll_interval_ms
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(poll_interval_ms);
        }
        if let Some(settle) = parse_optional_bool(options, "settle")
            .map_err(|error| self.type_error(pos, error.message))?
        {
            *self.config_settle.lock().unwrap_or_else(|p| p.into_inner()) = Some(settle);
        }
        if let Some(animations) = parse_optional_bool(options, "animations")
            .map_err(|error| self.type_error(pos, error.message))?
        {
            let mut automation_options = self.server.inner.automation_options();
            automation_options.animations = animations;
            self.server.inner.set_automation_options(automation_options);
        }
        Ok(())
    }

    fn configured_timeout_ms(&self) -> Option<u64> {
        *self
            .config_timeout_ms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn configured_poll_interval_ms(&self) -> Option<u64> {
        *self
            .config_poll_interval_ms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn configured_settle(&self) -> bool {
        self.config_settle
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unwrap_or(true)
    }

    pub(super) fn log(&self, line: impl Into<String>) {
        let mut logs = self
            .logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        logs.push(line.into());
    }

    pub(super) fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn assertions(&self) -> Vec<ScriptAssertion> {
        self.assertions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn fixture_applications(&self) -> Vec<FixtureApplication> {
        self.fixtures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record_fixture(&self, name: String, params: BTreeMap<String, WidgetValue>) {
        let mut fixtures = self
            .fixtures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fixtures.push(FixtureApplication { name, params });
    }

    fn record_assertion(&self, passed: bool, message: String, pos: ScriptPosition) {
        let location = self.format_location(pos);
        let assertion = ScriptAssertion {
            passed,
            message,
            location,
        };
        let mut assertions = self
            .assertions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assertions.push(assertion);
    }

    pub(super) fn images(&self) -> Vec<ImageCapture> {
        self.images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn next_image_id(&self) -> String {
        let id = self.image_counter.fetch_add(1, Ordering::Relaxed);
        format!("img_{id}")
    }

    fn store_image(&self, image: ImageCapture) {
        let mut images = self
            .images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        images.push(image);
    }

    fn format_location(&self, pos: ScriptPosition) -> String {
        let source = self.source_name.as_str();
        match (pos.line, pos.column) {
            (Some(line), Some(column)) => format!("{source}:{line}:{column}"),
            (Some(line), None) => format!("{source}:{line}"),
            (None, _) => source.to_string(),
        }
    }

    pub(super) fn error_location(&self, pos: ScriptPosition) -> Option<ScriptLocation> {
        let line = pos.line?;
        Some(ScriptLocation {
            line,
            column: pos.column,
        })
    }

    pub(super) fn type_error(
        &self,
        pos: ScriptPosition,
        message: impl Into<String>,
    ) -> ScriptErrorInfo {
        ScriptErrorInfo {
            error_type: "type_error".to_string(),
            message: message.into(),
            location: self.error_location(pos),
            backtrace: None,
            code: None,
            details: None,
        }
    }

    fn runtime_error(&self, pos: ScriptPosition, message: impl Into<String>) -> ScriptErrorInfo {
        ScriptErrorInfo {
            error_type: "runtime".to_string(),
            message: message.into(),
            location: self.error_location(pos),
            backtrace: None,
            code: None,
            details: None,
        }
    }

    fn tool_error(&self, pos: ScriptPosition, error: tmcp::ToolError) -> ScriptErrorInfo {
        let structured_error = error
            .structured
            .as_ref()
            .and_then(|structured| structured.get("error"));
        let mut details = structured_error
            .and_then(|structured| structured.get("details"))
            .cloned();
        if let Some(source) = structured_error.and_then(|structured| structured.get("source")) {
            let map = details.get_or_insert_with(|| Value::Object(Map::new()));
            if !map.is_object() {
                *map = serde_json::json!({ "details": map.take() });
            }
            map.as_object_mut()
                .expect("details was normalized to an object")
                .insert("source".to_string(), source.clone());
        }
        ScriptErrorInfo {
            error_type: if error.code == "timeout" {
                "timeout".to_string()
            } else {
                "tool".to_string()
            },
            message: error.message,
            location: self.error_location(pos),
            backtrace: None,
            code: Some(error.code.to_string()),
            details,
        }
    }

    pub(super) fn script_timeout_error(&self, pos: ScriptPosition) -> ScriptErrorInfo {
        ScriptErrorInfo {
            error_type: "timeout".to_string(),
            message: format!("Script timed out after {}ms", self.script_timeout_ms),
            location: self.error_location(pos),
            backtrace: None,
            code: Some("timeout".to_string()),
            details: None,
        }
    }

    fn remaining_script_duration(&self, pos: ScriptPosition) -> ScriptResult<Option<Duration>> {
        let Some(deadline) = self.deadline else {
            return Ok(None);
        };
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            return Err(self.script_timeout_error(pos));
        }
        Ok(Some(remaining))
    }

    async fn await_tool<T>(
        &self,
        pos: ScriptPosition,
        fut: impl Future<Output = ToolResult<T>>,
    ) -> ScriptResult<T> {
        let remaining = self.remaining_script_duration(pos)?;
        let result = match remaining {
            Some(remaining) => timeout(remaining, fut).await.map_err(|_| {
                self.tool_error(
                    pos,
                    ToolError::new(
                        ErrorCode::Timeout,
                        "Script deadline exceeded while waiting for tool call",
                    )
                    .into_tmcp(),
                )
            })?,
            None => fut.await,
        };
        result.map_err(|error| self.tool_error(pos, error))
    }

    fn to_json<T: Serialize>(&self, pos: ScriptPosition, value: T) -> ScriptResult<Value> {
        serde_json::to_value(value).map_err(|error| {
            self.runtime_error(pos, format!("Failed to serialize result: {error}"))
        })
    }

    fn widget_handle_json(
        &self,
        pos: ScriptPosition,
        widget: &WidgetRegistryEntry,
    ) -> ScriptResult<Value> {
        self.record_targeted_viewport(widget.viewport_id.clone());
        self.to_json(
            pos,
            serde_json::json!({
                "id": widget.id,
                "viewport_id": widget.viewport_id,
                "__viewport_id": widget.viewport_id,
            }),
        )
    }

    fn widget_state_json(
        &self,
        pos: ScriptPosition,
        widget: &WidgetRegistryEntry,
    ) -> ScriptResult<Value> {
        let mut state = WidgetState::from(widget);
        if let Ok(viewport_id) =
            resolve_viewport_id(&self.server.inner, Some(widget.viewport_id.clone()))
        {
            state.child_ids = self
                .server
                .inner
                .widgets
                .child_ids_of(viewport_id, widget.id.as_str());
        }
        self.to_json(pos, state)
    }

    fn widget_handle_list_json(
        &self,
        pos: ScriptPosition,
        widgets: &[WidgetRegistryEntry],
    ) -> ScriptResult<Value> {
        for widget in widgets {
            self.record_targeted_viewport(widget.viewport_id.clone());
        }
        self.to_json(
            pos,
            widgets
                .iter()
                .map(|widget| {
                    serde_json::json!({
                        "id": widget.id,
                        "viewport_id": widget.viewport_id,
                        "__viewport_id": widget.viewport_id,
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    fn viewport_handle_json(&self, pos: ScriptPosition, viewport_id: &str) -> ScriptResult<Value> {
        self.record_targeted_viewport(viewport_id.to_string());
        self.to_json(
            pos,
            serde_json::json!({
                "id": viewport_id,
            }),
        )
    }

    fn viewport_state_json(
        &self,
        pos: ScriptPosition,
        snapshot: &ViewportSnapshot,
    ) -> ScriptResult<Value> {
        let resolved =
            resolve_viewport_id(&self.server.inner, Some(snapshot.viewport_id.clone())).ok();
        let input = resolved
            .and_then(|viewport_id| self.server.inner.viewports.input_snapshot(viewport_id));
        let frame_count = resolved
            .map(|viewport_id| {
                self.server
                    .inner
                    .viewports
                    .recorded_frame_count(viewport_id)
            })
            .unwrap_or_default();
        self.to_json(
            pos,
            serde_json::json!({
                "id": snapshot.viewport_id,
                "name": snapshot.name,
                "title": snapshot.title,
                "outer_pos": snapshot.outer_pos,
                "outer_size": snapshot.outer_size,
                "inner_size": snapshot.inner_size,
                "focused": snapshot.focused,
                "minimized": snapshot.minimized,
                "occluded": snapshot.occluded,
                "os_minimized": snapshot.os_minimized,
                "os_occluded": snapshot.os_occluded,
                "maximized": snapshot.maximized,
                "fullscreen": snapshot.fullscreen,
                "frame_count": frame_count,
                "pixels_per_point": snapshot.pixels_per_point,
                "pointer_pos": input.as_ref().and_then(|i| i.pointer_pos),
            }),
        )
    }

    fn viewport_handle_list_json(
        &self,
        pos: ScriptPosition,
        snapshots: &[ViewportSnapshot],
    ) -> ScriptResult<Value> {
        self.to_json(
            pos,
            snapshots
                .iter()
                .map(|snapshot| serde_json::json!({ "id": snapshot.viewport_id }))
                .collect::<Vec<_>>(),
        )
    }

    fn modifiers_from_options(
        &self,
        options: Option<&Map<String, Value>>,
    ) -> Result<Option<Modifiers>, ScriptErrorInfo> {
        match options {
            Some(map) => Ok(Some(parse_modifiers(Some(map))?)),
            None => Ok(None),
        }
    }

    pub(super) fn parse_wait_options(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<(Option<String>, Option<u64>, Option<u64>)> {
        let viewport_id = self.parse_optional_viewport_option(pos, options)?;
        let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
        Ok((viewport_id, timeout_ms, poll_interval_ms))
    }

    fn parse_optional_viewport_option(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Option<String>> {
        let viewport = parse_optional_string(options, "viewport")
            .map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        match (viewport, viewport_id) {
            (Some(viewport), Some(viewport_id)) if viewport != viewport_id => Err(self.type_error(
                pos,
                "options must not include both viewport and viewport_id with different values",
            )),
            (Some(viewport), _) => Ok(Some(viewport)),
            (None, viewport_id) => Ok(viewport_id),
        }
    }

    fn diff_options(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<DiffOptions> {
        let move_epsilon = parse_optional_f32(options, "move_epsilon")
            .map_err(|error| self.type_error(pos, error.message))?
            .unwrap_or(0.5);
        if !move_epsilon.is_finite() || move_epsilon < 0.0 {
            return Err(self.type_error(pos, "move_epsilon must be a non-negative finite number"));
        }
        let include_invisible = parse_optional_bool(options, "include_invisible")
            .map_err(|error| self.type_error(pos, error.message))?
            .unwrap_or(false);
        let id_prefix = parse_optional_string(options, "id_prefix")
            .map_err(|error| self.type_error(pos, error.message))?;
        Ok(DiffOptions {
            move_epsilon,
            include_invisible,
            id_prefix,
        })
    }

    fn capture_snapshot(&self) -> ScriptCapture {
        let mut viewports = self
            .server
            .inner
            .viewports
            .viewports_snapshot()
            .into_iter()
            .map(|snapshot| snapshot.viewport_id)
            .collect::<Vec<_>>();
        if viewports.is_empty() {
            viewports.push(viewport_id_to_string(egui::ViewportId::ROOT));
        }
        let widgets = viewports
            .iter()
            .filter_map(|viewport_id| {
                self.server
                    .inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id.clone()))
                    .ok()
            })
            .flat_map(|viewport_id| self.server.inner.widgets.widget_list(viewport_id))
            .map(|widget| ScriptCapturedWidget {
                id: widget.id.clone(),
                viewport_id: widget.viewport_id.clone(),
                state: WidgetState::from(&widget),
            })
            .collect();
        ScriptCapture {
            frame: self.server.inner.frame_count(),
            widgets,
            viewports,
        }
    }

    fn resolve_target_viewport(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<&str>,
        target: &WidgetRef,
    ) -> ScriptResult<String> {
        let (_, resolved_viewport_id) =
            resolve_widget_and_viewport(&self.server.inner, viewport_id, target)
                .map_err(|error| self.tool_error(pos, error.into()))?;
        let viewport_id = viewport_id_to_string(resolved_viewport_id);
        self.record_targeted_viewport(viewport_id.clone());
        Ok(viewport_id)
    }

    async fn settle_after_action(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
        viewport_id: Option<String>,
    ) -> ScriptResult<()> {
        if !self.action_settle_enabled(pos, options)? {
            return Ok(());
        }
        let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
        self.await_tool(
            pos,
            self.server
                .wait_for_settle(viewport_id, timeout_ms, poll_interval_ms),
        )
        .await?;
        Ok(())
    }

    fn action_settle_enabled(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<bool> {
        parse_optional_bool(options, "settle")
            .map_err(|error| self.type_error(pos, error.message))
            .map(|settle| settle.unwrap_or_else(|| self.configured_settle()))
    }

    fn action_timeouts(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<(Option<u64>, Option<u64>)> {
        let timeout_ms = parse_optional_u64(options, "timeout_ms")
            .map_err(|error| self.type_error(pos, error.message))?
            .or_else(|| self.configured_timeout_ms());
        let poll_interval_ms = parse_optional_u64(options, "poll_interval_ms")
            .map_err(|error| self.type_error(pos, error.message))?
            .or_else(|| self.configured_poll_interval_ms());
        Ok((timeout_ms, poll_interval_ms))
    }

    fn parse_action_target(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<(WidgetRef, String)> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let action_viewport_id =
            self.resolve_target_viewport(pos, viewport_id.as_deref(), &target)?;
        Ok((target, action_viewport_id))
    }

    async fn finish_action<T: Serialize>(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
        viewport_id: Option<String>,
        result: T,
        _target: Option<&WidgetRef>,
    ) -> ScriptResult<Value> {
        self.settle_after_action(pos, options, viewport_id).await?;
        self.to_json(pos, result)
    }

    pub(super) fn widget_list(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let include_invisible = parse_optional_bool(options, "include_invisible")
            .map_err(|error| self.type_error(pos, error.message))?;
        if map_value(options, "offset").is_some() || map_value(options, "limit").is_some() {
            return Err(self.type_error(pos, "widget_list no longer accepts offset or limit"));
        }
        let role = match map_value(options, "role") {
            None => None,
            Some(value) => Some(
                parse_widget_role(value).map_err(|error| self.type_error(pos, error.message))?,
            ),
        };
        let id_prefix = parse_optional_string(options, "id_prefix")
            .map_err(|error| self.type_error(pos, error.message))?;
        let label = parse_optional_string(options, "label")
            .map_err(|error| self.type_error(pos, error.message))?;
        let label_contains = parse_optional_string(options, "label_contains")
            .map_err(|error| self.type_error(pos, error.message))?;
        let visible = parse_optional_bool(options, "visible")
            .map_err(|error| self.type_error(pos, error.message))?;
        let enabled = parse_optional_bool(options, "enabled")
            .map_err(|error| self.type_error(pos, error.message))?;
        let focused = parse_optional_bool(options, "focused")
            .map_err(|error| self.type_error(pos, error.message))?;
        let selected = parse_optional_bool(options, "selected")
            .map_err(|error| self.type_error(pos, error.message))?;
        let widgets = collect_widget_list(
            &self.server.inner,
            viewport_id,
            include_invisible,
            role,
            id_prefix.as_deref(),
            label.as_deref(),
            label_contains.as_deref(),
            visible,
            enabled,
            focused,
            selected,
        )
        .map_err(|error| self.tool_error(pos, error))?;
        self.widget_handle_list_json(pos, &widgets)
    }

    pub(super) async fn widget_set_value(
        &self,
        pos: ScriptPosition,
        target: &Value,
        value: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let value = widget_value_from_dynamic(value)
            .map_err(|error| self.type_error(pos, error.message))?;
        let settle_enabled = self.action_settle_enabled(pos, options)?;
        self.await_tool(
            pos,
            self.server.widget_set_value(
                Some(action_viewport_id.clone()),
                target.clone(),
                value.clone(),
            ),
        )
        .await?;
        self.settle_after_action(pos, options, Some(action_viewport_id.clone()))
            .await?;
        if settle_enabled {
            let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    "to accept the requested value",
                    |widget| {
                        widget
                            .and_then(|widget| widget.value.as_ref())
                            .is_some_and(|current| widget_values_match(current, &value))
                    },
                ),
            )
            .await?;
        }
        self.to_json(pos, ())
    }

    pub(super) fn widget_at_point(
        &self,
        pos: ScriptPosition,
        pos_arg: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let point = parse_pos2(pos_arg).map_err(|error| self.type_error(pos, error.message))?;
        let all_layers = parse_optional_bool(options, "all_layers")
            .map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let result = self
            .server
            .widget_at_point_result(point, all_layers, viewport_id.as_deref())
            .map_err(|error| self.tool_error(pos, error))?;
        self.widget_handle_list_json(pos, &result.widgets)
    }

    pub(super) async fn text_measure(
        &self,
        pos: ScriptPosition,
        target: &Value,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let result = self
            .await_tool(pos, self.server.text_measure(target))
            .await?;
        self.to_json(pos, result)
    }

    pub(super) async fn action_click(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let button = match map_value(options, "button") {
            None => None,
            Some(value) => Some(
                parse_pointer_button(value).map_err(|error| self.type_error(pos, error.message))?,
            ),
        };
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        let click_count = parse_optional_u8(options, "click_count")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_click(
                Some(action_viewport_id.clone()),
                target.clone(),
                button,
                modifiers,
                click_count,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), Some(&target))
            .await
    }

    pub(super) async fn action_hover(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let position = parse_optional_vec2(options, "position")
            .map_err(|error| self.type_error(pos, error.message))?;
        let duration_ms = parse_optional_u64(options, "duration_ms")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_hover(
                Some(action_viewport_id.clone()),
                target,
                position,
                duration_ms,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
    }

    pub(super) async fn action_type(
        &self,
        pos: ScriptPosition,
        target: &Value,
        text: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let clear = parse_optional_bool(options, "clear")
            .map_err(|error| self.type_error(pos, error.message))?;
        let enter = parse_optional_bool(options, "enter")
            .map_err(|error| self.type_error(pos, error.message))?;
        let focus_timeout_ms = parse_optional_u64(options, "focus_timeout_ms")
            .map_err(|error| self.type_error(pos, error.message))?;
        if focus_timeout_ms.is_some() {
            self.await_tool(pos, async {
                self.server
                    .focus_widget_for_keyboard(
                        Some(action_viewport_id.clone()),
                        &target,
                        focus_timeout_ms,
                    )
                    .await
                    .map_err(tmcp::ToolError::from)
            })
            .await?;
        }
        self.await_tool(
            pos,
            self.server.action_type(
                Some(action_viewport_id.clone()),
                target.clone(),
                text,
                enter,
                clear,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), Some(&target))
            .await
    }

    pub(super) async fn action_focus(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        self.await_tool(
            pos,
            self.server
                .action_focus(Some(action_viewport_id.clone()), target),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
    }

    pub(super) async fn action_drag(
        &self,
        pos: ScriptPosition,
        target: &Value,
        to: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let to = parse_pos2(to).map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server
                .action_drag(Some(action_viewport_id.clone()), target, to, modifiers),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
    }

    pub(super) async fn action_drag_relative(
        &self,
        pos: ScriptPosition,
        target: &Value,
        to: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let to = parse_vec2(to).map_err(|error| self.type_error(pos, error.message))?;
        let from = parse_optional_vec2(options, "from")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_drag_relative(
                Some(action_viewport_id.clone()),
                target,
                from,
                to,
                modifiers,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
    }

    pub(super) async fn action_drag_to_widget(
        &self,
        pos: ScriptPosition,
        from: &Value,
        to: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (from, action_viewport_id) = self.parse_action_target(pos, from, options)?;
        let to = parse_widget_ref(to).map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_drag_to_widget(
                Some(action_viewport_id.clone()),
                from,
                to,
                modifiers,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
    }

    pub(super) async fn action_scroll(
        &self,
        pos: ScriptPosition,
        target: &Value,
        delta: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let delta = parse_vec2(delta).map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server
                .action_scroll(Some(action_viewport_id.clone()), target, delta, modifiers),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
    }

    pub(super) async fn action_scroll_to(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let offset = parse_optional_vec2(options, "offset")
            .map_err(|error| self.type_error(pos, error.message))?;
        let mut align = match map_value(options, "align") {
            None => None,
            Some(value) => Some(
                parse_scroll_align(value).map_err(|error| self.type_error(pos, error.message))?,
            ),
        };
        if offset.is_some() {
            align = None;
        }
        if offset.is_none() && align.is_none() {
            return Err(self.type_error(pos, "scroll_to requires either offset or align"));
        }
        let target_offset = self
            .await_tool(
                pos,
                self.server.action_scroll_to(
                    Some(action_viewport_id.clone()),
                    target.clone(),
                    offset,
                    align,
                ),
            )
            .await?;
        let settle_enabled = self.action_settle_enabled(pos, options)?;
        self.settle_after_action(pos, options, Some(action_viewport_id.clone()))
            .await?;
        if settle_enabled {
            let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    "to reach the requested scroll offset",
                    |widget| {
                        widget
                            .and_then(|widget| widget.role_state.as_ref())
                            .and_then(|state| state.scroll_state())
                            .is_some_and(|scroll| {
                                scroll_offsets_match(scroll.offset, target_offset)
                            })
                    },
                ),
            )
            .await?;
        }
        self.to_json(pos, ())
    }

    pub(super) async fn action_scroll_into_view(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        self.await_tool(
            pos,
            self.server
                .action_scroll_into_view(Some(action_viewport_id.clone()), target.clone()),
        )
        .await?;
        let settle_enabled = self.action_settle_enabled(pos, options)?;
        self.settle_after_action(pos, options, Some(action_viewport_id.clone()))
            .await?;
        if settle_enabled {
            // Requesting the offsets only queues them. An animated scroll area
            // needs the target polled to the interaction-ready predicate, or
            // the next click fails on the call that just returned.
            let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    "to become interaction-ready after scroll_into_view",
                    |widget| widget.is_some_and(interaction_ready),
                ),
            )
            .await?;
        }
        self.to_json(pos, ())
    }

    pub(super) async fn action_key(
        &self,
        pos: ScriptPosition,
        combo: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (key, modifiers, key_name) =
            parse_key_combo(&combo).map_err(|msg| self.type_error(pos, msg))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let repeat = parse_optional_u32(options, "repeat_count")
            .map_err(|error| self.type_error(pos, error.message))?;
        let target = match options.and_then(|map| map.get("target")) {
            Some(value) => {
                Some(parse_widget_ref(value).map_err(|error| self.type_error(pos, error.message))?)
            }
            None => None,
        };
        let focus_timeout_ms = parse_optional_u64(options, "focus_timeout_ms")
            .map_err(|error| self.type_error(pos, error.message))?;
        let action_viewport_id = if let Some(target) = &target {
            Some(self.resolve_target_viewport(pos, viewport_id.as_deref(), target)?)
        } else {
            viewport_id
        };
        if let Some(target) = &target {
            let focus_timeout_ms = focus_timeout_ms.or(Some(5_000));
            self.await_tool(pos, async {
                self.server
                    .focus_widget_for_keyboard(action_viewport_id.clone(), target, focus_timeout_ms)
                    .await
                    .map_err(tmcp::ToolError::from)
            })
            .await?;
        }
        self.await_tool(
            pos,
            self.server.action_key(
                action_viewport_id.clone(),
                key,
                modifiers,
                &key_name,
                repeat,
            ),
        )
        .await?;
        self.finish_action(pos, options, action_viewport_id, (), target.as_ref())
            .await
    }

    pub(super) async fn action_paste(
        &self,
        pos: ScriptPosition,
        text: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.action_paste(viewport_id.clone(), text))
            .await?;
        self.finish_action(pos, options, viewport_id, (), None)
            .await
    }

    fn parse_optional_viewport_id_arg(
        &self,
        pos: ScriptPosition,
        arg: Option<&Value>,
    ) -> ScriptResult<Option<String>> {
        match arg {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(self.type_error(pos, "viewport_id must be a string")),
        }
    }

    pub(super) fn viewport_handle(
        &self,
        pos: ScriptPosition,
        viewport_id: &str,
    ) -> ScriptResult<Value> {
        self.viewport_handle_json(pos, viewport_id)
    }

    pub(super) async fn viewports_list(
        &self,
        pos: ScriptPosition,
        arg: Option<&Value>,
    ) -> ScriptResult<Value> {
        let viewport_id = self.parse_optional_viewport_id_arg(pos, arg)?;
        let result = self
            .await_tool(pos, self.server.viewports_list(viewport_id))
            .await?;
        self.viewport_handle_list_json(pos, &result)
    }

    pub(super) fn viewport_state(
        &self,
        pos: ScriptPosition,
        viewport_id: String,
    ) -> ScriptResult<Value> {
        let resolved = self
            .server
            .inner
            .viewports
            .resolve_viewport_id(Some(viewport_id.clone()))
            .ok();
        if let Some(resolved) = resolved
            && !self.server.inner.viewports.is_live_viewport(resolved)
        {
            return Ok(Value::Null);
        }
        let snapshot = self
            .server
            .inner
            .viewports
            .viewports_snapshot()
            .into_iter()
            .find(|snapshot| snapshot.viewport_id == viewport_id);
        snapshot.map_or(Ok(Value::Null), |snapshot| {
            self.viewport_state_json(pos, &snapshot)
        })
    }

    pub(super) fn widget_state(&self, pos: ScriptPosition, target: &Value) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let result = self
            .server
            .inner
            .widgets
            .resolve_widget_global(&self.server.inner.viewports, &target)
            .map_err(|error| tmcp::ToolError::from(ToolError::from(error)));
        match result {
            Ok(widget) => self.widget_state_json(pos, &widget),
            Err(error)
                if error.code == "not_found"
                    && error
                        .structured
                        .as_ref()
                        .and_then(|value| value.pointer("/error/details/reason"))
                        .is_none() =>
            {
                Ok(Value::Null)
            }
            Err(error) => Err(self.tool_error(pos, error)),
        }
    }

    pub(super) fn widget_parent(&self, pos: ScriptPosition, target: &Value) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let inner = &self.server.inner;
        let widget = resolve_widget(inner, None, &target)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let Some(parent_id) = widget.parent_id.as_deref() else {
            return Ok(Value::Null);
        };
        let viewport_id = resolve_viewport_id(inner, Some(widget.viewport_id.clone()))
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let parent = self
            .server
            .inner
            .widgets
            .widget_list(viewport_id)
            .into_iter()
            .find(|candidate| {
                candidate.id == parent_id
                    && candidate.rect.min.x <= widget.rect.min.x
                    && candidate.rect.min.y <= widget.rect.min.y
                    && candidate.rect.max.x >= widget.rect.max.x
                    && candidate.rect.max.y >= widget.rect.max.y
            })
            .or_else(|| {
                self.server
                    .inner
                    .widgets
                    .widget_list(viewport_id)
                    .into_iter()
                    .rev()
                    .find(|candidate| candidate.id == parent_id)
            });
        match parent {
            Some(parent) => self.widget_handle_json(pos, &parent),
            None => Ok(Value::Null),
        }
    }

    pub(super) fn widget_children(
        &self,
        pos: ScriptPosition,
        target: &Value,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let inner = &self.server.inner;
        let widget = resolve_widget(inner, None, &target)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let viewport_id = resolve_viewport_id(inner, Some(widget.viewport_id.clone()))
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let widgets = self.server.inner.widgets.widget_list(viewport_id);
        let children = widgets
            .into_iter()
            .filter(|candidate| candidate.parent_id.as_deref() == Some(widget.id.as_str()))
            .collect::<Vec<_>>();
        self.widget_handle_list_json(pos, &children)
    }

    pub(super) async fn viewport_input(
        &self,
        pos: ScriptPosition,
        event: &Value,
        viewport_id: String,
    ) -> ScriptResult<Value> {
        let event = serde_json::from_value::<RawInputEvent>(event.clone())
            .map_err(|error| self.type_error(pos, format!("invalid raw input event: {error}")))?;
        self.record_targeted_viewport(viewport_id.clone());
        self.await_tool(pos, self.server.input(Some(viewport_id), event))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn wait_for_widget_predicate<F, Fut>(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
        condition: &str,
        predicate: F,
    ) -> ScriptResult<Value>
    where
        F: Fn(Value) -> Fut + Clone,
        Fut: Future<Output = ScriptResult<bool>>,
    {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        let target_viewport = viewport_id
            .clone()
            .or_else(|| target.viewport_id.clone())
            .and_then(|viewport_id| {
                self.server
                    .inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            });
        let result = super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            self.deadline,
            || {
                let predicate = predicate.clone();
                let target = target.clone();
                let viewport_id = viewport_id.clone();
                async move {
                    match resolve_wait_widget(&self.server.inner, viewport_id.as_deref(), &target) {
                        Ok(widget) => match self.widget_state_json(pos, &widget) {
                            Ok(widget_json) => match predicate(widget_json).await {
                                Ok(matched) => Ok::<_, ScriptErrorInfo>((matched, Some(widget))),
                                Err(error) => Err(error),
                            },
                            Err(error) => Err(self.runtime_error(
                                pos,
                                format!("Failed to prepare widget state for predicate: {error:?}"),
                            )),
                        },
                        Err(error) if error.code() == ErrorCode::NotFound => {
                            match predicate(Value::Null).await {
                                Ok(matched) => Ok((matched, None)),
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(self.tool_error(pos, error.into())),
                    }
                }
            },
        )
        .await;

        match result {
            Ok((matched, widget, elapsed_ms, observation)) => {
                if !matched
                    && self
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    self.to_json(pos, widget.as_ref().map(WidgetState::from))
                } else {
                    let mut details = wait_timeout_details(
                        "widget",
                        elapsed_ms,
                        widget.as_ref(),
                        None,
                        None,
                        None,
                        &observation,
                    );
                    if let Err(error) =
                        resolve_wait_widget(&self.server.inner, viewport_id.as_deref(), &target)
                    {
                        super::super::merge_missing_widget_search(&mut details, &error);
                    }
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            wait_timeout_message(
                                format!(
                                    "Timed out waiting for widget {condition} after {timeout_ms}ms"
                                ),
                                &observation,
                            ),
                        )
                        .with_details(details)
                        .into_tmcp(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn wait_for_widget_absent(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        let target_viewport = viewport_id
            .clone()
            .or_else(|| target.viewport_id.clone())
            .and_then(|viewport_id| {
                self.server
                    .inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            });
        let result = super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            self.deadline,
            || {
                let result = match resolve_wait_widget(
                    &self.server.inner,
                    viewport_id.as_deref(),
                    &target,
                ) {
                    Ok(widget) => Ok::<_, ScriptErrorInfo>((false, Some(widget))),
                    Err(error) if error.code() == ErrorCode::NotFound => Ok((true, None)),
                    Err(error) => Err(self.tool_error(pos, error.into())),
                };
                async move { result }
            },
        )
        .await;

        match result {
            Ok((matched, _widget, elapsed_ms, observation)) => {
                if !matched
                    && self
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    Ok(Value::Null)
                } else {
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            wait_timeout_message(
                                format!(
                                    "Timed out waiting for widget absence after {timeout_ms}ms"
                                ),
                                &observation,
                            ),
                        )
                        .with_details(wait_timeout_details(
                            "widget_absent",
                            elapsed_ms,
                            None,
                            None,
                            None,
                            None,
                            &observation,
                        ))
                        .into_tmcp(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn wait_for_frames(
        &self,
        pos: ScriptPosition,
        count: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let count =
            parse_optional_u64_val(count).map_err(|error| self.type_error(pos, error.message))?;
        // Frame counting is app-wide, so only the timeout applies here.
        let (_viewport_id, timeout_ms, _poll_interval_ms) =
            self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.or_else(|| self.configured_timeout_ms());
        let result = self
            .await_tool(pos, self.server.wait_for_frame_count(count, timeout_ms))
            .await?;
        self.to_json(pos, result)
    }

    pub(super) async fn wait_for_fresh_capture(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        self.await_tool(
            pos,
            self.server
                .wait_for_fresh_capture(viewport_id, timeout_ms, poll_interval_ms),
        )
        .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn wait_for_settle(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let report = self
            .await_tool(
                pos,
                self.server
                    .wait_for_settle(viewport_id, timeout_ms, poll_interval_ms),
            )
            .await?;
        self.to_json(pos, report)
    }

    pub(super) fn capture(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.to_json(pos, self.capture_snapshot())
    }

    pub(super) fn capture_diff(
        &self,
        pos: ScriptPosition,
        capture: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let before = serde_json::from_value::<ScriptCapture>(capture.clone()).map_err(|error| {
            self.type_error(
                pos,
                format!("Capture:diff expected a capture table: {error}"),
            )
        })?;
        let options = self.diff_options(pos, options)?;
        self.to_json(
            pos,
            diff_captures(&before, &self.capture_snapshot(), &options),
        )
    }

    pub(super) async fn wait_for_viewport_predicate<F, Fut>(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
        predicate: F,
    ) -> ScriptResult<Value>
    where
        F: Fn(Value) -> Fut + Clone,
        Fut: Future<Output = ScriptResult<bool>>,
    {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
        let inner = &self.server.inner;
        let viewport_id = resolve_viewport_id(inner, viewport_id)
            .map_err(|error| self.tool_error(pos, error.into()))?;

        let result = super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            Some(viewport_id),
            self.deadline,
            || {
                let predicate = predicate.clone();
                async move {
                    match viewport_snapshot_for(&self.server.inner, viewport_id) {
                        Some(snapshot) => match self.viewport_state_json(pos, &snapshot) {
                            Ok(viewport_json) => match predicate(viewport_json).await {
                                Ok(matched) => Ok::<_, ScriptErrorInfo>((matched, Some(snapshot))),
                                Err(error) => Err(error),
                            },
                            Err(error) => Err(self.runtime_error(
                                pos,
                                format!(
                                    "Failed to prepare viewport state for predicate: {error:?}"
                                ),
                            )),
                        },
                        None => {
                            if self.server.inner.viewports.is_live_viewport(viewport_id) {
                                Err(self.tool_error(
                                    pos,
                                    ToolError::new(
                                        ErrorCode::NotActionable,
                                        "Viewport not ready for wait",
                                    )
                                    .into_tmcp(),
                                ))
                            } else {
                                match predicate(Value::Null).await {
                                    Ok(matched) => Ok((matched, None)),
                                    Err(error) => Err(error),
                                }
                            }
                        }
                    }
                }
            },
        )
        .await;

        match result {
            Ok((matched, viewport, elapsed_ms, observation)) => {
                if !matched
                    && self
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(self.script_timeout_error(pos));
                }
                let viewport = viewport
                    .ok_or_else(|| self.runtime_error(pos, "Viewport not ready for wait"))?;
                if matched {
                    self.viewport_state_json(pos, &viewport)
                } else {
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            wait_timeout_message(
                                format!(
                                    "Timed out waiting for viewport predicate after {timeout_ms}ms"
                                ),
                                &observation,
                            ),
                        )
                        .with_details(wait_timeout_details(
                            "viewport",
                            elapsed_ms,
                            None,
                            Some(&viewport),
                            None,
                            None,
                            &observation,
                        ))
                        .into_tmcp(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn screenshot(
        &self,
        pos: ScriptPosition,
        target: Option<&Value>,
    ) -> ScriptResult<Value> {
        let mut viewport_id = None;
        let mut widget_target: Option<WidgetRef> = None;
        if let Some(target) = target {
            if let Some(map) = target.as_object() {
                let has_target = map_has_any(map, &["id"]);
                if has_target {
                    widget_target = Some(
                        parse_widget_ref(target)
                            .map_err(|error| self.type_error(pos, error.message))?,
                    );
                    viewport_id = parse_optional_string(Some(map), "viewport_id")
                        .map_err(|error| self.type_error(pos, error.message))?;
                } else if map_has_any(map, &["viewport_id"]) {
                    viewport_id = parse_optional_string(Some(map), "viewport_id")
                        .map_err(|error| self.type_error(pos, error.message))?;
                } else {
                    return Err(
                        self.type_error(pos, "screenshot expects a WidgetRef or viewport_id")
                    );
                }
            } else {
                widget_target = Some(
                    parse_widget_ref(target)
                        .map_err(|error| self.type_error(pos, error.message))?,
                );
            }
        }

        let id = self.next_image_id();
        if let Some(target) = widget_target {
            let (widget, viewport_id_resolved) =
                resolve_widget_and_viewport(&self.server.inner, viewport_id.as_deref(), &target)
                    .map_err(|error| self.tool_error(pos, error.into()))?;
            let pixels_per_point = self
                .server
                .inner
                .viewports
                .input_snapshot(viewport_id_resolved)
                .map(|snapshot| snapshot.pixels_per_point)
                .unwrap_or(1.0);
            let data = self
                .await_tool(pos, async {
                    capture_screenshot(
                        &self.server.inner,
                        &self.server.runtime,
                        viewport_id_resolved,
                        ScreenshotKind::Widget {
                            rect: widget.interact_rect,
                            pixels_per_point,
                        },
                    )
                    .await
                    .map_err(tmcp::ToolError::from)
                })
                .await?;
            self.store_image(ImageCapture {
                id: id.clone(),
                data,
                kind: ScriptImageKind::Widget,
                viewport_id: viewport_id_to_string(viewport_id_resolved),
                target: Some(target),
                rect: Some(widget.interact_rect),
            });
            return Ok(image_ref_json(id));
        }

        let viewport_id_resolved = resolve_screenshot_viewport(&self.server.inner, viewport_id)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let data = self
            .await_tool(pos, async {
                capture_screenshot(
                    &self.server.inner,
                    &self.server.runtime,
                    viewport_id_resolved,
                    ScreenshotKind::Viewport,
                )
                .await
                .map_err(tmcp::ToolError::from)
            })
            .await?;
        self.store_image(ImageCapture {
            id: id.clone(),
            data,
            kind: ScriptImageKind::Viewport,
            viewport_id: viewport_id_to_string(viewport_id_resolved),
            target: None,
            rect: None,
        });
        Ok(image_ref_json(id))
    }

    pub(super) async fn sample_pixels(
        &self,
        pos: ScriptPosition,
        positions: &Value,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let positions = positions
            .as_array()
            .ok_or_else(|| self.type_error(pos, "sample_pixels expects an array of positions"))?
            .iter()
            .map(|value| parse_pos2(value).map_err(|error| self.type_error(pos, error.message)))
            .collect::<Result<Vec<_>, _>>()?;
        let samples = self
            .await_tool(
                pos,
                self.server.viewport_sample_pixels(viewport_id, positions),
            )
            .await?;
        self.to_json(pos, samples)
    }

    pub(super) async fn widget_sample_pixels(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
        positions: &Value,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let positions = positions
            .as_array()
            .ok_or_else(|| self.type_error(pos, "sample_pixels expects an array of positions"))?
            .iter()
            .map(|value| parse_pos2(value).map_err(|error| self.type_error(pos, error.message)))
            .collect::<Result<Vec<_>, _>>()?;
        let samples = self
            .await_tool(
                pos,
                self.server
                    .widget_sample_pixels(viewport_id, &target, positions),
            )
            .await?;
        self.to_json(pos, samples)
    }

    pub(super) async fn widget_sample_grid(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
        nx: &Value,
        ny: &Value,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let nx =
            parse_sample_grid_count(nx, "nx").map_err(|message| self.type_error(pos, message))?;
        let ny =
            parse_sample_grid_count(ny, "ny").map_err(|message| self.type_error(pos, message))?;
        let samples = self
            .await_tool(
                pos,
                self.server.widget_sample_grid(viewport_id, &target, nx, ny),
            )
            .await?;
        self.to_json(pos, samples)
    }

    pub(super) async fn check_layout(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let result = self
            .await_tool(pos, self.server.check_layout(viewport_id, None))
            .await?;
        self.to_json(pos, result)
    }

    pub(super) async fn check_layout_widget(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let result = self
            .await_tool(pos, self.server.check_layout(viewport_id, Some(target)))
            .await?;
        self.to_json(pos, result)
    }

    /// Show a highlight on a widget (by target) or a rect with a mandatory color.
    pub(super) async fn show_highlight_widget(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
        color: String,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let result = self
            .await_tool(
                pos,
                self.server
                    .show_highlight(viewport_id, Some(target), None, color),
            )
            .await?;
        self.to_json(pos, result)
    }

    /// Show a highlight on a rect with a mandatory color.
    pub(super) async fn show_highlight_rect(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
        rect: Rect,
        color: String,
    ) -> ScriptResult<Value> {
        let result = self
            .await_tool(
                pos,
                self.server
                    .show_highlight(viewport_id, None, Some(rect), color),
            )
            .await?;
        self.to_json(pos, result)
    }

    /// Clear a widget's highlight.
    pub(super) async fn clear_widget_highlight(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.clear_highlights(viewport_id, Some(target)))
            .await?;
        Ok(Value::Null)
    }

    /// Clear all highlights.
    pub(super) async fn clear_highlights(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.clear_highlights(None, None))
            .await?;
        Ok(Value::Null)
    }

    pub(super) async fn show_debug_overlay(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
        mode: Option<&Value>,
        options: Option<&Map<String, Value>>,
        scope: Option<WidgetRef>,
    ) -> ScriptResult<Value> {
        let mode = match mode {
            None => None,
            Some(value) => Some(
                parse_overlay_mode(value).map_err(|error| self.type_error(pos, error.message))?,
            ),
        };
        let options = options
            .map(|map| {
                Ok::<_, ScriptErrorInfo>(OverlayDebugOptionsInput {
                    show_labels: parse_optional_bool(Some(map), "show_labels")?,
                    show_sizes: parse_optional_bool(Some(map), "show_sizes")?,
                    label_font_size: parse_optional_f32(Some(map), "label_font_size")?,
                    bounds_color: parse_optional_string(Some(map), "bounds_color")?,
                    clip_color: parse_optional_string(Some(map), "clip_color")?,
                    overlap_color: parse_optional_string(Some(map), "overlap_color")?,
                })
            })
            .transpose()
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server
                .show_debug_overlay(viewport_id, mode, scope, options),
        )
        .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn clear_debug_overlay(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.clear_debug_overlay())
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn viewport_resize(
        &self,
        pos: ScriptPosition,
        options: &Value,
        viewport_id: String,
    ) -> ScriptResult<Value> {
        let resize = serde_json::from_value::<ResizeOptions>(options.clone())
            .map_err(|error| self.type_error(pos, format!("invalid resize options: {error}")))?;
        self.record_targeted_viewport(viewport_id.clone());
        self.await_tool(
            pos,
            self.server
                .viewport_resize(Some(viewport_id.clone()), resize),
        )
        .await?;
        self.finish_action(pos, options.as_object(), Some(viewport_id), (), None)
            .await
    }

    pub(super) async fn focus_window(
        &self,
        pos: ScriptPosition,
        viewport: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.focus_window(viewport.clone()))
            .await?;
        self.finish_action(pos, options, Some(viewport), (), None)
            .await
    }

    pub(super) async fn viewport_dismiss_popups(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        self.await_tool(
            pos,
            self.server.viewport_dismiss_popups(viewport_id.clone()),
        )
        .await?;
        self.finish_action(pos, options, viewport_id, (), None)
            .await
    }

    pub(super) async fn fixture_apply(
        &self,
        pos: ScriptPosition,
        name: String,
        params: Option<Value>,
    ) -> ScriptResult<Value> {
        self.record_fixture_viewports(&name);
        let params = fixture_params(params).map_err(|message| self.type_error(pos, message))?;
        let outcome = self
            .await_tool(pos, self.server.fixture_apply(name.clone(), Some(params)))
            .await?;
        self.record_fixture(name, outcome.params.clone());
        self.to_json(pos, outcome)
    }

    pub(super) fn fixtures(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.to_json(pos, self.server.inner.fixtures.fixtures_sorted())
    }

    pub(super) async fn diagnostic(
        &self,
        pos: ScriptPosition,
        name: String,
    ) -> ScriptResult<Value> {
        self.run_diagnostic(pos, &name)
            .await
            .map_err(|error| self.diagnostic_error(pos, error))
    }

    pub(super) async fn diagnostics(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        let mut values = BTreeMap::new();
        let mut errors = BTreeMap::new();
        for name in self.server.inner.diagnostics.names() {
            match self.run_diagnostic(pos, &name).await {
                Ok(value) => {
                    values.insert(name, value);
                }
                Err(error) => {
                    errors.insert(name, error);
                }
            }
        }
        self.to_json(
            pos,
            serde_json::json!({
                "values": values,
                "errors": errors,
            }),
        )
    }

    async fn run_diagnostic(&self, pos: ScriptPosition, name: &str) -> eguidev::DiagnosticResult {
        match self.server.inner.diagnostics.start(name) {
            DiagnosticExecution::Ready(result) => result,
            DiagnosticExecution::Queued(receiver) => {
                self.server.inner.request_repaint();
                let remaining = self.remaining_script_duration(pos).map_err(|error| {
                    eguidev::DiagnosticError::new(
                        error.code.as_deref().unwrap_or("timeout"),
                        error.message,
                    )
                })?;
                let remaining = remaining.unwrap_or(Duration::from_secs(24 * 60 * 60));
                spawn_blocking(move || receiver.recv_timeout(remaining))
                    .await
                    .map_err(|error| {
                        eguidev::DiagnosticError::new(
                            "internal",
                            format!("diagnostic wait task failed: {error}"),
                        )
                    })?
            }
        }
    }

    fn diagnostic_error(
        &self,
        pos: ScriptPosition,
        error: eguidev::DiagnosticError,
    ) -> ScriptErrorInfo {
        super::super::diagnostic::error_info(error, self.error_location(pos))
    }

    pub(super) fn dump(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let options =
            dump_options(pos, options).map_err(|message| self.type_error(pos, message))?;
        let dump = build_tree_dump(&self.server.inner, &options)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        self.to_json(pos, dump)
    }

    pub(super) fn dump_text(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let options =
            dump_options(pos, options).map_err(|message| self.type_error(pos, message))?;
        let dump = build_tree_dump(&self.server.inner, &options)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        self.to_json(pos, dump_text(&dump))
    }

    pub(super) fn record_assertion_outcome(
        &self,
        pos: ScriptPosition,
        passed: bool,
        message: String,
    ) {
        self.record_assertion(passed, message, pos);
    }
}

fn widget_values_match(current: &WidgetValue, expected: &WidgetValue) -> bool {
    match (current, expected) {
        (WidgetValue::Float(current), WidgetValue::Int(expected)) => {
            (*current - *expected as f64).abs() < f64::EPSILON
        }
        (WidgetValue::Int(current), WidgetValue::Float(expected)) => {
            (*current as f64 - *expected).abs() < f64::EPSILON
        }
        _ => current == expected,
    }
}

/// Compare two optional widget values, tolerating an int/float round trip.
fn optional_widget_values_match(before: Option<&WidgetValue>, after: Option<&WidgetValue>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => widget_values_match(before, after),
        _ => false,
    }
}

fn parse_sample_grid_count(value: &Value, name: &'static str) -> Result<usize, String> {
    let Some(number) = value.as_number() else {
        return Err(format!("sample_grid {name} must be a positive integer"));
    };
    let Some(count) = number.as_u64() else {
        return Err(format!("sample_grid {name} must be a positive integer"));
    };
    if count == 0 || count > MAX_SAMPLE_GRID_COUNT {
        return Err(format!(
            "sample_grid {name} must be an integer from 1 to {MAX_SAMPLE_GRID_COUNT}"
        ));
    }
    usize::try_from(count).map_err(|_| format!("sample_grid {name} is too large"))
}

fn scroll_offsets_match(current: Vec2, expected: Vec2) -> bool {
    (current.x - expected.x).abs() <= SCROLL_STABILITY_TOLERANCE
        && (current.y - expected.y).abs() <= SCROLL_STABILITY_TOLERANCE
}

fn diff_captures(before: &ScriptCapture, after: &ScriptCapture, options: &DiffOptions) -> TreeDiff {
    let before_viewports = before.viewports.iter().cloned().collect::<BTreeSet<_>>();
    let after_viewports = after.viewports.iter().cloned().collect::<BTreeSet<_>>();
    let before_widgets = capture_widget_map(before, options);
    let after_widgets = capture_widget_map(after, options);
    let keys = before_widgets
        .keys()
        .chain(after_widgets.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();

    for (viewport_id, id) in keys {
        match (
            before_widgets.get(&(viewport_id.clone(), id.clone())),
            after_widgets.get(&(viewport_id.clone(), id.clone())),
        ) {
            (None, Some(after)) => changes.push(WidgetDelta {
                id,
                viewport_id,
                change: "added",
                fields: Vec::new(),
                before: None,
                after: Some(after.state.clone()),
            }),
            (Some(before), None) => changes.push(WidgetDelta {
                id,
                viewport_id,
                change: "removed",
                fields: Vec::new(),
                before: Some(before.state.clone()),
                after: None,
            }),
            (Some(before), Some(after)) => {
                let fields =
                    changed_widget_fields(&before.state, &after.state, options.move_epsilon);
                if !fields.is_empty() {
                    changes.push(WidgetDelta {
                        id,
                        viewport_id,
                        change: "changed",
                        fields,
                        before: Some(before.state.clone()),
                        after: Some(after.state.clone()),
                    });
                }
            }
            (None, None) => {}
        }
    }

    TreeDiff {
        changes,
        viewports_added: after_viewports
            .difference(&before_viewports)
            .cloned()
            .collect(),
        viewports_removed: before_viewports
            .difference(&after_viewports)
            .cloned()
            .collect(),
    }
}

fn capture_widget_map(
    capture: &ScriptCapture,
    options: &DiffOptions,
) -> BTreeMap<(String, String), ScriptCapturedWidget> {
    capture
        .widgets
        .iter()
        .filter(|widget| options.include_invisible || widget.state.visible)
        .filter(|widget| {
            options
                .id_prefix
                .as_deref()
                .is_none_or(|prefix| widget.id.starts_with(prefix))
        })
        .map(|widget| {
            (
                (widget.viewport_id.clone(), widget.id.clone()),
                widget.clone(),
            )
        })
        .collect()
}

fn changed_widget_fields(
    before: &WidgetState,
    after: &WidgetState,
    move_epsilon: f32,
) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if before.role != after.role {
        fields.push("role");
    }
    if rect_changed(&before.rect, &after.rect, move_epsilon) {
        fields.push("rect");
    }
    if before.visible != after.visible {
        fields.push("visible");
    }
    if before.enabled != after.enabled {
        fields.push("enabled");
    }
    if before.focused != after.focused {
        fields.push("focused");
    }
    if before.selected != after.selected {
        fields.push("selected");
    }
    if before.label != after.label {
        fields.push("label");
    }
    // A capture crosses into Luau, where every number is a float, so a value
    // is compared by what it holds rather than by which variant carries it.
    if !optional_widget_values_match(before.value.as_ref(), after.value.as_ref()) {
        fields.push("value");
    }
    if before.value_text != after.value_text {
        fields.push("value_text");
    }
    if before.data != after.data {
        fields.push("data");
    }
    if before.parent_id != after.parent_id {
        fields.push("parent_id");
    }
    if before.child_ids != after.child_ids {
        fields.push("child_ids");
    }
    fields
}

fn rect_changed(before: &Rect, after: &Rect, epsilon: f32) -> bool {
    (before.min.x - after.min.x).abs() > epsilon
        || (before.min.y - after.min.y).abs() > epsilon
        || (before.max.x - after.max.x).abs() > epsilon
        || (before.max.y - after.max.y).abs() > epsilon
}

fn image_ref_json(id: String) -> Value {
    serde_json::json!({
        "type": "image_ref",
        "id": id,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio::task::yield_now;

    use super::{
        MAX_SAMPLE_GRID_COUNT, ScriptRuntime, changed_widget_fields, parse_sample_grid_count,
    };
    use crate::{
        EguiDiagnosticBatch, EguiDiagnosticKind,
        automation::script::types::ScriptPosition,
        registry::Inner,
        runtime::Runtime,
        types::{Pos2, Rect, WidgetRegistryEntry, WidgetRole, WidgetState, WidgetValue},
    };

    fn assert_send_sync<T: Send + Sync>() {}

    fn state(value: Option<WidgetValue>) -> WidgetState {
        let rect = Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        };
        WidgetState::from(&WidgetRegistryEntry {
            id: "slider".to_string(),
            explicit_id: true,
            native_id: 1,
            viewport_id: "root".to_string(),
            layer_id: "layer".to_string(),
            layer_order: 0,
            rect,
            interact_rect: rect,
            role: WidgetRole::Slider,
            label: None,
            value,
            data: None,
            layout: None,
            role_state: None,
            parent_id: None,
            enabled: true,
            visible: true,
            focused: false,
        })
    }

    fn id_clash_output() -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.warn_on_id_clash = true);
        // egui only carries rectangle-identity warnings in debug builds, so
        // this isolation is both unavailable and unnecessary in release.
        #[cfg(debug_assertions)]
        ctx.all_styles_mut(|style| style.debug.warn_if_rect_changes_id = false);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let id = egui::Id::new("duplicate");
            ui.ctx().check_for_id_clash(
                id,
                egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(20.0, 20.0)),
                "test widget",
            );
            ui.ctx().check_for_id_clash(
                id,
                egui::Rect::from_min_size(egui::pos2(60.0, 10.0), egui::vec2(20.0, 20.0)),
                "test widget",
            );
        });
        output.textures_delta.clear();
        output
    }

    fn script_runtime(timeout_ms: u64) -> (Arc<Runtime>, Arc<ScriptRuntime>) {
        let inner = Arc::new(Inner::new());
        inner.remember_context(egui::ViewportId::ROOT, &egui::Context::default());
        let runtime = Runtime::ensure_for_inner_with_diagnostic_barriers(&inner);
        let script = Arc::new(ScriptRuntime::new(
            inner,
            Arc::clone(&runtime),
            "diagnostics.luau".to_string(),
            timeout_ms,
        ));
        (runtime, script)
    }

    #[test]
    fn parse_sample_grid_count_rejects_axis_over_cap() {
        assert!(parse_sample_grid_count(&json!(0), "nx").is_err());
        assert!(parse_sample_grid_count(&json!(MAX_SAMPLE_GRID_COUNT + 1), "nx").is_err());
        assert_eq!(
            parse_sample_grid_count(&json!(MAX_SAMPLE_GRID_COUNT), "nx").expect("max"),
            MAX_SAMPLE_GRID_COUNT as usize
        );
    }

    #[test]
    fn script_runtime_is_send_sync() {
        assert_send_sync::<ScriptRuntime>();
    }

    #[test]
    fn evaluations_start_at_independent_journal_tails() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let first = ScriptRuntime::new(
            Arc::clone(&inner),
            Arc::clone(&runtime),
            "first.luau".to_string(),
            1_000,
        );
        runtime
            .egui_diagnostics()
            .record_output("root".to_string(), 1, &id_clash_output());
        let second = ScriptRuntime::new(
            inner,
            Arc::clone(&runtime),
            "second.luau".to_string(),
            1_000,
        );
        runtime
            .egui_diagnostics()
            .record_output("root".to_string(), 2, &id_clash_output());

        assert!(
            first.diagnostic_selection(None).batch.entries.len()
                > second.diagnostic_selection(None).batch.entries.len()
        );
        assert!(
            second
                .diagnostic_selection(None)
                .batch
                .entries
                .iter()
                .all(|entry| entry.frame == 2)
        );
    }

    #[tokio::test]
    async fn viewport_read_waits_for_output_and_dismisses_returned_entries() {
        let (runtime, script) = script_runtime(1_000);
        runtime
            .egui_diagnostics()
            .record_output("root".to_string(), 1, &id_clash_output());

        let read_script = Arc::clone(&script);
        let read = tokio::spawn(async move {
            read_script
                .egui_diagnostics(ScriptPosition::default(), "root".to_string())
                .await
        });
        yield_now().await;
        runtime.egui_diagnostics().record_output(
            "root".to_string(),
            2,
            &egui::FullOutput::default(),
        );

        let value = read.await.expect("read task").expect("diagnostic read");
        let batch: EguiDiagnosticBatch = serde_json::from_value(value).expect("diagnostic batch");
        assert!(
            batch
                .entries
                .iter()
                .all(|entry| entry.kind == EguiDiagnosticKind::IdClash)
        );
        assert!(script.diagnostic_selection(None).batch.is_empty());
    }

    #[tokio::test]
    async fn viewport_clear_dismisses_without_returning_entries() {
        let (runtime, script) = script_runtime(1_000);
        runtime
            .egui_diagnostics()
            .record_output("root".to_string(), 1, &id_clash_output());

        let clear_script = Arc::clone(&script);
        let clear = tokio::spawn(async move {
            clear_script
                .clear_egui_diagnostics(ScriptPosition::default(), "root".to_string())
                .await
        });
        yield_now().await;
        runtime.egui_diagnostics().record_output(
            "root".to_string(),
            2,
            &egui::FullOutput::default(),
        );

        clear.await.expect("clear task").expect("diagnostic clear");
        assert!(script.diagnostic_selection(None).batch.is_empty());
    }

    #[test]
    fn diff_ignores_the_int_float_round_trip_a_capture_takes_through_luau() {
        // Luau has one number type, so a captured Float(42.0) returns as Int(42).
        let before = state(Some(WidgetValue::Int(42)));
        let after = state(Some(WidgetValue::Float(42.0)));
        assert!(changed_widget_fields(&before, &after, 0.5).is_empty());
    }

    #[test]
    fn diff_still_reports_a_real_value_change() {
        let before = state(Some(WidgetValue::Float(42.0)));
        let after = state(Some(WidgetValue::Float(43.0)));
        assert_eq!(
            changed_widget_fields(&before, &after, 0.5),
            vec!["value", "value_text"]
        );

        let cleared = state(None);
        assert_eq!(
            changed_widget_fields(&before, &cleared, 0.5),
            vec!["value", "value_text"]
        );
    }
}
