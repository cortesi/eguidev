//! Borrowed host functions and the script positions of their callers.

use super::*;

pub(super) fn configure_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let options = optional_json_arg(scope, "configure", args)?;
    runtime
        .configure(pos, options.as_ref().and_then(Value::as_object))
        .map_err(|info| scoped_host_error(scope, info))?;
    Ok(MultiValue::new())
}

pub(super) fn fixtures_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: &MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    no_args("fixtures", args)?;
    let pos = script_position_from_caller(scope);
    let value = runtime
        .fixtures(pos)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn dump_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let options = optional_json_arg(scope, "dump", args)?;
    let options = match options.as_ref() {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map),
        Some(_) => return Err(RuntimeError::runtime("dump expected an options table")),
    };
    let value = runtime
        .dump(pos, options)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn dump_text_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let options = optional_json_arg(scope, "dump_text", args)?;
    let options = match options.as_ref() {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map),
        Some(_) => return Err(RuntimeError::runtime("dump_text expected an options table")),
    };
    let value = runtime
        .dump_text(pos, options)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn viewport_widget_list_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let (viewport_id, mut options) = viewport_self_and_options(scope, "widget_list", args)?;
    inject_viewport_id(viewport_id, &mut options);
    let value = runtime
        .widget_list(pos, options.as_ref().and_then(Value::as_object))
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn viewport_state_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let viewport_id = viewport_self(scope, "state", args)?;
    let value = runtime
        .viewport_state(pos, viewport_id)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn viewport_widget_at_point_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let (viewport_id, point, mut options) =
        viewport_self_point_and_options(scope, "widget_at_point", args)?;
    inject_viewport_id(viewport_id, &mut options);
    let value = runtime
        .widget_at_point(pos, &point, options.as_ref().and_then(Value::as_object))
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn widget_viewport_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let receiver = widget_receiver(scope, "viewport", args)?;
    let viewport_id = receiver.viewport_id.as_deref().unwrap_or("root");
    let value = runtime
        .viewport_handle_json(pos, viewport_id)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn widget_state_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let target = widget_self(scope, "state", args)?;
    let value = runtime
        .widget_state(pos, &target)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn widget_parent_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let target = widget_self(scope, "parent", args)?;
    let value = runtime
        .widget_parent(pos, &target)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn widget_children_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let target = widget_self(scope, "children", args)?;
    let value = runtime
        .widget_children(pos, &target)
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn assert_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let values = args.into_vec();
    let pos = script_position_from_caller(scope);
    let Some(condition) = values.first().copied() else {
        return Err(scoped_host_error(
            scope,
            runtime.type_error(pos, "assert expected a boolean condition"),
        ));
    };
    let condition = from_scoped_value::<bool>(scope, condition).map_err(|error| {
        scoped_host_error(
            scope,
            runtime.type_error(pos, format!("assert condition must be boolean: {error}")),
        )
    })?;
    let message = values
        .get(1)
        .copied()
        .map(|message| from_scoped_value::<String>(scope, message))
        .transpose()
        .map_err(|error| {
            scoped_host_error(
                scope,
                runtime.type_error(pos, format!("assert message must be string: {error}")),
            )
        })?;
    let message = message.unwrap_or_else(|| {
        if condition {
            "assertion passed".to_string()
        } else {
            "assertion failed".to_string()
        }
    });
    runtime.record_assertion(pos, condition, message);
    Ok(MultiValue::new())
}

pub(super) fn capture_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: &MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    no_args("capture", args)?;
    let value = runtime.capture();
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn capture_diff_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let (capture, options) = capture_self_and_options(scope, "diff", args)?;
    let value = runtime
        .capture_diff(pos, &capture, options.as_ref().and_then(Value::as_object))
        .map_err(|info| scoped_host_error(scope, info))?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

pub(super) fn log_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let value = one_arg("log", args)?;
    let rendered = match scoped_value_to_json(scope, value) {
        Ok(Value::String(value)) => value,
        Ok(value) if !value.is_null() => value.to_string(),
        Ok(_) => "null".to_string(),
        Err(error) => return Err(error),
    };
    runtime.log(rendered);
    Ok(MultiValue::new())
}

pub(super) fn frozen_array_host<'s>(
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let value = one_arg("array", args)?;
    let ScopedValue::Table(source) = value else {
        return Err(RuntimeError::runtime("array expected a table"));
    };
    let ScopedValue::Table(result) = json_to_scoped_value(scope, &Value::Array(Vec::new()))? else {
        return Err(RuntimeError::runtime("failed to create array"));
    };
    for index in 1..=source.len(scope)? {
        let value = source.get::<_, ScopedValue<'_>>(scope, index as f64)?;
        result.set(scope, index as f64, value)?;
    }
    result.freeze(scope)?;
    Ok(MultiValue::from_values(vec![ScopedValue::Table(result)]))
}

pub(super) fn args_host<'s>(
    script_args: &Value,
    scope: &Scope<'s>,
    args: &MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    no_args("__eguidev_args", args)?;
    let value = json_to_scoped_value(scope, script_args)?;
    let ScopedValue::Table(table) = value else {
        return Err(RuntimeError::runtime(
            "script args did not convert to a table",
        ));
    };
    table.freeze_deep(scope)?;
    Ok(MultiValue::from_values(vec![ScopedValue::Table(table)]))
}

pub(super) fn script_position_from_caller(scope: &Scope<'_>) -> ScriptPosition {
    script_position_from_location(scope.caller_location(0))
}

pub(super) async fn script_position_from_context(
    ctx: &AsyncHostContext,
) -> Result<ScriptPosition, RuntimeError> {
    let location = ctx.scope(|scope| Ok(scope.caller_location(0))).await?;
    Ok(script_position_from_location(location))
}

pub(super) fn script_position_from_location(location: Option<SourceLocation>) -> ScriptPosition {
    location
        .map(|location| ScriptPosition {
            line: Some(location.line as usize),
            column: None,
        })
        .unwrap_or_default()
}
