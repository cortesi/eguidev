//! Marshaling of script values and conversion of errors to script error
//! records.

use super::*;

pub(super) fn values_to_script_value(
    runtime: &ScriptRuntime,
    values: &[ValueSnapshot],
) -> Result<super::super::types::ScriptValue, ScriptErrorInfo> {
    let json_values = values
        .iter()
        .map(marshaled_script_value_to_json)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| type_error(format!("failed to convert Ruau result to JSON: {error}")))?;
    let Some(value) = script_return_value_from_json_values(json_values) else {
        return Ok(super::super::types::ScriptValue::default());
    };
    Ok(script_value_from_json(runtime, value))
}

pub(super) fn marshaled_script_value_to_json(value: &ValueSnapshot) -> Result<Value, RuntimeError> {
    match marshaled_to_json(value) {
        Ok(value) => Ok(value),
        Err(error) => marshaled_sparse_array_to_json(value).unwrap_or(Err(error)),
    }
}

pub(super) fn marshaled_sparse_array_to_json(
    value: &ValueSnapshot,
) -> Option<Result<Value, RuntimeError>> {
    let ValueSnapshot::Table(pairs) = value else {
        return None;
    };
    let max_index = pairs
        .iter()
        .map(|pair| marshaled_positive_array_index(&pair.key))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    if max_index == 0 || max_index == pairs.len() {
        return None;
    }
    let mut slots = vec![None; max_index];
    for pair in pairs {
        let index = marshaled_positive_array_index(&pair.key)?;
        let slot = &mut slots[index - 1];
        if slot.replace(&pair.value).is_some() {
            return Some(Err(RuntimeError::runtime(
                "sparse array contains duplicate integer keys",
            )));
        }
    }
    Some(
        slots
            .into_iter()
            .map(|value| {
                value.map_or(Ok(Value::Null), |value| {
                    marshaled_script_value_to_json(value)
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    )
}

pub(super) fn marshaled_positive_array_index(value: &ValueSnapshot) -> Option<usize> {
    match value {
        ValueSnapshot::Integer(value) => usize::try_from(*value).ok().filter(|value| *value > 0),
        ValueSnapshot::Number(value) => {
            if value.fract() == 0.0 && *value >= 1.0 && *value <= usize::MAX as f64 {
                Some(*value as usize)
            } else {
                None
            }
        }
        ValueSnapshot::Nil
        | ValueSnapshot::Boolean(_)
        | ValueSnapshot::Vector(_)
        | ValueSnapshot::String(_)
        | ValueSnapshot::Buffer(_)
        | ValueSnapshot::Table(_)
        | ValueSnapshot::LightUserdata { .. }
        | ValueSnapshot::Opaque(_) => None,
    }
}

pub(super) fn compile_error_info(error: &CompileError, source_name: &str) -> ScriptErrorInfo {
    let error_type = match error.kind() {
        CompileErrorKind::Parse => "parse",
        CompileErrorKind::Internal => "runtime",
        _ => "runtime",
    };
    ScriptErrorInfo {
        error_type: error_type.to_string(),
        message: error.message().to_string(),
        location: error.location().map(|location| ScriptLocation {
            line: location.begin.line as usize + 1,
            column: Some(location.begin.column as usize + 1),
        }),
        backtrace: error
            .location()
            .is_some()
            .then(|| vec![source_name.to_string()]),
        code: None,
        details: None,
    }
}

pub(super) fn ruau_script_error_info(error: &MarshaledScriptError) -> ScriptErrorInfo {
    if let Some(mut info) = error.payload_ref::<ScriptErrorInfo>().cloned() {
        if info.backtrace.is_none() {
            info.backtrace = backtrace_lines(error);
        }
        return info;
    }
    if let Ok(value) = marshaled_to_json(error.value())
        && let Some(info) = public_error_info(
            &value,
            error.frames().iter().find_map(frame_location),
            backtrace_lines(error),
        )
    {
        return info;
    }
    let error_type = match error.kind() {
        RuntimeErrorKind::Deadline | RuntimeErrorKind::Cancelled => "timeout",
        _ => "runtime",
    };
    ScriptErrorInfo {
        error_type: error_type.to_string(),
        message: marshaled_error_text(error.value()),
        location: error.frames().iter().find_map(frame_location),
        backtrace: backtrace_lines(error),
        code: None,
        details: None,
    }
}

pub(super) fn public_error_info(
    value: &Value,
    location: Option<ScriptLocation>,
    backtrace: Option<Vec<String>>,
) -> Option<ScriptErrorInfo> {
    let error = value.as_object()?;
    let code = error.get("code")?.as_str()?.to_string();
    let message = error.get("message")?.as_str()?.to_string();
    let mut details = error.get("details").cloned();
    if error.get("operation").is_some_and(|value| !value.is_null())
        || error.get("target").is_some_and(|value| !value.is_null())
    {
        let mut context = serde_json::Map::new();
        if let Some(operation) = error.get("operation").filter(|value| !value.is_null()) {
            context.insert("operation".to_string(), operation.clone());
        }
        if let Some(target) = error.get("target").filter(|value| !value.is_null()) {
            context.insert("target".to_string(), target.clone());
        }
        if let Some(value) = details.take() {
            context.insert("details".to_string(), value);
        }
        details = Some(Value::Object(context));
    }
    Some(ScriptErrorInfo {
        error_type: if code == "timeout" {
            "timeout".to_string()
        } else {
            "eguidev".to_string()
        },
        message,
        location,
        backtrace,
        code: Some(code),
        details,
    })
}

pub(super) fn fatal_error_info(
    kind: RuntimeErrorKind,
    rendered_error: &str,
    timeout_ms: u64,
) -> ScriptErrorInfo {
    let error_type = match kind {
        RuntimeErrorKind::Deadline | RuntimeErrorKind::Cancelled => "timeout",
        _ => "runtime",
    };
    let message = if error_type == "timeout" && timeout_ms > 0 {
        format!("Script timed out after {timeout_ms}ms")
    } else {
        format!("Ruau VM failed with {kind:?}: {rendered_error}")
    };
    ScriptErrorInfo {
        error_type: error_type.to_string(),
        message,
        location: None,
        backtrace: None,
        code: None,
        details: None,
    }
}

pub(super) fn runtime_error(message: String) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: "runtime".to_string(),
        message,
        location: None,
        backtrace: None,
        code: None,
        details: None,
    }
}

pub(super) fn check_error_info(error: CheckFailure) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: error.error_type.to_string(),
        message: error.message,
        location: error.line.map(|line| ScriptLocation {
            line,
            column: error.column,
        }),
        backtrace: None,
        code: Some(format!("{}_failed", error.error_type)),
        details: Some(serde_json::json!({ "diagnostics": error.diagnostics })),
    }
}

pub(super) fn type_error(message: String) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: "type_error".to_string(),
        message,
        location: None,
        backtrace: None,
        code: None,
        details: None,
    }
}

pub(super) fn script_eval_task_error(error: &JoinError) -> ScriptEvalOutcome {
    ScriptEvalOutcome::error_only(runtime_error(format!("script task failed: {error}")))
}

pub(super) fn ruau_runtime_error_info(error: &RuntimeError) -> ScriptErrorInfo {
    if let Some(info) = error.payload_ref::<ScriptErrorInfo>().cloned() {
        return info;
    }
    ScriptErrorInfo {
        error_type: error_type_for_kind(error.kind()).to_string(),
        message: error.message().to_string(),
        location: None,
        backtrace: None,
        code: None,
        details: None,
    }
}

pub(super) fn ruau_host_script_error_info(
    kind: RuntimeErrorKind,
    value: &OwnedValue,
    traceback: Option<&str>,
) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: error_type_for_kind(kind).to_string(),
        message: value.display_lua(),
        location: None,
        backtrace: traceback_lines_from_text(traceback),
        code: None,
        details: None,
    }
}

pub(super) fn error_type_for_kind(kind: RuntimeErrorKind) -> &'static str {
    match kind {
        RuntimeErrorKind::Deadline | RuntimeErrorKind::Cancelled => "timeout",
        _ => "runtime",
    }
}

pub(super) fn marshaled_error_text(value: &ValueSnapshot) -> String {
    match marshaled_to_json(value) {
        Ok(Value::String(message)) => message,
        Ok(value) if !value.is_null() => value.to_string(),
        Ok(_) => "null".to_string(),
        Err(error) => error.to_string(),
    }
}

pub(super) fn owned_values_shape(values: &[OwnedValue]) -> String {
    if values.is_empty() {
        return "no values".to_string();
    }
    values
        .iter()
        .map(OwnedValue::type_name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn frame_location(frame: &TracebackFrame) -> Option<ScriptLocation> {
    frame.line.map(|line| ScriptLocation {
        line: line as usize,
        column: None,
    })
}

pub(super) fn backtrace_lines(error: &MarshaledScriptError) -> Option<Vec<String>> {
    let mut lines = error
        .frames()
        .iter()
        .map(|frame| {
            let mut rendered = frame.chunk_name.clone();
            if let Some(line) = frame.line {
                rendered.push(':');
                rendered.push_str(&line.to_string());
            }
            if let Some(function_name) = &frame.function_name {
                rendered.push_str(" function ");
                rendered.push_str(function_name);
            }
            rendered
        })
        .collect::<Vec<_>>();
    if error.frames_truncated() {
        lines.push("... traceback truncated".to_string());
    }
    if lines.is_empty() { None } else { Some(lines) }
}

pub(super) fn traceback_lines_from_text(traceback: Option<&str>) -> Option<Vec<String>> {
    let lines = traceback?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if lines.is_empty() { None } else { Some(lines) }
}

/// Raise the failure of a runtime call as the script-visible table of the
/// declared `Error` type. `details` and its `target` are Luau tables when the
/// VM can build them, and the details fall back to JSON text otherwise. The
/// whole error rides along as the payload, for the outcome.
pub(super) fn host_error_table(
    info: ScriptErrorInfo,
    details: Option<OwnedValue>,
    target: Option<OwnedValue>,
) -> RuntimeError {
    let mut fields = vec![ScriptErrorField::new("error_type", info.error_type.clone())];
    if let Some(code) = info.code.clone() {
        fields.push(ScriptErrorField::new("code", code));
    }
    match (details, info.details.as_ref()) {
        (Some(details), _) => fields.push(ScriptErrorField::new("details", details)),
        (None, Some(details)) => fields.push(ScriptErrorField::new("details", details.to_string())),
        (None, None) => {}
    }
    if let Some(target) = target {
        fields.push(ScriptErrorField::new("target", target));
    }
    RuntimeError::structured(info.message.clone(), fields).with_payload(info)
}

/// Return the error target that the details of an error name.
pub(super) fn error_target(info: &ScriptErrorInfo) -> Option<&Value> {
    info.details.as_ref()?.get("target")
}

/// Build one field value of an error table in a VM scope.
pub(super) fn scoped_error_value(
    scope: &Scope<'_>,
    value: &Value,
) -> Result<OwnedValue, RuntimeError> {
    let value = json_to_scoped_value_with_options(scope, value, JsonDecodeOptions::typed())?;
    scope.owned_value(value)
}

/// Raise the failure of a borrowed host call, which holds its VM scope.
pub(super) fn scoped_host_error(scope: &Scope<'_>, info: ScriptErrorInfo) -> RuntimeError {
    let details = info
        .details
        .as_ref()
        .and_then(|details| scoped_error_value(scope, details).ok());
    let target = error_target(&info).and_then(|target| scoped_error_value(scope, target).ok());
    host_error_table(info, details, target)
}

/// Build one field value of an error table through the VM scope of an async
/// host call.
pub(super) async fn async_error_value(
    ctx: &AsyncHostContext,
    value: Option<Value>,
) -> Option<OwnedValue> {
    let value = value?;
    ctx.scope(move |scope| scoped_error_value(scope, &value))
        .await
        .ok()
}

/// Raise the failure of a runtime call through the VM scope of its async
/// host call.
pub(super) trait RaiseInScope<T> {
    /// Return the value, or raise the error as a script-visible table.
    fn or_raise(
        self,
        ctx: &AsyncHostContext,
    ) -> impl Future<Output = Result<T, RuntimeError>> + Send;
}

impl<T: Send> RaiseInScope<T> for ScriptResult<T> {
    fn or_raise(
        self,
        ctx: &AsyncHostContext,
    ) -> impl Future<Output = Result<T, RuntimeError>> + Send {
        let ctx = ctx.clone();
        async move {
            match self {
                Ok(value) => Ok(value),
                Err(info) => {
                    let details = async_error_value(&ctx, info.details.clone()).await;
                    let target = async_error_value(&ctx, error_target(&info).cloned()).await;
                    Err(host_error_table(info, details, target))
                }
            }
        }
    }
}
