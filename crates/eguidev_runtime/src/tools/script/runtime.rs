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

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tmcp::ToolResult;
use tokio::{task::spawn_blocking, time::timeout};

use super::{
    super::{
        DEFAULT_POLL_INTERVAL_MS, DEFAULT_WAIT_TIMEOUT_MS, DevMcpServer, ErrorCode,
        OverlayDebugOptionsInput, SCROLL_STABILITY_TOLERANCE, ToolError, capture_screenshot,
        collect_widget_list, parse_key_combo, resolve_screenshot_viewport,
        resolve_widget_and_viewport, viewport_snapshot_for, wait_timeout_details,
        wait_timeout_message,
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
    registry::{Inner, viewport_id_to_string},
    runtime::Runtime,
    screenshots::ScreenshotKind,
    types::{Modifiers, Rect, Vec2, WidgetRef, WidgetRegistryEntry, WidgetState, WidgetValue},
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
    deadline: Instant,
    script_timeout_ms: u64,
    config_timeout_ms: Mutex<Option<u64>>,
    config_poll_interval_ms: Mutex<Option<u64>>,
    config_settle: Mutex<Option<bool>>,
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

fn resolve_viewport_id(
    inner: &Inner,
    viewport_id: Option<String>,
) -> Result<egui::ViewportId, ToolError> {
    inner
        .viewports
        .resolve_viewport_id(viewport_id)
        .map_err(Into::into)
}

fn unique_viewport_lookup(
    matches: Vec<&ViewportSnapshot>,
    selector: String,
) -> Result<Option<&ViewportSnapshot>, String> {
    match matches.as_slice() {
        [] => Ok(None),
        [snapshot] => Ok(Some(*snapshot)),
        _ => Err(format!("multiple viewports matched {selector}")),
    }
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
    pub(super) fn new(
        inner: Arc<Inner>,
        runtime: Arc<Runtime>,
        source_name: String,
        timeout_ms: u64,
    ) -> Self {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(timeout_ms))
            .unwrap_or_else(Instant::now);
        Self {
            server: DevMcpServer::with_runtime(inner, runtime),
            logs: Mutex::new(Vec::new()),
            assertions: Mutex::new(Vec::new()),
            fixtures: Mutex::new(Vec::new()),
            images: Mutex::new(Vec::new()),
            image_counter: AtomicUsize::new(0),
            source_name,
            deadline,
            script_timeout_ms: timeout_ms,
            config_timeout_ms: Mutex::new(None),
            config_poll_interval_ms: Mutex::new(None),
            config_settle: Mutex::new(None),
        }
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

    fn assertion_error(&self, pos: ScriptPosition, message: impl Into<String>) -> ScriptErrorInfo {
        ScriptErrorInfo {
            error_type: "assertion".to_string(),
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
        let details = error
            .structured
            .as_ref()
            .and_then(|structured| structured.get("error"))
            .and_then(|structured| structured.get("details"))
            .cloned();
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

    fn remaining_script_duration(&self, pos: ScriptPosition) -> ScriptResult<Duration> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            return Err(self.script_timeout_error(pos));
        }
        Ok(remaining)
    }

    async fn await_tool<T>(
        &self,
        pos: ScriptPosition,
        fut: impl Future<Output = ToolResult<T>>,
    ) -> ScriptResult<T> {
        let remaining = self.remaining_script_duration(pos)?;
        let result = timeout(remaining, fut).await.map_err(|_| {
            self.tool_error(
                pos,
                ToolError::new(
                    ErrorCode::Timeout,
                    "Script deadline exceeded while waiting for tool call",
                )
                .into_tmcp(),
            )
        })?;
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
        self.to_json(pos, WidgetState::from(widget))
    }

    fn widget_handle_list_json(
        &self,
        pos: ScriptPosition,
        widgets: &[WidgetRegistryEntry],
    ) -> ScriptResult<Value> {
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
        let input = self.server.inner.viewports.input_snapshot(
            resolve_viewport_id(&self.server.inner, Some(snapshot.viewport_id.clone()))
                .unwrap_or_default(),
        );
        self.to_json(
            pos,
            serde_json::json!({
                "name": snapshot.name,
                "title": snapshot.title,
                "outer_pos": Value::Null,
                "outer_size": snapshot.outer_size,
                "inner_size": snapshot.inner_size,
                "focused": snapshot.focused,
                "minimized": snapshot.minimized,
                "occluded": snapshot.occluded,
                "os_minimized": snapshot.os_minimized,
                "os_occluded": snapshot.os_occluded,
                "maximized": snapshot.maximized,
                "fullscreen": snapshot.fullscreen,
                "frame_count": self.server.inner.frame_count(),
                "pixels_per_point": input.as_ref().map(|i| i.pixels_per_point).unwrap_or(1.0),
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
        let timeout_ms = parse_optional_u64(options, "timeout_ms")
            .map_err(|error| self.type_error(pos, error.message))?
            .or_else(|| self.configured_timeout_ms());
        let poll_interval_ms = parse_optional_u64(options, "poll_interval_ms")
            .map_err(|error| self.type_error(pos, error.message))?
            .or_else(|| self.configured_poll_interval_ms());
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
        Ok(viewport_id_to_string(resolved_viewport_id))
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
        let timeout_ms = self.configured_timeout_ms();
        let poll_interval_ms = self.configured_poll_interval_ms();
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
        let widgets = collect_widget_list(
            &self.server.inner,
            viewport_id,
            include_invisible,
            role,
            id_prefix.as_deref(),
            label.as_deref(),
            label_contains.as_deref(),
        )
        .map_err(|error| self.tool_error(pos, error))?;
        self.widget_handle_list_json(pos, &widgets)
    }

    pub(super) fn widget_get(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = self.parse_optional_viewport_option(pos, options)?;
        let result = self
            .server
            .widget_get_result(viewport_id.as_deref(), &target)
            .map_err(|error| self.tool_error(pos, error))?;
        self.widget_handle_json(pos, &result.widget)
    }

    pub(super) async fn widget_find(
        &self,
        pos: ScriptPosition,
        id: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let target = parse_widget_ref(&Value::String(id))
            .map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let widget = self
            .await_tool(
                pos,
                self.server.wait_for_widget_state(
                    viewport_id,
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    |widget| widget.is_some(),
                ),
            )
            .await?
            .ok_or_else(|| self.runtime_error(pos, "widget wait matched without a widget"))?;
        self.widget_handle_json(pos, &widget)
    }

    pub(super) fn try_widget_find(
        &self,
        pos: ScriptPosition,
        id: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let target = parse_widget_ref(&Value::String(id))
            .map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = self.parse_optional_viewport_option(pos, options)?;
        let result = match viewport_id.as_deref() {
            Some(viewport_id) => self
                .server
                .widget_get_result(Some(viewport_id), &target)
                .map(|result| result.widget),
            None => self
                .server
                .inner
                .widgets
                .resolve_widget_global(&self.server.inner.viewports, &target)
                .map_err(|error| tmcp::ToolError::from(ToolError::from(error))),
        };
        match result {
            Ok(widget) => self.widget_handle_json(pos, &widget),
            Err(error) if error.code == "not_found" => Ok(Value::Null),
            Err(error) => Err(self.tool_error(pos, error)),
        }
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
            let timeout_ms = self.configured_timeout_ms();
            let poll_interval_ms = self.configured_poll_interval_ms();
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
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
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let action_viewport_id = self.resolve_target_viewport(pos, None, &target)?;
        self.await_tool(
            pos,
            self.server.action_focus(Some(action_viewport_id), target),
        )
        .await?;
        self.to_json(pos, ())
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
            let timeout_ms = self.configured_timeout_ms();
            let poll_interval_ms = self.configured_poll_interval_ms();
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
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
                .action_scroll_into_view(Some(action_viewport_id.clone()), target),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id), (), None)
            .await
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

    pub(super) fn root_viewport(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.viewport_handle_json(pos, "root")
    }

    pub(super) fn viewport_lookup(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let title = parse_optional_string(options, "title")
            .map_err(|error| self.type_error(pos, error.message))?;
        let title_contains = parse_optional_string(options, "title_contains")
            .map_err(|error| self.type_error(pos, error.message))?;
        let name = parse_optional_string(options, "name")
            .map_err(|error| self.type_error(pos, error.message))?;
        let focused = parse_optional_bool(options, "focused")
            .map_err(|error| self.type_error(pos, error.message))?;
        if let Some(error) = self.server.inner.viewports.viewport_name_error() {
            return Err(self.tool_error(pos, ToolError::from(error).into()));
        }
        if name.is_none() && title.is_none() && title_contains.is_none() && focused.is_none() {
            return Err(self.type_error(
                pos,
                "viewport requires name, focused, title, or title_contains",
            ));
        }
        let snapshots = self.server.inner.viewports.viewports_snapshot();
        let mut selector = Vec::new();
        let mut matches = if let Some(name) = name.as_deref() {
            selector.push(format!("name {name:?}"));
            snapshots
                .iter()
                .filter(|snapshot| snapshot.name.as_deref() == Some(name))
                .collect::<Vec<_>>()
        } else if let Some(title) = title.as_deref() {
            let exact = snapshots
                .iter()
                .filter(|snapshot| snapshot.title.as_deref() == Some(title))
                .collect::<Vec<_>>();
            if !exact.is_empty() || title_contains.is_none() {
                selector.push(format!("title {title:?}"));
                exact
            } else if let Some(needle) = title_contains.as_deref() {
                selector.push(format!("title_contains {needle:?}"));
                snapshots
                    .iter()
                    .filter(|snapshot| {
                        snapshot
                            .title
                            .as_deref()
                            .is_some_and(|title| title.contains(needle))
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        } else if let Some(needle) = title_contains.as_deref() {
            selector.push(format!("title_contains {needle:?}"));
            snapshots
                .iter()
                .filter(|snapshot| {
                    snapshot
                        .title
                        .as_deref()
                        .is_some_and(|title| title.contains(needle))
                })
                .collect::<Vec<_>>()
        } else {
            snapshots.iter().collect::<Vec<_>>()
        };
        if let Some(focused) = focused {
            selector.push(format!("focused {focused}"));
            matches.retain(|snapshot| snapshot.focused == focused);
        }
        let selector = selector.join(", ");
        unique_viewport_lookup(matches, selector)
            .map_err(|error| self.runtime_error(pos, error))?
            .map_or(Ok(Value::Null), |snapshot| {
                self.viewport_handle_json(pos, &snapshot.viewport_id)
            })
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
        let resolved = resolve_viewport_id(&self.server.inner, Some(viewport_id.clone()))
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let snapshot = viewport_snapshot_for(&self.server.inner, resolved).ok_or_else(|| {
            self.runtime_error(pos, format!("Viewport `{viewport_id}` is not available"))
        })?;
        self.viewport_state_json(pos, &snapshot)
    }

    pub(super) fn widget_state(&self, pos: ScriptPosition, target: &Value) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let result = self
            .server
            .widget_get_result(None, &target)
            .map_err(|error| self.tool_error(pos, error))?;
        self.widget_state_json(pos, &result.widget)
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

    pub(super) async fn raw_pointer_move(
        &self,
        pos: ScriptPosition,
        pos_arg: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let point = parse_pos2(pos_arg).map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.input_pointer_move(viewport_id, point))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn raw_pointer_button(
        &self,
        pos: ScriptPosition,
        pos_arg: &Value,
        button: &Value,
        pressed: bool,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let point = parse_pos2(pos_arg).map_err(|error| self.type_error(pos, error.message))?;
        let button =
            parse_pointer_button(button).map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server
                .input_pointer_button(viewport_id, point, button, pressed, modifiers),
        )
        .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn raw_key(
        &self,
        pos: ScriptPosition,
        key: String,
        pressed: bool,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.input_key(viewport_id, key, pressed, modifiers),
        )
        .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn raw_text(
        &self,
        pos: ScriptPosition,
        text: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.input_text(viewport_id, text))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn raw_scroll(
        &self,
        pos: ScriptPosition,
        delta: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let delta = parse_vec2(delta).map_err(|error| self.type_error(pos, error.message))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers = self
            .modifiers_from_options(options)
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.input_scroll(viewport_id, delta, modifiers))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn wait_for_widget_predicate<F, Fut>(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
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
            })
            .or(Some(egui::ViewportId::ROOT));
        let result = super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            Some(self.deadline),
            || {
                let predicate = predicate.clone();
                let target = target.clone();
                let viewport_id = viewport_id.clone();
                async move {
                    match resolve_widget_and_viewport(
                        &self.server.inner,
                        viewport_id.as_deref(),
                        &target,
                    )
                    .map(|(widget, _)| widget)
                    {
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
                        Err(error) if error.code == ErrorCode::NotFound => Ok((false, None)),
                        Err(error) => Err(self.tool_error(pos, error.into())),
                    }
                }
            },
        )
        .await;

        match result {
            Ok((matched, widget, elapsed_ms, observation)) => {
                if !matched && self.deadline <= Instant::now() {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    self.to_json(pos, widget.as_ref().map(WidgetState::from))
                } else {
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            wait_timeout_message(
                                format!(
                                    "Timed out waiting for widget predicate after {timeout_ms}ms"
                                ),
                                &observation,
                            ),
                        )
                        .with_details(wait_timeout_details(
                            "widget",
                            elapsed_ms,
                            widget.as_ref(),
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

    pub(super) async fn wait_for_widget_visible(
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
            })
            .or(Some(egui::ViewportId::ROOT));
        let result = super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            Some(self.deadline),
            || {
                let result = match resolve_widget_and_viewport(
                    &self.server.inner,
                    viewport_id.as_deref(),
                    &target,
                )
                .map(|(widget, _)| widget)
                {
                    Ok(widget) => Ok::<_, ScriptErrorInfo>((widget.visible, Some(widget))),
                    Err(error) if error.code == ErrorCode::NotFound => Ok((false, None)),
                    Err(error) => Err(self.tool_error(pos, error.into())),
                };
                async move { result }
            },
        )
        .await;

        match result {
            Ok((matched, widget, elapsed_ms, observation)) => {
                if !matched && self.deadline <= Instant::now() {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    if let Some(widget) = widget.as_ref() {
                        self.widget_state_json(pos, widget)
                    } else {
                        Err(self.runtime_error(
                            pos,
                            "wait_for_widget_visible matched without a widget snapshot",
                        ))
                    }
                } else {
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            wait_timeout_message(
                                format!(
                                    "Timed out waiting for widget visibility predicate after {timeout_ms}ms"
                                ),
                                &observation,
                            ),
                        )
                        .with_details(wait_timeout_details(
                            "widget_visible",
                            elapsed_ms,
                            widget.as_ref(),
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
            })
            .or(Some(egui::ViewportId::ROOT));
        let result = super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            Some(self.deadline),
            || {
                let result = match resolve_widget_and_viewport(
                    &self.server.inner,
                    viewport_id.as_deref(),
                    &target,
                )
                .map(|(widget, _)| widget)
                {
                    Ok(widget) => Ok::<_, ScriptErrorInfo>((false, Some(widget))),
                    Err(error) if error.code == ErrorCode::NotFound => Ok((true, None)),
                    Err(error) => Err(self.tool_error(pos, error.into())),
                };
                async move { result }
            },
        )
        .await;

        match result {
            Ok((matched, _widget, elapsed_ms, observation)) => {
                if !matched && self.deadline <= Instant::now() {
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
    ) -> ScriptResult<Value> {
        let count =
            parse_optional_u64_val(count).map_err(|error| self.type_error(pos, error.message))?;
        let timeout_ms = self.configured_timeout_ms();
        let result = self
            .await_tool(pos, self.server.wait_for_frame_count(count, timeout_ms))
            .await?;
        self.to_json(pos, result)
    }

    pub(super) async fn wait_for_capture(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        self.await_tool(
            pos,
            self.server
                .wait_for_capture(viewport_id, timeout_ms, poll_interval_ms),
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

    pub(super) async fn wait_for_scroll_ready(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let widget = self
            .await_tool(
                pos,
                self.server.wait_for_scroll_ready(
                    viewport_id,
                    target,
                    timeout_ms,
                    poll_interval_ms,
                ),
            )
            .await?;
        let Some(widget) = widget else {
            return Err(self.runtime_error(
                pos,
                "wait_for_scroll_ready matched without a widget snapshot",
            ));
        };
        self.widget_state_json(pos, &widget)
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
            Some(self.deadline),
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
                        None => Err(self.tool_error(
                            pos,
                            ToolError::new(ErrorCode::InvalidRef, "Viewport not ready for wait")
                                .into_tmcp(),
                        )),
                    }
                }
            },
        )
        .await;

        match result {
            Ok((matched, viewport, elapsed_ms, observation)) => {
                if !matched && self.deadline <= Instant::now() {
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

    /// Hide a widget's highlight.
    pub(super) async fn hide_highlight_widget(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.hide_highlight(viewport_id, Some(target)))
            .await?;
        Ok(Value::Null)
    }

    /// Clear all highlights.
    pub(super) async fn hide_highlight_all(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.hide_highlight(None, None))
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

    pub(super) async fn hide_debug_overlay(&self, pos: ScriptPosition) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.hide_debug_overlay())
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn viewport_set_inner_size(
        &self,
        pos: ScriptPosition,
        size: &Value,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let size = parse_vec2(size).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.viewport_set_inner_size(viewport_id, size))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn viewport_set_resize_options(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let min_size = parse_optional_vec2(options, "min_size")
            .map_err(|error| self.type_error(pos, error.message))?;
        let max_size = parse_optional_vec2(options, "max_size")
            .map_err(|error| self.type_error(pos, error.message))?;
        let increments = parse_optional_vec2(options, "increments")
            .map_err(|error| self.type_error(pos, error.message))?;
        let resizable = parse_optional_bool(options, "resizable")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.viewport_set_resize_options(
                viewport_id,
                min_size,
                max_size,
                increments,
                resizable,
            ),
        )
        .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn focus_window(
        &self,
        pos: ScriptPosition,
        viewport: String,
    ) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.focus_window(viewport))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn viewport_dismiss_popups(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.viewport_dismiss_popups(viewport_id))
            .await?;
        self.to_json(pos, ())
    }

    pub(super) async fn fixture(
        &self,
        pos: ScriptPosition,
        name: String,
        params: Option<Value>,
    ) -> ScriptResult<Value> {
        let timeout_ms = self.configured_timeout_ms();
        let params = fixture_params(params).map_err(|message| self.type_error(pos, message))?;
        let outcome = self
            .await_tool(
                pos,
                self.server.fixture(name.clone(), Some(params), timeout_ms),
            )
            .await?;
        self.record_fixture(name, outcome.params.clone());
        self.to_json(pos, outcome.values)
    }

    pub(super) async fn fixture_raw(
        &self,
        pos: ScriptPosition,
        name: String,
        params: Option<Value>,
    ) -> ScriptResult<Value> {
        let params = fixture_params(params).map_err(|message| self.type_error(pos, message))?;
        let outcome = self
            .await_tool(pos, self.server.fixture_apply(name.clone(), Some(params)))
            .await?;
        self.record_fixture(name, outcome.params);
        self.to_json(pos, ())
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
        ScriptErrorInfo {
            error_type: if error.code == "timeout" {
                "timeout".to_string()
            } else {
                "diagnostic".to_string()
            },
            message: error.message,
            location: self.error_location(pos),
            backtrace: None,
            code: Some(error.code),
            details: error.details,
        }
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

    fn assert_result(
        &self,
        pos: ScriptPosition,
        passed: bool,
        message: String,
    ) -> ScriptResult<()> {
        self.record_assertion(passed, message.clone(), pos);
        if passed {
            Ok(())
        } else {
            Err(self.assertion_error(pos, message))
        }
    }

    pub(super) fn assert_condition(
        &self,
        pos: ScriptPosition,
        condition: bool,
        message: Option<String>,
    ) -> ScriptResult<()> {
        let message = message.unwrap_or_else(|| {
            if condition {
                "assertion passed".to_string()
            } else {
                "assertion failed".to_string()
            }
        });
        self.assert_result(pos, condition, message)
    }

    pub(super) async fn assert_widget_exists(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<()> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        self.await_tool(
            pos,
            self.server.wait_for_widget_state(
                viewport_id,
                target,
                timeout_ms,
                poll_interval_ms,
                |widget| widget.is_some(),
            ),
        )
        .await?;
        self.assert_result(pos, true, "widget exists".to_string())
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

fn parse_sample_grid_count(value: &Value, name: &'static str) -> Result<usize, String> {
    let Some(number) = value.as_number() else {
        return Err(format!("sample_grid {name} must be a positive integer"));
    };
    let Some(count) = number.as_u64() else {
        return Err(format!("sample_grid {name} must be a positive integer"));
    };
    if count == 0 || count > usize::MAX as u64 {
        return Err(format!("sample_grid {name} must be a positive integer"));
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
    if before.value != after.value {
        fields.push("value");
    }
    if before.value_text != after.value_text {
        fields.push("value_text");
    }
    if before.data != after.data {
        fields.push("data");
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
    use super::ScriptRuntime;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn script_runtime_is_send_sync() {
        assert_send_sync::<ScriptRuntime>();
    }
}
