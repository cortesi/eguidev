//! Queries of the script runtime: widgets, viewports, text, and tree dumps.

use super::*;

impl ScriptRuntime {
    pub(in super::super) fn widget_list(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let include_invisible = parse_optional_bool(options, "include_invisible")
            .map_err(|error| self.type_error(pos, error.message))?;
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
        let covered = parse_optional_bool(options, "covered")
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
            covered,
        )
        .map_err(|error| self.tool_error(pos, error))?;
        self.widget_handle_list_json(pos, &widgets)
    }

    pub(in super::super) fn widget_at_point(
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

    pub(in super::super) async fn text_measure(
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

    pub(in super::super) async fn viewports_list(
        &self,
        pos: ScriptPosition,
    ) -> ScriptResult<Value> {
        let result = self.await_tool(pos, self.server.viewports_list()).await?;
        self.viewport_handle_list_json(pos, &result)
    }

    pub(in super::super) fn viewport_state(
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

    pub(in super::super) fn widget_state(
        &self,
        pos: ScriptPosition,
        target: &Value,
    ) -> ScriptResult<Value> {
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

    pub(in super::super) fn widget_parent(
        &self,
        pos: ScriptPosition,
        target: &Value,
    ) -> ScriptResult<Value> {
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
        let candidates = inner.widgets.widget_list(viewport_id);
        let parent = candidates
            .iter()
            .find(|candidate| {
                candidate.id == parent_id
                    && candidate.rect.min.x <= widget.rect.min.x
                    && candidate.rect.min.y <= widget.rect.min.y
                    && candidate.rect.max.x >= widget.rect.max.x
                    && candidate.rect.max.y >= widget.rect.max.y
            })
            .or_else(|| {
                candidates
                    .iter()
                    .rev()
                    .find(|candidate| candidate.id == parent_id)
            });
        match parent {
            Some(parent) => self.widget_handle_json(pos, parent),
            None => Ok(Value::Null),
        }
    }

    pub(in super::super) fn widget_children(
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

    pub(in super::super) fn dump(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let options = dump_options(options).map_err(|message| self.type_error(pos, message))?;
        let dump = build_tree_dump(&self.server.inner, &options)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        self.to_json(pos, dump)
    }

    pub(in super::super) fn dump_text(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let options = dump_options(options).map_err(|message| self.type_error(pos, message))?;
        let dump = build_tree_dump(&self.server.inner, &options)
            .map_err(|error| self.tool_error(pos, error.into()))?;
        self.to_json(pos, dump_text(&dump))
    }
}

pub(super) fn dump_options(options: Option<&Map<String, Value>>) -> Result<DumpOptions, String> {
    let Some(options) = options else {
        return Ok(DumpOptions::default());
    };
    let parsed = serde_json::from_value(Value::Object(options.clone()))
        .map_err(|error| format!("invalid dump options: {error}"))?;
    // `DumpOptions` refuses an unknown key, so the parse read each key.
    #[cfg(test)]
    super::super::parse::record_table_reads(options);
    Ok(parsed)
}
