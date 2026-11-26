#![allow(
    clippy::multiple_inherent_impl,
    clippy::needless_pass_by_value,
    clippy::result_large_err
)]

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
use serde::Serialize;
use serde_json::{Map, Value};
use tmcp::ToolResult;
use tokio::time::timeout;

use super::{
    super::{
        DEFAULT_POLL_INTERVAL_MS, DEFAULT_WAIT_TIMEOUT_MS, DevMcpServer, ErrorCode, HoverConfirm,
        MAX_SAMPLE_GRID_COUNT, OverlayDebugOptionsInput, SCROLL_STABILITY_TOLERANCE,
        ScreenshotFormat, ScreenshotOptions, ToolError, capture_native_screenshot,
        capture_screenshot, collect_widget_list, interaction_ready, parse_key_combo,
        resolve_screenshot_viewport, resolve_widget_and_viewport, viewport_snapshot_for,
        wait_timeout_details, wait_timeout_message, widget_wait_timeout_message,
        with_widget_target,
    },
    parse::{
        map_value, parse_modifiers, parse_optional_bool, parse_optional_f32, parse_optional_string,
        parse_optional_u8, parse_optional_u32, parse_optional_u64, parse_optional_vec2,
        parse_overlay_mode, parse_pointer_button, parse_pos2, parse_scroll_align, parse_vec2,
        parse_widget_ref, parse_widget_role, widget_value_from_dynamic,
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
    registry::{Inner, lock, viewport_id_to_string},
    runtime::Runtime,
    screenshots::ScreenshotKind,
    types::{
        ActionOptions, Pos2, RawInputEvent, Rect, ResizeOptions, RoleState, Vec2, WidgetRef,
        WidgetRegistryEntry, WidgetState, WidgetValue,
    },
    viewports::ViewportSnapshot,
};

mod actions;
mod captures;
mod fixtures;
mod queries;
mod waits;

/// One registry snapshot that `capture()` took. The runtime keeps it for the
/// evaluation, so that a capture never sends its widgets through Luau.
#[derive(Clone, Debug)]
struct ScriptCapture {
    frame: u64,
    widgets: Vec<ScriptCapturedWidget>,
    viewports: Vec<String>,
}

#[derive(Clone, Debug)]
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
    /// The snapshots of this evaluation, by the opaque ID of each capture.
    captures: Mutex<BTreeMap<u64, ScriptCapture>>,
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
            captures: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn restore_automation_options(&self) {
        self.server
            .inner
            .set_automation_options(self.saved_automation_options);
    }

    pub(super) fn record_targeted_viewport(&self, viewport_id: impl Into<String>) {
        lock(&self.targeted_viewports, "targeted viewports lock").insert(viewport_id.into());
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
        lock(&self.targeted_viewports, "targeted viewports lock").clone()
    }

    fn diagnostic_selection(&self, viewport_id: Option<&str>) -> DiagnosticSelection {
        let dismissed = lock(
            &self.dismissed_egui_diagnostics,
            "dismissed diagnostics lock",
        );
        self.server.runtime.egui_diagnostics().select(
            self.egui_diagnostic_start,
            &dismissed,
            viewport_id,
        )
    }

    fn dismiss_diagnostic_sequences(&self, sequences: impl IntoIterator<Item = u64>) {
        lock(
            &self.dismissed_egui_diagnostics,
            "dismissed diagnostics lock",
        )
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
        let (resolved, mut batch) = self.take_egui_diagnostics(pos, viewport_id).await?;
        batch.warnings_enabled = self
            .server
            .inner
            .context_for(resolved)
            .map(|ctx| ctx.options(|options| options.warn_on_id_clash));
        self.to_json(pos, batch)
    }

    pub(super) async fn clear_egui_diagnostics(
        &self,
        pos: ScriptPosition,
        viewport_id: String,
    ) -> ScriptResult<()> {
        self.take_egui_diagnostics(pos, viewport_id).await?;
        Ok(())
    }

    /// Wait for the diagnostics of one viewport, dismiss them, and return the
    /// resolved viewport with its batch.
    async fn take_egui_diagnostics(
        &self,
        pos: ScriptPosition,
        viewport_id: String,
    ) -> ScriptResult<(egui::ViewportId, EguiDiagnosticBatch)> {
        let resolved = resolve_viewport_id(&self.server.inner, Some(viewport_id))
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let viewport_id = viewport_id_to_string(resolved);
        self.record_targeted_viewport(viewport_id.clone());
        self.await_diagnostic_outputs(&BTreeSet::from([viewport_id.clone()]))
            .await?;
        let selection = self.diagnostic_selection(Some(&viewport_id));
        self.dismiss_diagnostic_sequences(selection.sequences);
        Ok((resolved, selection.batch))
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
            *lock(&self.config_timeout_ms, "script timeout lock") = Some(timeout_ms);
        }
        if let Some(poll_interval_ms) = parse_optional_u64(options, "poll_interval_ms")
            .map_err(|error| self.type_error(pos, error.message))?
        {
            *lock(&self.config_poll_interval_ms, "script poll interval lock") =
                Some(poll_interval_ms);
        }
        if let Some(settle) = parse_optional_bool(options, "settle")
            .map_err(|error| self.type_error(pos, error.message))?
        {
            *lock(&self.config_settle, "script settle lock") = Some(settle);
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
        *lock(&self.config_timeout_ms, "script timeout lock")
    }

    fn configured_poll_interval_ms(&self) -> Option<u64> {
        *lock(&self.config_poll_interval_ms, "script poll interval lock")
    }

    fn configured_settle(&self) -> bool {
        lock(&self.config_settle, "script settle lock").unwrap_or(true)
    }

    pub(super) fn log(&self, line: impl Into<String>) {
        let mut logs = lock(&self.logs, "script logs lock");
        logs.push(line.into());
    }

    pub(super) fn logs(&self) -> Vec<String> {
        lock(&self.logs, "script logs lock").clone()
    }

    pub(super) fn assertions(&self) -> Vec<ScriptAssertion> {
        lock(&self.assertions, "script assertions lock").clone()
    }

    pub(super) fn fixture_applications(&self) -> Vec<FixtureApplication> {
        lock(&self.fixtures, "script fixtures lock").clone()
    }

    fn record_fixture(&self, name: String, params: BTreeMap<String, WidgetValue>) {
        let mut fixtures = lock(&self.fixtures, "script fixtures lock");
        fixtures.push(FixtureApplication { name, params });
    }

    pub(super) fn record_assertion(&self, pos: ScriptPosition, passed: bool, message: String) {
        let location = self.format_location(pos);
        let assertion = ScriptAssertion {
            passed,
            message,
            location,
        };
        let mut assertions = lock(&self.assertions, "script assertions lock");
        assertions.push(assertion);
    }

    pub(super) fn images(&self) -> Vec<ImageCapture> {
        lock(&self.images, "script images lock").clone()
    }

    fn next_image_id(&self) -> String {
        let id = self.image_counter.fetch_add(1, Ordering::Relaxed);
        format!("img_{id}")
    }

    fn store_image(&self, image: ImageCapture) {
        let mut images = lock(&self.images, "script images lock");
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

    pub(super) fn viewport_handle_json(
        &self,
        pos: ScriptPosition,
        viewport_id: &str,
    ) -> ScriptResult<Value> {
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
        let output = resolved
            .and_then(|viewport_id| self.server.inner.viewports.output_snapshot(viewport_id));
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
                "os_title_visible": snapshot.os_title_visible,
                "cursor_icon": output.map(|output| format!("{:?}", output.cursor_icon)),
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
        parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))
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

    async fn finish_action(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        self.settle_after_action(pos, options, viewport_id).await?;
        self.to_json(pos, ())
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

#[cfg(test)]
mod tests;
