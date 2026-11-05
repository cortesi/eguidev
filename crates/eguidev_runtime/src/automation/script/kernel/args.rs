//! Argument extractors of the host functions.

use super::*;

pub(super) struct JsonArg(pub(super) Value);

impl<'s> FromLua<'s> for JsonArg {
    fn from_lua(value: ScopedValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        scoped_value_to_json(scope, value).map(Self)
    }
}

pub(super) struct FrameCountOptionsArgs {
    pub(super) count: Option<f64>,
    pub(super) options: Option<Value>,
}

impl<'s> FromLuaMulti<'s> for FrameCountOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() > 2 {
            return Err(RuntimeError::runtime(format!(
                "wait_for_frames expected optional count and optional options, got {} arguments",
                values.len()
            )));
        }
        let options = if values.len() == 2 {
            optional_json_value(scope, values.pop())?
        } else {
            None
        };
        let count = match values.pop() {
            Some(value) => Option::<f64>::from_lua(value, scope)?,
            None => None,
        };
        Ok(Self { count, options })
    }
}

pub(super) struct OptionalJsonArg(pub(super) Option<Value>);

impl<'s> FromLuaMulti<'s> for OptionalJsonArg {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        match values.len() {
            0 => Ok(Self(None)),
            1 => scoped_value_to_json(scope, values.remove(0))
                .map(Some)
                .map(Self),
            got => Err(RuntimeError::runtime(format!(
                "expected at most one argument, got {got}"
            ))),
        }
    }
}

pub(super) struct FixtureArgs {
    pub(super) name: String,
    pub(super) params: Option<Value>,
}

impl<'s> FromLuaMulti<'s> for FixtureArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(1..=2).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "fixture expected name and optional params, got {} arguments",
                values.len()
            )));
        }
        let name = String::from_lua(values.remove(0), scope)?;
        let params = optional_json_value(scope, values.pop())?;
        Ok(Self { name, params })
    }
}

pub(super) struct ViewportReceiver {
    pub(super) id: String,
}

impl ViewportReceiver {
    pub(super) fn options_with_viewport(&self, mut options: Option<Value>) -> Option<Value> {
        inject_viewport_id(self.id.clone(), &mut options);
        options
    }
}

impl<'s> FromLua<'s> for ViewportReceiver {
    fn from_lua(value: ScopedValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let value = JsonArg::from_lua(value, scope)?.0;
        let Some(id) = value
            .as_object()
            .and_then(|object| object.get("id"))
            .and_then(Value::as_str)
        else {
            return Err(RuntimeError::runtime("method expected viewport self table"));
        };
        Ok(Self { id: id.to_string() })
    }
}

impl<'s> FromLuaMulti<'s> for ViewportReceiver {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 1 {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self, got {} arguments",
                values.len()
            )));
        }
        Self::from_lua(values.remove(0), scope)
    }
}

pub(super) struct ViewportOptionsArgs {
    pub(super) receiver: ViewportReceiver,
    pub(super) options: Option<Value>,
}

impl ViewportOptionsArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(1..=2).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self { receiver, options })
    }
}

pub(super) struct ViewportStringOptionsArgs {
    pub(super) receiver: ViewportReceiver,
    pub(super) value: String,
    pub(super) options: Option<Value>,
}

impl ViewportStringOptionsArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportStringOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self, string, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let value = String::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            value,
            options,
        })
    }
}

pub(super) struct ViewportValueArgs {
    pub(super) receiver: ViewportReceiver,
    pub(super) value: Value,
}

impl<'s> FromLuaMulti<'s> for ViewportValueArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 2 {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self and value argument, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let value = JsonArg::from_lua(values.remove(0), scope)?.0;
        Ok(Self { receiver, value })
    }
}

pub(super) struct ViewportValueStringArgs {
    pub(super) receiver: ViewportReceiver,
    pub(super) value: Value,
    pub(super) text: String,
}

impl<'s> FromLuaMulti<'s> for ViewportValueStringArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 3 {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self, value, and string argument, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let value = JsonArg::from_lua(values.remove(0), scope)?.0;
        let text = String::from_lua(values.remove(0), scope)?;
        Ok(Self {
            receiver,
            value,
            text,
        })
    }
}

pub(super) struct ViewportPredicateArgs {
    pub(super) receiver: ViewportReceiver,
    pub(super) predicate: StashedClosure,
    pub(super) options: Option<Value>,
}

impl ViewportPredicateArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportPredicateArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "wait_for expected self, predicate, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let predicate = stashed_function_arg(scope, "wait_for", values.remove(0))?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            predicate,
            options,
        })
    }
}

pub(super) struct WidgetReceiver {
    pub(super) value: Value,
    pub(super) id: String,
    pub(super) viewport_id: Option<String>,
}

impl WidgetReceiver {
    pub(super) fn options_with_viewport(&self, mut options: Option<Value>) -> Option<Value> {
        if let Some(viewport_id) = &self.viewport_id {
            inject_viewport_id(viewport_id.clone(), &mut options);
        }
        options
    }

    pub(super) fn widget_ref(&self) -> WidgetRef {
        WidgetRef {
            id: self.id.clone(),
            viewport_id: self.viewport_id.clone(),
        }
    }
}

impl<'s> FromLua<'s> for WidgetReceiver {
    fn from_lua(value: ScopedValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let value = JsonArg::from_lua(value, scope)?.0;
        let Some(object) = value.as_object() else {
            return Err(RuntimeError::runtime("method expected widget self table"));
        };
        let Some(id) = object.get("id").and_then(Value::as_str) else {
            return Err(RuntimeError::runtime("method expected widget self table"));
        };
        let viewport_id = object
            .get("__viewport_id")
            .or_else(|| object.get("viewport_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let id = id.to_string();
        Ok(Self {
            value,
            id,
            viewport_id,
        })
    }
}

impl<'s> FromLuaMulti<'s> for WidgetReceiver {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 1 {
            return Err(RuntimeError::runtime(format!(
                "method expected widget self, got {} arguments",
                values.len()
            )));
        }
        Self::from_lua(values.remove(0), scope)
    }
}

pub(super) struct WidgetStringArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) value: String,
}

impl<'s> FromLuaMulti<'s> for WidgetStringArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 2 {
            return Err(RuntimeError::runtime(format!(
                "method expected widget self and string argument, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let value = String::from_lua(values.remove(0), scope)?;
        Ok(Self { receiver, value })
    }
}

pub(super) struct WidgetValueArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) value: Value,
}

impl<'s> FromLuaMulti<'s> for WidgetValueArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 2 {
            return Err(RuntimeError::runtime(format!(
                "method expected widget self and value argument, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let value = JsonArg::from_lua(values.remove(0), scope)?.0;
        Ok(Self { receiver, value })
    }
}

pub(super) struct WidgetGridArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) nx: Value,
    pub(super) ny: Value,
}

impl<'s> FromLuaMulti<'s> for WidgetGridArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 3 {
            return Err(RuntimeError::runtime(format!(
                "method expected widget self, nx, and ny arguments, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let nx = JsonArg::from_lua(values.remove(0), scope)?.0;
        let ny = JsonArg::from_lua(values.remove(0), scope)?.0;
        Ok(Self { receiver, nx, ny })
    }
}

pub(super) struct WidgetPredicateArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) predicate: StashedClosure,
    pub(super) options: Option<Value>,
    pub(super) condition: String,
}

impl WidgetPredicateArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetPredicateArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 4 {
            return Err(RuntimeError::runtime(format!(
                "widget_wait expected self, predicate, options, and condition, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let predicate = stashed_function_arg(scope, "widget_wait", values.remove(0))?;
        let options = optional_json_value(scope, Some(values.remove(0)))?;
        let condition = String::from_lua(values.remove(0), scope)?;
        Ok(Self {
            receiver,
            predicate,
            options,
            condition,
        })
    }
}

pub(super) struct WidgetOptionsArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) options: Option<Value>,
}

impl WidgetOptionsArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(1..=2).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "method expected self and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self { receiver, options })
    }
}

pub(super) struct WidgetTextOptionsArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) text: String,
    pub(super) options: Option<Value>,
}

impl WidgetTextOptionsArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetTextOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "method expected self, text, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let text = String::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            text,
            options,
        })
    }
}

pub(super) struct WidgetValueOptionsArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) value: Value,
    pub(super) options: Option<Value>,
}

impl WidgetValueOptionsArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetValueOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "method expected self, value, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let value = JsonArg::from_lua(values.remove(0), scope)?.0;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            value,
            options,
        })
    }
}

pub(super) struct WidgetDragRelativeArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) relative: Value,
    pub(super) options: Option<Value>,
}

impl WidgetDragRelativeArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetDragRelativeArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=4).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "drag_relative expected self, relative, optional from, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let relative = JsonArg::from_lua(values.remove(0), scope)?.0;
        let third = if values.is_empty() {
            None
        } else {
            Some(values.remove(0))
        };
        let has_fourth = !values.is_empty();
        let mut options = optional_json_value(scope, values.pop())?;
        if let Some(third) = third {
            let value = JsonArg::from_lua(third, scope)?.0;
            // An explicit `nil` in the `from` position reads the same as
            // omitting it, so `drag_relative(delta, nil, options)` works.
            if value.is_null() {
                // Nothing to record.
            } else if has_fourth || is_vec2_value(&value) {
                insert_option(&mut options, "from", value)?;
            } else {
                options = Some(value);
            }
        }
        Ok(Self {
            receiver,
            relative,
            options,
        })
    }
}

pub(super) struct WidgetDragToArgs {
    pub(super) receiver: WidgetReceiver,
    pub(super) target: WidgetReceiver,
    pub(super) options: Option<Value>,
}

impl WidgetDragToArgs {
    pub(super) fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetDragToArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "drag_to expected self, target widget, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let target = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            target,
            options,
        })
    }
}

pub(super) fn optional_json_value<'s>(
    scope: &Scope<'s>,
    value: Option<ScopedValue<'s>>,
) -> Result<Option<Value>, RuntimeError> {
    value
        .map(|value| JsonArg::from_lua(value, scope).map(|value| value.0))
        .transpose()
        .map(|value| value.filter(|value| !value.is_null()))
}

pub(super) fn stashed_function_arg<'s>(
    scope: &Scope<'s>,
    name: &str,
    value: ScopedValue<'s>,
) -> Result<StashedClosure, RuntimeError> {
    let ScopedValue::Function(function) = value else {
        return Err(RuntimeError::runtime(format!("{name} expected a function")));
    };
    scope.stash_function(function)
}

pub(super) struct PredicateJsonArg {
    pub(super) value: StashedValue,
}

impl<'s> IntoLuaMulti<'s> for PredicateJsonArg {
    fn into_lua_multi(self, scope: &Scope<'s>) -> Result<MultiValue<'s>, RuntimeError> {
        Ok(MultiValue::from_values(vec![
            scope.fetch_value(&self.value)?,
        ]))
    }
}

pub(super) async fn predicate_matches(
    ctx: &AsyncHostContext,
    predicate: &StashedClosure,
    value: Value,
) -> ScriptResult<bool> {
    let value = stash_predicate_value(ctx, value)
        .await
        .map_err(|error| ruau_runtime_error_info(&error))?;
    let result = ctx
        .call_protected(predicate, PredicateJsonArg { value })
        .await
        .map_err(|error| ruau_runtime_error_info(&error))?;
    let result = result.map_err(|error| {
        ruau_host_script_error_info(error.kind(), error.value(), error.traceback())
    })?;
    predicate_bool_result(&result.values)
}

pub(super) async fn stash_predicate_value(
    ctx: &AsyncHostContext,
    value: Value,
) -> Result<StashedValue, RuntimeError> {
    ctx.scope(move |scope| {
        let value = json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?;
        scope.stash_value(value)
    })
    .await
}

pub(super) fn predicate_bool_result(values: &[OwnedValue]) -> ScriptResult<bool> {
    match values {
        [OwnedValue::Boolean(value)] => Ok(*value),
        values => Err(type_error(format!(
            "wait predicate must return one boolean, got {}",
            owned_values_shape(values)
        ))),
    }
}

pub(super) fn insert_option(
    options: &mut Option<Value>,
    key: &str,
    value: Value,
) -> Result<(), RuntimeError> {
    let Some(map) = options
        .get_or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
    else {
        return Err(RuntimeError::runtime(
            "options must be a table when adding script binding options",
        ));
    };
    map.insert(key.to_string(), value);
    Ok(())
}

pub(super) fn is_vec2_value(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.len() == 2
        && map.get("x").and_then(Value::as_f64).is_some()
        && map.get("y").and_then(Value::as_f64).is_some()
}

pub(super) fn no_args(name: &str, args: &MultiValue<'_>) -> Result<(), RuntimeError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError::runtime(format!(
            "{name} expected no arguments, got {}",
            args.len()
        )))
    }
}

pub(super) fn one_arg<'s>(
    name: &str,
    args: MultiValue<'s>,
) -> Result<ScopedValue<'s>, RuntimeError> {
    let mut values = args.into_vec();
    match values.len() {
        1 => Ok(values.remove(0)),
        got => Err(RuntimeError::runtime(format!(
            "{name} expected one argument, got {got}"
        ))),
    }
}

pub(super) fn optional_json_arg<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<Option<Value>, RuntimeError> {
    let mut values = args.into_vec();
    match values.len() {
        0 => Ok(None),
        1 => scoped_value_to_json(scope, values.remove(0)).map(Some),
        got => Err(RuntimeError::runtime(format!(
            "{name} expected at most one argument, got {got}"
        ))),
    }
}

pub(super) fn capture_self_and_options<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<(Value, Option<Value>), RuntimeError> {
    let mut values = args.into_vec();
    if !(1..=2).contains(&values.len()) {
        return Err(RuntimeError::runtime(format!(
            "{name} expected self and optional options, got {} arguments",
            values.len()
        )));
    }
    let capture = scoped_value_to_json(scope, values.remove(0))?;
    let options = values
        .pop()
        .map(|value| scoped_value_to_json(scope, value))
        .transpose()?
        .filter(|value| !value.is_null());
    Ok((capture, options))
}

pub(super) fn viewport_self_and_options<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<(String, Option<Value>), RuntimeError> {
    let mut values = args.into_vec();
    let self_value = match values.len() {
        1 | 2 => values.remove(0),
        got => {
            return Err(RuntimeError::runtime(format!(
                "{name} expected self and optional options, got {got} arguments"
            )));
        }
    };
    let viewport_id = viewport_id_from_self(scope, name, self_value)?;
    let options = values
        .pop()
        .map(|value| scoped_value_to_json(scope, value))
        .transpose()?
        .filter(|value| !value.is_null());
    Ok((viewport_id, options))
}

pub(super) fn viewport_self<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<String, RuntimeError> {
    let mut values = args.into_vec();
    match values.len() {
        1 => viewport_id_from_self(scope, name, values.remove(0)),
        got => Err(RuntimeError::runtime(format!(
            "{name} expected self, got {got} arguments"
        ))),
    }
}

pub(super) fn viewport_self_point_and_options<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<(String, Value, Option<Value>), RuntimeError> {
    let mut values = args.into_vec();
    if !(2..=3).contains(&values.len()) {
        return Err(RuntimeError::runtime(format!(
            "{name} expected self, point, and optional options, got {} arguments",
            values.len()
        )));
    }
    let viewport_id = viewport_id_from_self(scope, name, values.remove(0))?;
    let point = scoped_value_to_json(scope, values.remove(0))?;
    let options = widget_at_point_options(scope, values.pop())?;
    Ok((viewport_id, point, options))
}

pub(super) fn viewport_id_from_self<'s>(
    scope: &Scope<'s>,
    name: &str,
    self_value: ScopedValue<'s>,
) -> Result<String, RuntimeError> {
    let ScopedValue::Table(table) = self_value else {
        return Err(RuntimeError::runtime(format!(
            "{name} expected viewport self table"
        )));
    };
    table.get::<_, String>(scope, "id")
}

pub(super) fn widget_self<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<Value, RuntimeError> {
    Ok(widget_receiver(scope, name, args)?.value)
}

pub(super) fn widget_receiver<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<WidgetReceiver, RuntimeError> {
    let mut values = args.into_vec();
    if values.len() != 1 {
        return Err(RuntimeError::runtime(format!(
            "{name} expected widget self, got {} arguments",
            values.len()
        )));
    }
    WidgetReceiver::from_lua(values.remove(0), scope)
}

pub(super) fn widget_at_point_options<'s>(
    scope: &Scope<'s>,
    value: Option<ScopedValue<'s>>,
) -> Result<Option<Value>, RuntimeError> {
    match value {
        None => Ok(None),
        Some(ScopedValue::Boolean(all_layers)) => {
            let mut map = serde_json::Map::new();
            map.insert("all_layers".to_string(), Value::Bool(all_layers));
            Ok(Some(Value::Object(map)))
        }
        Some(value) => scoped_value_to_json(scope, value).map(Some),
    }
}

pub(super) fn inject_viewport_id(viewport_id: String, options: &mut Option<Value>) {
    let map = options
        .get_or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut();
    if let Some(map) = map {
        map.entry("viewport_id")
            .or_insert(Value::String(viewport_id));
    }
}

pub(super) fn optional_luau_number_to_json(value: Option<f64>) -> Result<Value, RuntimeError> {
    value.map_or(Ok(Value::Null), |value| {
        if value.fract() == 0.0 && value >= 0.0 && value <= u64::MAX as f64 {
            return Ok(Value::Number((value as u64).into()));
        }
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| RuntimeError::runtime("number argument must be finite"))
    })
}
