//! Captures of the script runtime: tree captures, screenshots, pixel samples,
//! layout checks, and overlays.

use super::*;

impl ScriptRuntime {
    /// Take a snapshot, keep it for the evaluation, and return its frame and
    /// its opaque ID.
    pub(in super::super) fn capture(&self) -> Value {
        let snapshot = self.capture_snapshot();
        let frame = snapshot.frame;
        let mut captures = lock(&self.captures, "script captures lock");
        let id = captures.last_key_value().map_or(1, |(id, _)| id + 1);
        captures.insert(id, snapshot);
        serde_json::json!({ "frame": frame, "__capture_id": id })
    }

    /// Compare the snapshot of a capture of this evaluation with the current
    /// registry.
    pub(in super::super) fn capture_diff(
        &self,
        pos: ScriptPosition,
        capture: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let id = capture
            .get("__capture_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| self.type_error(pos, "Capture:diff expected a capture table"))?;
        let options = self.diff_options(pos, options)?;
        let after = self.capture_snapshot();
        let captures = lock(&self.captures, "script captures lock");
        let before = captures.get(&id).ok_or_else(|| {
            self.type_error(pos, "Capture:diff expected a capture of this evaluation")
        })?;
        self.to_json(pos, diff_captures(before, &after, &options))
    }

    /// Drop the snapshots of the evaluation.
    pub(in super::super) fn drop_captures(&self) {
        lock(&self.captures, "script captures lock").clear();
    }

    /// Read the optional screenshot encoding options.
    ///
    /// An absent table keeps the established JPEG encoding and the 1,600 pixel
    /// long edge, so every current script is unaffected.
    pub(super) fn screenshot_options(
        &self,
        pos: ScriptPosition,
        options: Option<&Value>,
    ) -> ScriptResult<ScreenshotOptions> {
        let mut resolved = ScreenshotOptions::default();
        let Some(options) = options else {
            return Ok(resolved);
        };
        if options.is_null() {
            return Ok(resolved);
        }
        let map = options
            .as_object()
            .ok_or_else(|| self.type_error(pos, "screenshot options must be a table"))?;
        if let Some(format) = map_value(Some(map), "format") {
            let word = format
                .as_str()
                .ok_or_else(|| self.type_error(pos, "screenshot format must be a string"))?;
            resolved.format = ScreenshotFormat::parse(word).ok_or_else(|| {
                self.type_error(pos, format!("unknown screenshot format `{word}`"))
            })?;
        }
        if let Some(max_dimension) = map_value(Some(map), "max_dimension") {
            let value = max_dimension
                .as_f64()
                .ok_or_else(|| self.type_error(pos, "max_dimension must be a number"))?;
            if !value.is_finite()
                || value < 0.0
                || value > f64::from(u32::MAX)
                || value.fract() != 0.0
            {
                return Err(self.type_error(pos, "max_dimension must be a non-negative integer"));
            }
            resolved.max_dimension = value as u32;
        }
        Ok(resolved)
    }

    pub(in super::super) async fn screenshot(
        &self,
        pos: ScriptPosition,
        target: Option<&Value>,
        options: Option<&Value>,
    ) -> ScriptResult<Value> {
        let options = self.screenshot_options(pos, options)?;
        let mut viewport_id = None;
        let mut widget_target: Option<WidgetRef> = None;
        if let Some(target) = target {
            if let Some(map) = target.as_object() {
                let has_target = map.contains_key("id");
                if has_target {
                    widget_target = Some(
                        parse_widget_ref(target)
                            .map_err(|error| self.type_error(pos, error.message))?,
                    );
                    viewport_id = parse_optional_string(Some(map), "viewport_id")
                        .map_err(|error| self.type_error(pos, error.message))?;
                } else if map.contains_key("viewport_id") {
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
            let encoded = self
                .await_tool(pos, async {
                    capture_screenshot(
                        &self.server.inner,
                        &self.server.runtime,
                        viewport_id_resolved,
                        ScreenshotKind::Widget {
                            rect: widget.interact_rect,
                            pixels_per_point,
                        },
                        options,
                    )
                    .await
                    .map_err(tmcp::ToolError::from)
                })
                .await?;
            self.store_image(ImageCapture {
                id: id.clone(),
                data: encoded.data,
                media_type: encoded.media_type,
                kind: ScriptImageKind::Widget,
                viewport_id: viewport_id_to_string(viewport_id_resolved),
                target: Some(target),
                rect: Some(widget.interact_rect),
            });
            return Ok(image_ref_json(id));
        }

        let viewport_id_resolved = resolve_screenshot_viewport(&self.server.inner, viewport_id)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        let encoded = self
            .await_tool(pos, async {
                capture_screenshot(
                    &self.server.inner,
                    &self.server.runtime,
                    viewport_id_resolved,
                    ScreenshotKind::Viewport,
                    options,
                )
                .await
                .map_err(tmcp::ToolError::from)
            })
            .await?;
        self.store_image(ImageCapture {
            id: id.clone(),
            data: encoded.data,
            media_type: encoded.media_type,
            kind: ScriptImageKind::Viewport,
            viewport_id: viewport_id_to_string(viewport_id_resolved),
            target: None,
            rect: None,
        });
        Ok(image_ref_json(id))
    }

    pub(in super::super) async fn native_screenshot(
        &self,
        pos: ScriptPosition,
        viewport_id: String,
        options: Option<&Value>,
    ) -> ScriptResult<Value> {
        let options = self.screenshot_options(pos, options)?;
        let viewport_id_resolved =
            resolve_screenshot_viewport(&self.server.inner, Some(viewport_id))
                .map_err(|error| self.tool_error(pos, error.into()))?;
        let id = self.next_image_id();
        let encoded = self
            .await_tool(pos, async {
                capture_native_screenshot(&self.server.inner, viewport_id_resolved, options)
                    .map_err(tmcp::ToolError::from)
            })
            .await?;
        self.store_image(ImageCapture {
            id: id.clone(),
            data: encoded.data,
            media_type: encoded.media_type,
            kind: ScriptImageKind::NativeViewport,
            viewport_id: viewport_id_to_string(viewport_id_resolved),
            target: None,
            rect: None,
        });
        Ok(image_ref_json(id))
    }

    /// Parse the array of positions of a pixel sample.
    fn sample_positions(&self, pos: ScriptPosition, positions: &Value) -> ScriptResult<Vec<Pos2>> {
        positions
            .as_array()
            .ok_or_else(|| self.type_error(pos, "sample_pixels expects an array of positions"))?
            .iter()
            .map(|value| parse_pos2(value).map_err(|error| self.type_error(pos, error.message)))
            .collect()
    }

    pub(in super::super) async fn sample_pixels(
        &self,
        pos: ScriptPosition,
        positions: &Value,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let positions = self.sample_positions(pos, positions)?;
        let samples = self
            .await_tool(
                pos,
                self.server.viewport_sample_pixels(viewport_id, positions),
            )
            .await?;
        self.to_json(pos, samples)
    }

    pub(in super::super) async fn widget_sample_pixels(
        &self,
        pos: ScriptPosition,
        target: &Value,
        viewport_id: Option<String>,
        positions: &Value,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let positions = self.sample_positions(pos, positions)?;
        let samples = self
            .await_tool(
                pos,
                self.server
                    .widget_sample_pixels(viewport_id, &target, positions),
            )
            .await?;
        self.to_json(pos, samples)
    }

    pub(in super::super) async fn widget_sample_grid(
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

    pub(in super::super) async fn check_layout(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        let result = self
            .await_tool(pos, self.server.check_layout(viewport_id, None))
            .await?;
        self.to_json(pos, result)
    }

    pub(in super::super) async fn check_layout_widget(
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

    /// Show a highlight on a widget (by target) or a rect with a mandatory
    /// color.
    pub(in super::super) async fn show_highlight_widget(
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
    pub(in super::super) async fn show_highlight_rect(
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
    pub(in super::super) async fn clear_widget_highlight(
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

    /// Clear all highlights in the selected viewport.
    pub(in super::super) async fn clear_highlights(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.clear_highlights(viewport_id, None))
            .await?;
        Ok(Value::Null)
    }

    pub(in super::super) async fn show_debug_overlay(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
        options: Option<&Map<String, Value>>,
        scope: Option<WidgetRef>,
    ) -> ScriptResult<Value> {
        let mode = match map_value(options, "mode") {
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

    pub(in super::super) async fn clear_debug_overlay(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
    ) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.clear_debug_overlay(viewport_id))
            .await?;
        self.to_json(pos, ())
    }

    pub(in super::super) async fn clear_widget_debug_overlay(
        &self,
        pos: ScriptPosition,
        target: WidgetRef,
    ) -> ScriptResult<Value> {
        let viewport_id = match target.viewport_id.as_ref() {
            Some(viewport_id) => viewport_id.clone(),
            None => {
                resolve_widget(&self.server.inner, None, &target)
                    .map_err(|error| self.tool_error(pos, error.into()))?
                    .viewport_id
            }
        };
        self.clear_debug_overlay(pos, Some(viewport_id)).await
    }
}

/// Compare two optional widget values, tolerating an int/float round trip.
pub(super) fn optional_widget_values_match(
    before: Option<&WidgetValue>,
    after: Option<&WidgetValue>,
) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => widget_values_match(before, after),
        _ => false,
    }
}

pub(super) fn parse_sample_grid_count(value: &Value, name: &'static str) -> Result<usize, String> {
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

pub(super) fn diff_captures(
    before: &ScriptCapture,
    after: &ScriptCapture,
    options: &DiffOptions,
) -> TreeDiff {
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

pub(super) fn capture_widget_map(
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

pub(super) fn changed_widget_fields(
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

pub(super) fn rect_changed(before: &Rect, after: &Rect, epsilon: f32) -> bool {
    (before.min.x - after.min.x).abs() > epsilon
        || (before.min.y - after.min.y).abs() > epsilon
        || (before.max.x - after.max.x).abs() > epsilon
        || (before.max.y - after.max.y).abs() > epsilon
}

pub(super) fn image_ref_json(id: String) -> Value {
    serde_json::json!({
        "type": "image_ref",
        "id": id,
    })
}
