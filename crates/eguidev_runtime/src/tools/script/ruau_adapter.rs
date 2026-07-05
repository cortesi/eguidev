#![allow(clippy::missing_docs_in_private_items, clippy::result_large_err)]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(test)]
use ruau::ast::parse::{Options, SyntaxFlags, parse_file_with};
use ruau::{
    bytecode::{CompileError, CompileErrorKind, CompileOptions},
    decl::DeclSource,
    vm::{
        Ambient, AsyncHostContext, CallOptions, Deadline, FromLua, FromLuaMulti, IntoLuaMulti,
        Limits, LoadedModule, MarshaledScriptError, MarshaledValue, ModuleBuilderExt, MultiValue,
        RuntimeCapabilities, RuntimeError, Scope, ScopedHostFunction, ScopedValue, SourceLocation,
        StashedClosure, StashedValue, Table, TracebackFrame, Vm, async_host_fn,
        serde::{from_scoped_value, json_to_scoped_value, marshaled_to_json, scoped_value_to_json},
    },
    vm_api::{
        HostReturn, ModuleBinding, ModuleBuilder, NativeModule, OwnedValue, RuntimeErrorKind,
    },
};
use serde_json::Value;
use tokio::{
    runtime::Builder as TokioRuntimeBuilder,
    task::{JoinError, LocalSet, spawn_blocking},
};

use super::{
    outcome::{build_error_outcome, build_success_outcome},
    runtime::ScriptRuntime,
    types::{
        ScriptArgs, ScriptErrorInfo, ScriptEvalOutcome, ScriptLocation, ScriptPosition,
        ScriptResult, ScriptTiming,
    },
    value::{script_args_to_json, script_return_value_from_json_values, script_value_from_json},
};
use crate::{registry::Inner, runtime::Runtime, types::WidgetRef};

const EGUIDEV_SEED: u64 = 0x00e9_d1de;
const BUILTIN_PRELUDE_SOURCE: &str = include_str!("../../../luau/prelude.luau");

const SETUP_SOURCE: &[u8] = br#"
--!nonstrict
args = __eguidev_args()
__eguidev_args = nil

local __eguidev_configure = configure
local __eguidev_wait_options = {
    timeout_ms = 5000,
    poll_interval_ms = 16,
}

local function __eguidev_copy_wait_options(options)
    local copy = {}
    if options ~= nil then
        for key, value in pairs(options) do
            copy[key] = value
        end
    end
    return copy
end

function configure(options)
    __eguidev_configure(options)
    if options ~= nil then
        if options.timeout_ms ~= nil then
            __eguidev_wait_options.timeout_ms = options.timeout_ms
        end
        if options.poll_interval_ms ~= nil then
            __eguidev_wait_options.poll_interval_ms = options.poll_interval_ms
        end
    end
end

function wait_until(predicate, options)
    if type(predicate) ~= "function" then
        error("wait_until expected a predicate function", 2)
    end
    local wait_options = __eguidev_copy_wait_options(__eguidev_wait_options)
    if options ~= nil then
        for key, value in pairs(options) do
            wait_options[key] = value
        end
    end
    local timeout_ms = wait_options.timeout_ms
    local deadline_ms = os.clock() * 1000 + timeout_ms
    while not predicate() do
        wait_options.timeout_ms = math.max(0, math.ceil(deadline_ms - os.clock() * 1000))
        wait_for_capture(wait_options)
    end
end
"#;

const CORE_DECLARATION: &str = include_str!("../../../luau/eguidev.d.luau");

const VIEWPORT_METHODS: &[u8] = b"viewport_methods";
const WIDGET_METHODS: &[u8] = b"widget_methods";
const CAPTURE_METHODS: &[u8] = b"capture_methods";

#[cfg(test)]
const SUPPORTED_GLOBALS: &[&str] = &[
    "_G",
    "args",
    "assert",
    "assert_widget_exists",
    "bit32",
    "buffer",
    "capture",
    "configure",
    "coroutine",
    "debug",
    "diagnostic",
    "diagnostics",
    "dump",
    "dump_text",
    "expect",
    "expect_above",
    "expect_absent",
    "expect_left_of",
    "expect_no_overlap",
    "expect_painted",
    "expect_text_fits",
    "expect_tree",
    "expect_within",
    "fixture",
    "fixture_raw",
    "fixtures",
    "ipairs",
    "log",
    "math",
    "next",
    "os",
    "pairs",
    "root",
    "select",
    "string",
    "table",
    "tonumber",
    "tostring",
    "try_widget",
    "type",
    "utf8",
    "viewport",
    "viewports",
    "wait_for_capture",
    "wait_for_frames",
    "wait_until",
    "widget",
];

#[cfg(test)]
const SUPPORTED_METHODS: &[&str] = &[
    "check_layout",
    "children",
    "click",
    "diff",
    "dismiss_popups",
    "drag",
    "drag_relative",
    "drag_to",
    "focus",
    "hide_debug_overlay",
    "hide_highlight",
    "hover",
    "key",
    "parent",
    "paste",
    "raw_key",
    "raw_pointer_button",
    "raw_pointer_move",
    "raw_scroll",
    "raw_text",
    "sample_grid",
    "sample_pixels",
    "screenshot",
    "scroll",
    "scroll_into_view",
    "scroll_to",
    "set_inner_size",
    "set_resize_options",
    "set_value",
    "show_debug_overlay",
    "show_highlight",
    "state",
    "text_measure",
    "type_text",
    "viewport",
    "wait_for",
    "wait_for_absent",
    "wait_for_capture",
    "wait_for_scroll_ready",
    "wait_for_settle",
    "wait_for_visible",
    "wait_for_widget",
    "wait_for_widget_absent",
    "wait_for_widget_visible",
    "widget_at_point",
    "widget_get",
    "widget_list",
];

pub(super) async fn run_script_eval(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    source_name: String,
    args: ScriptArgs,
) -> ScriptEvalOutcome {
    let _guard = super::SCRIPT_EVAL_LOCK.lock().await;
    match spawn_blocking(move || {
        run_script_eval_blocking(inner, runtime, script, timeout_ms, source_name, args)
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => script_eval_task_error(&error),
    }
}

fn run_script_eval_blocking(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    source_name: String,
    args: ScriptArgs,
) -> ScriptEvalOutcome {
    let local_runtime = match TokioRuntimeBuilder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return ScriptEvalOutcome::error_only(runtime_error(format!(
                "failed to build Ruau local runtime: {error}"
            )));
        }
    };
    LocalSet::new().block_on(
        &local_runtime,
        run_script_eval_local(inner, runtime, script, timeout_ms, source_name, args),
    )
}

async fn run_script_eval_local(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    source_name: String,
    args: ScriptArgs,
) -> ScriptEvalOutcome {
    let app_preludes = inner.script_preludes.preludes();
    let script_runtime = Arc::new(ScriptRuntime::new(
        inner,
        runtime,
        source_name.clone(),
        timeout_ms,
    ));
    let start = Instant::now();
    let compile_start = Instant::now();
    let runtime_capabilities = RuntimeCapabilities::default();

    let module = Arc::new(EguidevModule {
        args: script_args_to_luau_json(&args),
        runtime: Arc::clone(&script_runtime),
        declaration: script_declarations(&app_preludes),
    });
    let mut vm = match Vm::builder()
        .ambient(Ambient::production(EGUIDEV_SEED))
        .limits(base_limits())
        .runtime_capabilities(runtime_capabilities.clone())
        .module(module)
        .build()
    {
        Ok(vm) => vm,
        Err(error) => {
            let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
            return build_error_outcome(
                &script_runtime,
                runtime_error(format!("failed to build Ruau VM: {error}")),
                timing,
            );
        }
    };

    let setup = match load(
        &mut vm,
        &runtime_capabilities,
        b"@eguidev_setup.luau",
        SETUP_SOURCE,
        &source_name,
    ) {
        Ok(setup) => setup,
        Err(error) => {
            let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
            return build_error_outcome(&script_runtime, error, timing);
        }
    };
    if let Err(error) = run_setup(&mut vm, &setup).await {
        let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return build_error_outcome(&script_runtime, error, timing);
    }
    if let Err(error) = run_builtin_prelude(&mut vm, &runtime_capabilities).await {
        let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return build_error_outcome(&script_runtime, error, timing);
    }
    if let Err(error) = run_app_preludes(&mut vm, &runtime_capabilities, &app_preludes).await {
        let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return build_error_outcome(&script_runtime, error, timing);
    }
    if let Err(error) = vm.sandbox_for_untrusted() {
        let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return build_error_outcome(
            &script_runtime,
            runtime_error(format!("failed to install Ruau sandbox: {error}")),
            timing,
        );
    }

    let source_chunk_name = format!("@{source_name}");
    let module = match load(
        &mut vm,
        &runtime_capabilities,
        source_chunk_name.as_bytes(),
        script.as_bytes(),
        &source_name,
    ) {
        Ok(module) => module,
        Err(error) => {
            let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
            return build_error_outcome(&script_runtime, error, timing);
        }
    };

    let compile_elapsed = compile_start.elapsed();
    let exec_start = Instant::now();
    let outcome = vm
        .exec_async(
            &module,
            CallOptions::new().limits(invocation_limits(timeout_ms)),
        )
        .await;
    let timing = timing(start, compile_elapsed, exec_start.elapsed());

    match outcome {
        Ok(values) => match values_to_script_value(&script_runtime, &values) {
            Ok(script_value) => build_success_outcome(&script_runtime, script_value, timing),
            Err(error) => build_error_outcome(&script_runtime, error, timing),
        },
        Err(error) => {
            if let Some(error) = error.script_error() {
                return build_error_outcome(&script_runtime, ruau_script_error_info(error), timing);
            }
            let rendered_error = error.to_string();
            build_error_outcome(
                &script_runtime,
                fatal_error_info(error.kind(), &rendered_error, timeout_ms),
                timing,
            )
        }
    }
}

#[cfg(test)]
fn is_supported_by_initial_ruau_slice(script: &str) -> bool {
    let result = parse_file_with(script, Options::default(), SyntaxFlags::all_luau());
    if !result.is_ok() {
        return false;
    }
    let Some(document) = result.into_json_document() else {
        return false;
    };
    let Ok(value) = serde_json::to_value(document) else {
        return false;
    };
    globals_in_ast(&value).all(is_supported_global)
        && methods_in_ast(&value).all(is_supported_method)
}

fn script_args_to_luau_json(args: &ScriptArgs) -> Value {
    let mut value = script_args_to_json(args);
    promote_integer_numbers_to_luau_numbers(&mut value);
    value
}

fn typed_json_to_luau_scoped_value<'s>(
    scope: &Scope<'s>,
    value: &Value,
) -> Result<ScopedValue<'s>, RuntimeError> {
    let mut value = value.clone();
    strip_object_null_fields(&mut value);
    promote_integer_numbers_to_luau_numbers(&mut value);
    json_to_scoped_value(scope, &value)
}

// Typed host returns strip optional null object fields before lossless JSON
// conversion, preserving array identity without exposing JSON null sentinels
// for optional record fields. Script args skip the stripping step so explicit
// nulls remain distinguishable from missing fields.
fn typed_json_array_to_luau_scoped_value<'s>(
    scope: &Scope<'s>,
    value: &Value,
) -> Result<ScopedValue<'s>, RuntimeError> {
    typed_json_to_luau_scoped_value(scope, value)
}

fn lossless_json_to_luau_scoped_value<'s>(
    scope: &Scope<'s>,
    value: &Value,
) -> Result<ScopedValue<'s>, RuntimeError> {
    let mut value = value.clone();
    promote_integer_numbers_to_luau_numbers(&mut value);
    json_to_scoped_value(scope, &value)
}

fn promote_integer_numbers_to_luau_numbers(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for value in map.values_mut() {
                promote_integer_numbers_to_luau_numbers(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                promote_integer_numbers_to_luau_numbers(value);
            }
        }
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                *number = serde_json::Number::from_f64(value as f64)
                    .expect("typed JSON integers convert to finite Luau numbers");
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

fn strip_object_null_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, value| !value.is_null());
            for value in map.values_mut() {
                strip_object_null_fields(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_object_null_fields(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
fn globals_in_ast(value: &Value) -> impl Iterator<Item = &str> {
    let mut globals = Vec::new();
    collect_globals(value, &mut globals);
    globals.into_iter()
}

#[cfg(test)]
fn methods_in_ast(value: &Value) -> impl Iterator<Item = &str> {
    let mut methods = Vec::new();
    collect_methods(value, &mut methods);
    methods.into_iter()
}

#[cfg(test)]
fn collect_globals<'a>(value: &'a Value, globals: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            let is_global = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "AstExprGlobal");
            if is_global && let Some(name) = map.get("global").and_then(Value::as_str) {
                globals.push(name);
            }
            for value in map.values() {
                collect_globals(value, globals);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_globals(value, globals);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
fn collect_methods<'a>(value: &'a Value, methods: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            let is_method_index = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "AstExprIndexName")
                && map.get("op").and_then(Value::as_str) == Some(":");
            if is_method_index && let Some(name) = map.get("index").and_then(Value::as_str) {
                methods.push(name);
            }
            for value in map.values() {
                collect_methods(value, methods);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_methods(value, methods);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
fn is_supported_global(name: &str) -> bool {
    SUPPORTED_GLOBALS.binary_search(&name).is_ok()
}

#[cfg(test)]
fn is_supported_method(name: &str) -> bool {
    SUPPORTED_METHODS.binary_search(&name).is_ok()
}

fn script_declarations(app_preludes: &[eguidev::ScriptPrelude]) -> String {
    let mut declaration = CORE_DECLARATION.trim_end().to_string();
    declaration.push_str("\ndeclare function __eguidev_args(): any\n");
    for prelude in app_preludes {
        if prelude.declarations.trim().is_empty() {
            continue;
        }
        declaration.push('\n');
        declaration.push_str(prelude.declarations.trim());
    }
    declaration.push('\n');
    declaration
}

async fn run_setup(vm: &mut Vm, setup: &LoadedModule) -> Result<(), ScriptErrorInfo> {
    match vm.exec_async(setup, CallOptions::new()).await {
        Ok(values) if values.is_empty() => Ok(()),
        Ok(values) => Err(runtime_error(format!(
            "Ruau setup returned unexpected values: {values:?}"
        ))),
        Err(error) => {
            if let Some(error) = error.script_error() {
                return Err(ruau_script_error_info(error));
            }
            let rendered_error = error.to_string();
            Err(fatal_error_info(error.kind(), &rendered_error, 0))
        }
    }
}

async fn run_builtin_prelude(
    vm: &mut Vm,
    runtime_capabilities: &RuntimeCapabilities,
) -> Result<(), ScriptErrorInfo> {
    let label = "built-in prelude";
    let prelude = load_prelude(
        vm,
        runtime_capabilities,
        b"@eguidev_prelude.luau",
        BUILTIN_PRELUDE_SOURCE.as_bytes(),
        label,
    )?;
    run_prelude(vm, &prelude, label).await
}

async fn run_app_preludes(
    vm: &mut Vm,
    runtime_capabilities: &RuntimeCapabilities,
    app_preludes: &[eguidev::ScriptPrelude],
) -> Result<(), ScriptErrorInfo> {
    for prelude in app_preludes {
        let namespace_source = format!("--!nonstrict\n{} = {{}}\n", prelude.namespace);
        let namespace_label = format!("app prelude namespace {}", prelude.namespace);
        let namespace_setup = load_prelude(
            vm,
            runtime_capabilities,
            format!("@{namespace_label}.luau").as_bytes(),
            namespace_source.as_bytes(),
            &namespace_label,
        )?;
        run_prelude(vm, &namespace_setup, &namespace_label).await?;

        let source_label = format!("app prelude {}", prelude.namespace);
        let app_prelude = load_prelude(
            vm,
            runtime_capabilities,
            format!("@{}.prelude.luau", prelude.namespace).as_bytes(),
            prelude.source.as_bytes(),
            &source_label,
        )?;
        run_prelude(vm, &app_prelude, &source_label).await?;
    }
    Ok(())
}

async fn run_prelude(
    vm: &mut Vm,
    prelude: &LoadedModule,
    label: &str,
) -> Result<(), ScriptErrorInfo> {
    match vm.exec_async(prelude, CallOptions::new()).await {
        Ok(values) if values.is_empty() => Ok(()),
        Ok(values) => Err(prelude_error_info(
            label,
            runtime_error(format!("prelude returned unexpected values: {values:?}")),
        )),
        Err(error) => {
            let info = if let Some(error) = error.script_error() {
                ruau_script_error_info(error)
            } else {
                let rendered_error = error.to_string();
                fatal_error_info(error.kind(), &rendered_error, 0)
            };
            Err(prelude_error_info(label, info))
        }
    }
}

fn load(
    vm: &mut Vm,
    runtime_capabilities: &RuntimeCapabilities,
    chunk_name: &[u8],
    source: &[u8],
    source_name: &str,
) -> Result<LoadedModule, ScriptErrorInfo> {
    let chunk = runtime_capabilities
        .compile_source(source, &CompileOptions::new())
        .map_err(|error| compile_error_info(&error, source_name))?;
    vm.load_named(&chunk, chunk_name)
        .map_err(|error| runtime_error(format!("failed to load Ruau chunk: {error}")))
}

fn load_prelude(
    vm: &mut Vm,
    runtime_capabilities: &RuntimeCapabilities,
    chunk_name: &[u8],
    source: &[u8],
    label: &str,
) -> Result<LoadedModule, ScriptErrorInfo> {
    let chunk = runtime_capabilities
        .compile_source(source, &CompileOptions::new())
        .map_err(|error| prelude_error_info(label, compile_error_info(&error, label)))?;
    vm.load_named(&chunk, chunk_name).map_err(|error| {
        prelude_error_info(
            label,
            runtime_error(format!("failed to load Ruau chunk: {error}")),
        )
    })
}

fn prelude_error_info(label: &str, mut info: ScriptErrorInfo) -> ScriptErrorInfo {
    info.error_type = "prelude".to_string();
    if !info.message.starts_with(label) {
        info.message = format!("{label}: {}", info.message);
    }
    if info.backtrace.is_none() {
        info.backtrace = Some(vec![label.to_string()]);
    }
    info
}

fn values_to_script_value(
    runtime: &ScriptRuntime,
    values: &[MarshaledValue],
) -> Result<super::types::ScriptValue, ScriptErrorInfo> {
    let json_values = values
        .iter()
        .map(marshaled_script_value_to_json)
        .map(|value| value.map(normalize_integral_numbers))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| type_error(format!("failed to convert Ruau result to JSON: {error}")))?;
    let Some(value) = script_return_value_from_json_values(json_values) else {
        return Ok(super::types::ScriptValue::default());
    };
    Ok(script_value_from_json(runtime, value))
}

fn marshaled_script_value_to_json(value: &MarshaledValue) -> Result<Value, RuntimeError> {
    match marshaled_to_json(value) {
        Ok(value) => Ok(value),
        Err(error) => marshaled_sparse_array_to_json(value).unwrap_or(Err(error)),
    }
}

fn marshaled_sparse_array_to_json(value: &MarshaledValue) -> Option<Result<Value, RuntimeError>> {
    let MarshaledValue::Table(pairs) = value else {
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
                    marshaled_script_value_to_json(value).map(normalize_integral_numbers)
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    )
}

fn marshaled_positive_array_index(value: &MarshaledValue) -> Option<usize> {
    match value {
        MarshaledValue::Integer(value) => usize::try_from(*value).ok().filter(|value| *value > 0),
        MarshaledValue::Number(value) => {
            if value.fract() == 0.0 && *value >= 1.0 && *value <= usize::MAX as f64 {
                Some(*value as usize)
            } else {
                None
            }
        }
        MarshaledValue::Nil
        | MarshaledValue::Boolean(_)
        | MarshaledValue::Vector(_)
        | MarshaledValue::String(_)
        | MarshaledValue::Buffer(_)
        | MarshaledValue::Table(_)
        | MarshaledValue::LightUserdata { .. }
        | MarshaledValue::Opaque(_) => None,
    }
}

fn normalize_integral_numbers(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.into_iter().map(normalize_integral_numbers).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, normalize_integral_numbers(value)))
                .collect(),
        ),
        Value::Number(number) => number
            .as_f64()
            .filter(|value| {
                number.as_i64().is_none()
                    && value.fract() == 0.0
                    && *value >= i64::MIN as f64
                    && *value <= i64::MAX as f64
            })
            .map_or_else(
                || Value::Number(number.clone()),
                |value| Value::Number((value as i64).into()),
            ),
        Value::Null | Value::Bool(_) | Value::String(_) => value,
    }
}

fn compile_error_info(error: &CompileError, source_name: &str) -> ScriptErrorInfo {
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

fn ruau_script_error_info(error: &MarshaledScriptError) -> ScriptErrorInfo {
    if let Some(mut info) = error.payload_ref::<ScriptErrorInfo>().cloned() {
        if info.backtrace.is_none() {
            info.backtrace = backtrace_lines(error);
        }
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

fn fatal_error_info(
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

fn runtime_error(message: String) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: "runtime".to_string(),
        message,
        location: None,
        backtrace: None,
        code: None,
        details: None,
    }
}

fn type_error(message: String) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: "type_error".to_string(),
        message,
        location: None,
        backtrace: None,
        code: None,
        details: None,
    }
}

fn script_eval_task_error(error: &JoinError) -> ScriptEvalOutcome {
    ScriptEvalOutcome::error_only(runtime_error(format!("script task failed: {error}")))
}

fn ruau_runtime_error_info(error: &RuntimeError) -> ScriptErrorInfo {
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

fn ruau_host_script_error_info(
    kind: RuntimeErrorKind,
    value: &OwnedValue,
    traceback: Option<&str>,
) -> ScriptErrorInfo {
    ScriptErrorInfo {
        error_type: error_type_for_kind(kind).to_string(),
        message: owned_value_text(value),
        location: None,
        backtrace: traceback_lines_from_text(traceback),
        code: None,
        details: None,
    }
}

fn error_type_for_kind(kind: RuntimeErrorKind) -> &'static str {
    match kind {
        RuntimeErrorKind::Deadline | RuntimeErrorKind::Cancelled => "timeout",
        _ => "runtime",
    }
}

fn marshaled_error_text(value: &MarshaledValue) -> String {
    match marshaled_to_json(value) {
        Ok(Value::String(message)) => message,
        Ok(value) if !value.is_null() => value.to_string(),
        Ok(_) => "null".to_string(),
        Err(error) => error.to_string(),
    }
}

fn owned_value_text(value: &OwnedValue) -> String {
    match value {
        OwnedValue::Nil => "nil".to_string(),
        OwnedValue::Boolean(value) => value.to_string(),
        OwnedValue::Number(value) => value.to_string(),
        OwnedValue::Integer(value) => value.to_string(),
        OwnedValue::Vector(value) => format!("{value:?}"),
        OwnedValue::LightUserdata { .. } => "lightuserdata".to_string(),
        OwnedValue::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        OwnedValue::Pinned(_) => "pinned value".to_string(),
    }
}

fn owned_values_shape(values: &[OwnedValue]) -> String {
    if values.is_empty() {
        return "no values".to_string();
    }
    values
        .iter()
        .map(owned_value_type_name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn owned_value_type_name(value: &OwnedValue) -> &'static str {
    match value {
        OwnedValue::Nil => "nil",
        OwnedValue::Boolean(_) => "boolean",
        OwnedValue::Number(_) | OwnedValue::Integer(_) => "number",
        OwnedValue::Vector(_) => "vector",
        OwnedValue::LightUserdata { .. } => "lightuserdata",
        OwnedValue::Bytes(_) => "string",
        OwnedValue::Pinned(_) => "pinned",
    }
}

fn frame_location(frame: &TracebackFrame) -> Option<ScriptLocation> {
    frame.line.map(|line| ScriptLocation {
        line: line as usize,
        column: None,
    })
}

fn backtrace_lines(error: &MarshaledScriptError) -> Option<Vec<String>> {
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

fn traceback_lines_from_text(traceback: Option<&str>) -> Option<Vec<String>> {
    let lines = traceback?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if lines.is_empty() { None } else { Some(lines) }
}

fn timing(start: Instant, compile_elapsed: Duration, exec_elapsed: Duration) -> ScriptTiming {
    ScriptTiming {
        compile_ms: compile_elapsed.as_millis() as u64,
        exec_ms: exec_elapsed.as_millis() as u64,
        total_ms: start.elapsed().as_millis() as u64,
    }
}

fn base_limits() -> Limits {
    Limits {
        gas: Some(10_000_000),
        max_memory_bytes: Some(16 * 1024 * 1024),
        max_native_depth: Some(16),
        quantum: Some(1_000),
        ..Limits::unlimited()
    }
}

fn invocation_limits(timeout_ms: u64) -> Limits {
    Limits {
        deadline: deadline_after(timeout_ms),
        ..base_limits()
    }
}

fn deadline_after(timeout_ms: u64) -> Option<Deadline> {
    Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .map(Deadline::Wall)
}

struct EguidevModule {
    args: Value,
    runtime: Arc<ScriptRuntime>,
    declaration: String,
}

impl NativeModule for EguidevModule {
    fn name(&self) -> &str {
        "eguidev_initial"
    }

    fn declaration(&self) -> DeclSource<'_> {
        DeclSource::Text(&self.declaration)
    }

    fn build(&self, builder: &mut dyn ModuleBuilder) {
        self.register_core_globals(builder);
        self.register_viewport_methods(builder);
        self.register_widget_methods(builder);
        self.register_capture_methods(builder);
        self.register_script_utility_globals(builder);
    }
}

impl EguidevModule {
    fn register_core_globals(&self, builder: &mut dyn ModuleBuilder) {
        builder.scoped_function(
            "assert",
            ModuleBinding::GlobalOverride,
            Box::new(AssertFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "configure",
            ModuleBinding::Global,
            Box::new(ConfigureFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "fixture",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, args: FixtureArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .fixture(pos, args.name, args.params)
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "fixture_raw",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, args: FixtureArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    runtime
                        .fixture_raw(pos, args.name, args.params)
                        .await
                        .map_err(host_script_error)?;
                    Ok(HostReturn::default())
                }
            }),
        );
        builder.scoped_function(
            "fixtures",
            ModuleBinding::Global,
            Box::new(FixturesFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "diagnostic",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, name: String| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .diagnostic(pos, name)
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "diagnostics",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, (): ()| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime.diagnostics(pos).await.map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        builder.scoped_function(
            "dump",
            ModuleBinding::Global,
            Box::new(DumpFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "dump_text",
            ModuleBinding::Global,
            Box::new(DumpTextFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "root",
            ModuleBinding::Global,
            Box::new(RootFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "viewport",
            ModuleBinding::Global,
            Box::new(ViewportFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, args: StringOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .widget_find(
                            pos,
                            args.value,
                            args.options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return_with_metatable(&ctx, value, WIDGET_METHODS).await
                }
            }),
        );
        builder.scoped_function(
            "try_widget",
            ModuleBinding::Global,
            Box::new(TryWidgetFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewports",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, (): ()| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewports_list(pos, None)
                        .await
                        .map_err(host_script_error)?;
                    typed_json_array_host_return_with_metatable(&ctx, value, VIEWPORT_METHODS).await
                }
            }),
        );
    }

    fn register_capture_methods(&self, builder: &mut dyn ModuleBuilder) {
        builder.scoped_function(
            "diff",
            ModuleBinding::hidden("capture_methods"),
            Box::new(CaptureDiffFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
    }

    fn register_viewport_methods(&self, builder: &mut dyn ModuleBuilder) {
        builder.scoped_function(
            "state",
            ModuleBinding::hidden("viewport_methods"),
            Box::new(ViewportStateFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "widget_list",
            ModuleBinding::hidden("viewport_methods"),
            Box::new(ViewportWidgetListFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "widget_get",
            ModuleBinding::hidden("viewport_methods"),
            Box::new(ViewportWidgetGetFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "widget_at_point",
            ModuleBinding::hidden("viewport_methods"),
            Box::new(ViewportWidgetAtPointFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_widget",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportWidgetPredicateArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let options = args.options_with_viewport();
                        let target = Value::String(args.widget_id);
                        let predicate = args.predicate.clone();
                        let predicate_ctx = ctx.clone();
                        let value = runtime
                            .wait_for_widget_predicate(
                                pos,
                                &target,
                                options.as_ref().and_then(Value::as_object),
                                move |widget| {
                                    let predicate = predicate.clone();
                                    let predicate_ctx = predicate_ctx.clone();
                                    async move {
                                        predicate_matches(&predicate_ctx, &predicate, widget).await
                                    }
                                },
                            )
                            .await
                            .map_err(host_script_error)?;
                        typed_json_host_return(&ctx, value).await
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_widget_visible",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportStringArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.receiver.options_with_viewport(None);
                    let target = Value::String(args.value);
                    let value = runtime
                        .wait_for_widget_visible(
                            pos,
                            &target,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_widget_absent",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportStringOptionsArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let options = args.options_with_viewport();
                        let target = Value::String(args.value);
                        let value = runtime
                            .wait_for_widget_absent(
                                pos,
                                &target,
                                options.as_ref().and_then(Value::as_object),
                            )
                            .await
                            .map_err(host_script_error)?;
                        typed_scalar_host_return(value)
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportPredicateArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let predicate = args.predicate.clone();
                    let predicate_ctx = ctx.clone();
                    let value = runtime
                        .wait_for_viewport_predicate(
                            pos,
                            options.as_ref().and_then(Value::as_object),
                            move |viewport| {
                                let predicate = predicate.clone();
                                let predicate_ctx = predicate_ctx.clone();
                                async move {
                                    predicate_matches(&predicate_ctx, &predicate, viewport).await
                                }
                            },
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "focus",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .focus_window(pos, viewport.id)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_settle",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = viewport.options_with_viewport(None);
                    let value = runtime
                        .wait_for_settle(pos, options.as_ref().and_then(Value::as_object))
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_capture",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = viewport.options_with_viewport(None);
                    let value = runtime
                        .wait_for_capture(pos, options.as_ref().and_then(Value::as_object))
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "dismiss_popups",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewport_dismiss_popups(pos, Some(viewport.id))
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "key",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportStringOptionsArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let options = args.options_with_viewport();
                        let value = runtime
                            .action_key(
                                pos,
                                args.value,
                                options.as_ref().and_then(Value::as_object),
                            )
                            .await
                            .map_err(host_script_error)?;
                        typed_scalar_host_return(value)
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "paste",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportStringOptionsArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let options = args.options_with_viewport();
                        let value = runtime
                            .action_paste(
                                pos,
                                args.value,
                                options.as_ref().and_then(Value::as_object),
                            )
                            .await
                            .map_err(host_script_error)?;
                        typed_scalar_host_return(value)
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "raw_pointer_move",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.receiver.options_with_viewport(None);
                    let value = runtime
                        .raw_pointer_move(
                            pos,
                            &args.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "raw_pointer_button",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportRawPointerButtonArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let pressed = parse_raw_action(&args.action)
                            .map_err(|message| host_script_error(type_error(message)))?;
                        let options = args.options_with_viewport();
                        let value = runtime
                            .raw_pointer_button(
                                pos,
                                &args.point,
                                &args.button,
                                pressed,
                                options.as_ref().and_then(Value::as_object),
                            )
                            .await
                            .map_err(host_script_error)?;
                        typed_scalar_host_return(value)
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "raw_key",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportRawKeyArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let pressed = parse_raw_action(&args.action)
                        .map_err(|message| host_script_error(type_error(message)))?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .raw_key(
                            pos,
                            args.key,
                            pressed,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "raw_text",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportStringArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.receiver.options_with_viewport(None);
                    let value = runtime
                        .raw_text(pos, args.value, options.as_ref().and_then(Value::as_object))
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "raw_scroll",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportValueOptionsArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let options = args.options_with_viewport();
                        let value = runtime
                            .raw_scroll(
                                pos,
                                &args.value,
                                options.as_ref().and_then(Value::as_object),
                            )
                            .await
                            .map_err(host_script_error)?;
                        typed_scalar_host_return(value)
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "set_inner_size",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewport_set_inner_size(pos, &args.value, Some(args.receiver.id))
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "set_resize_options",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewport_set_resize_options(
                            pos,
                            args.value.as_object(),
                            Some(args.receiver.id),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "screenshot",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let target = serde_json::json!({ "viewport_id": viewport.id });
                    let value = runtime
                        .screenshot(pos, Some(&target))
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "sample_pixels",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .sample_pixels(pos, &args.value, Some(args.receiver.id))
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "check_layout",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .check_layout(pos, Some(viewport.id))
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "show_highlight",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportValueStringArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let rect = super::parse::parse_rect(&args.value)
                            .map_err(|error| host_script_error(type_error(error.message)))?;
                        let value = runtime
                            .show_highlight_rect(pos, Some(args.receiver.id), rect, args.text)
                            .await
                            .map_err(host_script_error)?;
                        typed_json_host_return(&ctx, value).await
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "hide_highlight",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, _: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .hide_highlight_all(pos)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "show_debug_overlay",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportOverlayArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .show_debug_overlay(
                            pos,
                            Some(args.receiver.id),
                            args.mode.as_ref(),
                            args.options.as_ref().and_then(Value::as_object),
                            None,
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "hide_debug_overlay",
            ModuleBinding::hidden("viewport_methods"),
            async_host_fn(move |ctx: AsyncHostContext, _: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .hide_debug_overlay(pos)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
    }

    fn register_widget_methods(&self, builder: &mut dyn ModuleBuilder) {
        builder.scoped_function(
            "viewport",
            ModuleBinding::hidden("widget_methods"),
            Box::new(WidgetViewportFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "click",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_click(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "hover",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_hover(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "type_text",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetTextOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_type(
                            pos,
                            &args.receiver.value,
                            args.text,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "focus",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .action_focus(pos, &receiver.value)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "set_value",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetValueOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .widget_set_value(
                            pos,
                            &args.receiver.value,
                            &args.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "drag",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetValueOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_drag(
                            pos,
                            &args.receiver.value,
                            &args.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "drag_relative",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetDragRelativeArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_drag_relative(
                            pos,
                            &args.receiver.value,
                            &args.relative,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "drag_to",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetDragToArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_drag_to_widget(
                            pos,
                            &args.receiver.value,
                            &args.target.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "scroll",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetValueOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_scroll(
                            pos,
                            &args.receiver.value,
                            &args.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "scroll_to",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_scroll_to(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "scroll_into_view",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_scroll_into_view(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "text_measure",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .text_measure(pos, &receiver.value)
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "check_layout",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .check_layout_widget(pos, &receiver.value, receiver.viewport_id)
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "screenshot",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .screenshot(pos, Some(&receiver.value))
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "sample_pixels",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .widget_sample_pixels(
                            pos,
                            &args.receiver.value,
                            args.receiver.viewport_id,
                            &args.value,
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "sample_grid",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetGridArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .widget_sample_grid(
                            pos,
                            &args.receiver.value,
                            args.receiver.viewport_id,
                            &args.nx,
                            &args.ny,
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "show_highlight",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetStringArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .show_highlight_widget(
                            pos,
                            &args.receiver.value,
                            args.receiver.viewport_id,
                            args.value,
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "hide_highlight",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .hide_highlight_widget(pos, &receiver.value, receiver.viewport_id)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "show_debug_overlay",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOverlayArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .show_debug_overlay(
                            pos,
                            None,
                            args.mode.as_ref(),
                            args.options.as_ref().and_then(Value::as_object),
                            Some(args.receiver.widget_ref()),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "hide_debug_overlay",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, _: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .hide_debug_overlay(pos)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetPredicateArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let predicate = args.predicate.clone();
                    let predicate_ctx = ctx.clone();
                    let value = runtime
                        .wait_for_widget_predicate(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                            move |widget| {
                                let predicate = predicate.clone();
                                let predicate_ctx = predicate_ctx.clone();
                                async move {
                                    predicate_matches(&predicate_ctx, &predicate, widget).await
                                }
                            },
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_visible",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .wait_for_widget_visible(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_scroll_ready",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .wait_for_scroll_ready(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_json_host_return(&ctx, value).await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_absent",
            ModuleBinding::hidden("widget_methods"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .wait_for_widget_absent(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        builder.scoped_function(
            "state",
            ModuleBinding::hidden("widget_methods"),
            Box::new(WidgetStateFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "parent",
            ModuleBinding::hidden("widget_methods"),
            Box::new(WidgetParentFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "children",
            ModuleBinding::hidden("widget_methods"),
            Box::new(WidgetChildrenFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
    }

    fn register_script_utility_globals(&self, builder: &mut dyn ModuleBuilder) {
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_capture",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, options: OptionalJsonArg| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    runtime
                        .wait_for_capture(pos, options.0.as_ref().and_then(Value::as_object))
                        .await
                        .map_err(host_script_error)?;
                    Ok(HostReturn::default())
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "wait_for_frames",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, count: Option<f64>| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let count = optional_luau_number_to_json(count)?;
                    let value = runtime
                        .wait_for_frames(pos, &count)
                        .await
                        .map_err(host_script_error)?;
                    typed_scalar_host_return(value)
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "assert_widget_exists",
            ModuleBinding::Global,
            async_host_fn(move |ctx: AsyncHostContext, id: String| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let target = Value::String(id);
                    runtime
                        .assert_widget_exists(pos, &target, None)
                        .await
                        .map_err(host_script_error)?;
                    Ok(HostReturn::default())
                }
            }),
        );
        builder.scoped_function(
            "log",
            ModuleBinding::Global,
            Box::new(LogFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "capture",
            ModuleBinding::Global,
            Box::new(CaptureFn {
                runtime: Arc::clone(&self.runtime),
            }),
        );
        builder.scoped_function(
            "__eguidev_args",
            ModuleBinding::Global,
            Box::new(ArgsFn {
                args: self.args.clone(),
            }),
        );
    }
}

struct JsonArg(Value);

impl<'s> FromLua<'s> for JsonArg {
    fn from_lua(value: ScopedValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        scoped_value_to_json(scope, value)
            .map(normalize_integral_numbers)
            .map(Self)
    }
}

struct OptionalJsonArg(Option<Value>);

impl<'s> FromLuaMulti<'s> for OptionalJsonArg {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        match values.len() {
            0 => Ok(Self(None)),
            1 => scoped_value_to_json(scope, values.remove(0))
                .map(normalize_integral_numbers)
                .map(Some)
                .map(Self),
            got => Err(RuntimeError::runtime(format!(
                "expected at most one argument, got {got}"
            ))),
        }
    }
}

struct FixtureArgs {
    name: String,
    params: Option<Value>,
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

struct ViewportReceiver {
    id: String,
}

impl ViewportReceiver {
    fn options_with_viewport(&self, mut options: Option<Value>) -> Option<Value> {
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

struct ViewportStringArgs {
    receiver: ViewportReceiver,
    value: String,
}

impl<'s> FromLuaMulti<'s> for ViewportStringArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if values.len() != 2 {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self and string argument, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let value = String::from_lua(values.remove(0), scope)?;
        Ok(Self { receiver, value })
    }
}

struct ViewportStringOptionsArgs {
    receiver: ViewportReceiver,
    value: String,
    options: Option<Value>,
}

impl ViewportStringOptionsArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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

struct StringOptionsArgs {
    value: String,
    options: Option<Value>,
}

impl<'s> FromLuaMulti<'s> for StringOptionsArgs {
    fn from_lua_multi(args: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let (value, options) = string_and_options_arg(scope, "widget", args)?;
        Ok(Self { value, options })
    }
}

struct ViewportValueArgs {
    receiver: ViewportReceiver,
    value: Value,
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

struct ViewportValueOptionsArgs {
    receiver: ViewportReceiver,
    value: Value,
    options: Option<Value>,
}

impl ViewportValueOptionsArgs {
    fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportValueOptionsArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "method expected viewport self, value, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let value = JsonArg::from_lua(values.remove(0), scope)?.0;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            value,
            options,
        })
    }
}

struct ViewportRawPointerButtonArgs {
    receiver: ViewportReceiver,
    point: Value,
    button: Value,
    action: String,
    options: Option<Value>,
}

impl ViewportRawPointerButtonArgs {
    fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportRawPointerButtonArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(4..=5).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "raw_pointer_button expected self, point, button, action, and optional modifiers, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let point = JsonArg::from_lua(values.remove(0), scope)?.0;
        let button = JsonArg::from_lua(values.remove(0), scope)?.0;
        let action = String::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            point,
            button,
            action,
            options,
        })
    }
}

struct ViewportRawKeyArgs {
    receiver: ViewportReceiver,
    key: String,
    action: String,
    options: Option<Value>,
}

impl ViewportRawKeyArgs {
    fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportRawKeyArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(3..=4).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "raw_key expected self, key, action, and optional modifiers, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let key = String::from_lua(values.remove(0), scope)?;
        let action = String::from_lua(values.remove(0), scope)?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            key,
            action,
            options,
        })
    }
}

struct ViewportValueStringArgs {
    receiver: ViewportReceiver,
    value: Value,
    text: String,
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

struct ViewportOverlayArgs {
    receiver: ViewportReceiver,
    mode: Option<Value>,
    options: Option<Value>,
}

impl<'s> FromLuaMulti<'s> for ViewportOverlayArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(1..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "show_debug_overlay expected self, optional mode, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let mode = optional_json_value(
            scope,
            if values.is_empty() {
                None
            } else {
                Some(values.remove(0))
            },
        )?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            mode,
            options,
        })
    }
}

struct ViewportPredicateArgs {
    receiver: ViewportReceiver,
    predicate: StashedClosure,
    options: Option<Value>,
}

impl ViewportPredicateArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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

struct ViewportWidgetPredicateArgs {
    receiver: ViewportReceiver,
    widget_id: String,
    predicate: StashedClosure,
    options: Option<Value>,
}

impl ViewportWidgetPredicateArgs {
    fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for ViewportWidgetPredicateArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(3..=4).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "wait_for_widget expected self, id, predicate, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = ViewportReceiver::from_lua(values.remove(0), scope)?;
        let widget_id = String::from_lua(values.remove(0), scope)?;
        let predicate = stashed_function_arg(scope, "wait_for_widget", values.remove(0))?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            widget_id,
            predicate,
            options,
        })
    }
}

struct WidgetReceiver {
    value: Value,
    id: String,
    viewport_id: Option<String>,
}

impl WidgetReceiver {
    fn options_with_viewport(&self, mut options: Option<Value>) -> Option<Value> {
        if let Some(viewport_id) = &self.viewport_id {
            inject_viewport_id(viewport_id.clone(), &mut options);
        }
        options
    }

    fn widget_ref(&self) -> WidgetRef {
        WidgetRef {
            id: Some(self.id.clone()),
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

struct WidgetStringArgs {
    receiver: WidgetReceiver,
    value: String,
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

struct WidgetValueArgs {
    receiver: WidgetReceiver,
    value: Value,
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

struct WidgetGridArgs {
    receiver: WidgetReceiver,
    nx: Value,
    ny: Value,
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

struct WidgetOverlayArgs {
    receiver: WidgetReceiver,
    mode: Option<Value>,
    options: Option<Value>,
}

impl<'s> FromLuaMulti<'s> for WidgetOverlayArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(1..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "show_debug_overlay expected self, optional mode, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let mode = optional_json_value(
            scope,
            if values.is_empty() {
                None
            } else {
                Some(values.remove(0))
            },
        )?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            mode,
            options,
        })
    }
}

struct WidgetPredicateArgs {
    receiver: WidgetReceiver,
    predicate: StashedClosure,
    options: Option<Value>,
}

impl WidgetPredicateArgs {
    fn options_with_viewport(&self) -> Option<Value> {
        self.receiver.options_with_viewport(self.options.clone())
    }
}

impl<'s> FromLuaMulti<'s> for WidgetPredicateArgs {
    fn from_lua_multi(values: MultiValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        let mut values = values.into_vec();
        if !(2..=3).contains(&values.len()) {
            return Err(RuntimeError::runtime(format!(
                "wait_for expected self, predicate, and optional options, got {} arguments",
                values.len()
            )));
        }
        let receiver = WidgetReceiver::from_lua(values.remove(0), scope)?;
        let predicate = stashed_function_arg(scope, "wait_for", values.remove(0))?;
        let options = optional_json_value(scope, values.pop())?;
        Ok(Self {
            receiver,
            predicate,
            options,
        })
    }
}

struct WidgetOptionsArgs {
    receiver: WidgetReceiver,
    options: Option<Value>,
}

impl WidgetOptionsArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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

struct WidgetTextOptionsArgs {
    receiver: WidgetReceiver,
    text: String,
    options: Option<Value>,
}

impl WidgetTextOptionsArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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

struct WidgetValueOptionsArgs {
    receiver: WidgetReceiver,
    value: Value,
    options: Option<Value>,
}

impl WidgetValueOptionsArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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

struct WidgetDragRelativeArgs {
    receiver: WidgetReceiver,
    relative: Value,
    options: Option<Value>,
}

impl WidgetDragRelativeArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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
            if has_fourth || is_vec2_value(&value) {
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

struct WidgetDragToArgs {
    receiver: WidgetReceiver,
    target: WidgetReceiver,
    options: Option<Value>,
}

impl WidgetDragToArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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

fn optional_json_value<'s>(
    scope: &Scope<'s>,
    value: Option<ScopedValue<'s>>,
) -> Result<Option<Value>, RuntimeError> {
    value
        .map(|value| JsonArg::from_lua(value, scope).map(|value| value.0))
        .transpose()
        .map(|value| value.filter(|value| !value.is_null()))
}

fn stashed_function_arg<'s>(
    scope: &Scope<'s>,
    name: &str,
    value: ScopedValue<'s>,
) -> Result<StashedClosure, RuntimeError> {
    let ScopedValue::Function(function) = value else {
        return Err(RuntimeError::runtime(format!("{name} expected a function")));
    };
    scope.stash_function(function)
}

struct PredicateJsonArg {
    value: StashedValue,
}

impl<'s> IntoLuaMulti<'s> for PredicateJsonArg {
    fn into_lua_multi(self, scope: &Scope<'s>) -> Result<MultiValue<'s>, RuntimeError> {
        Ok(MultiValue::from_values(vec![
            scope.fetch_value(&self.value)?,
        ]))
    }
}

async fn predicate_matches(
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

async fn stash_predicate_value(
    ctx: &AsyncHostContext,
    value: Value,
) -> Result<StashedValue, RuntimeError> {
    ctx.scope(move |scope| {
        let value = typed_json_to_luau_scoped_value(scope, &value)?;
        scope.stash_value(value)
    })
    .await
}

fn predicate_bool_result(values: &[OwnedValue]) -> ScriptResult<bool> {
    match values {
        [OwnedValue::Boolean(value)] => Ok(*value),
        values => Err(type_error(format!(
            "wait predicate must return one boolean, got {}",
            owned_values_shape(values)
        ))),
    }
}

fn insert_option(options: &mut Option<Value>, key: &str, value: Value) -> Result<(), RuntimeError> {
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

fn is_vec2_value(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.len() == 2
        && map.get("x").and_then(Value::as_f64).is_some()
        && map.get("y").and_then(Value::as_f64).is_some()
}

fn parse_raw_action(action: &str) -> Result<bool, String> {
    match action {
        "press" => Ok(true),
        "release" => Ok(false),
        _ => Err("action must be \"press\" or \"release\"".to_string()),
    }
}

struct ConfigureFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for ConfigureFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let options = optional_json_arg(scope, "configure", args)?;
        self.runtime
            .configure(pos, options.as_ref().and_then(Value::as_object))
            .map_err(host_script_error)?;
        Ok(MultiValue::new())
    }
}

struct FixturesFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for FixturesFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        no_args("fixtures", &args)?;
        let pos = script_position_from_caller(scope);
        let value = self.runtime.fixtures(pos).map_err(host_script_error)?;
        single_typed_json_return(scope, &value)
    }
}

struct DumpFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for DumpFn {
    fn call<'s>(
        &self,
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
        let value = self.runtime.dump(pos, options).map_err(host_script_error)?;
        single_typed_json_return(scope, &value)
    }
}

struct DumpTextFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for DumpTextFn {
    fn call<'s>(
        &self,
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
        let value = self
            .runtime
            .dump_text(pos, options)
            .map_err(host_script_error)?;
        single_typed_json_return(scope, &value)
    }
}

struct RootFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for RootFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        no_args("root", &args)?;
        let pos = script_position_from_caller(scope);
        let value = self.runtime.root_viewport(pos).map_err(host_script_error)?;
        single_typed_json_table_return_with_metatable(scope, &value, VIEWPORT_METHODS)
    }
}

struct ViewportFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for ViewportFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let options = optional_json_arg(scope, "viewport", args)?;
        let options = match options.as_ref() {
            None => None,
            Some(Value::Object(map)) => Some(map),
            Some(_) => return Err(RuntimeError::runtime("viewport expected an options table")),
        };
        let value = self
            .runtime
            .viewport_lookup(pos, options)
            .map_err(host_script_error)?;
        optional_typed_json_table_return_with_metatable(scope, &value, VIEWPORT_METHODS)
    }
}

struct ViewportWidgetListFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for ViewportWidgetListFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let (viewport_id, mut options) = viewport_self_and_options(scope, "widget_list", args)?;
        inject_viewport_id(viewport_id, &mut options);
        let value = self
            .runtime
            .widget_list(pos, options.as_ref().and_then(Value::as_object))
            .map_err(host_script_error)?;
        single_typed_json_array_return_with_metatable(scope, &value, WIDGET_METHODS)
    }
}

struct ViewportStateFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for ViewportStateFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let viewport_id = viewport_self(scope, "state", args)?;
        let value = self
            .runtime
            .viewport_state(pos, viewport_id)
            .map_err(host_script_error)?;
        single_typed_json_return(scope, &value)
    }
}

struct ViewportWidgetGetFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for ViewportWidgetGetFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let (viewport_id, widget_id) = viewport_self_and_string(scope, "widget_get", args)?;
        let mut options = Some(Value::Object(serde_json::Map::new()));
        inject_viewport_id(viewport_id, &mut options);
        let target = Value::String(widget_id);
        let value = self
            .runtime
            .widget_get(pos, &target, options.as_ref().and_then(Value::as_object))
            .map_err(host_script_error)?;
        single_typed_json_table_return_with_metatable(scope, &value, WIDGET_METHODS)
    }
}

struct ViewportWidgetAtPointFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for ViewportWidgetAtPointFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let (viewport_id, point, mut options) =
            viewport_self_point_and_options(scope, "widget_at_point", args)?;
        inject_viewport_id(viewport_id, &mut options);
        let value = self
            .runtime
            .widget_at_point(pos, &point, options.as_ref().and_then(Value::as_object))
            .map_err(host_script_error)?;
        single_typed_json_array_return_with_metatable(scope, &value, WIDGET_METHODS)
    }
}

struct WidgetViewportFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for WidgetViewportFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let receiver = widget_receiver(scope, "viewport", args)?;
        let viewport_id = receiver.viewport_id.as_deref().unwrap_or("root");
        let value = self
            .runtime
            .viewport_handle(pos, viewport_id)
            .map_err(host_script_error)?;
        single_typed_json_table_return_with_metatable(scope, &value, VIEWPORT_METHODS)
    }
}

struct WidgetStateFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for WidgetStateFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let target = widget_self(scope, "state", args)?;
        let value = self
            .runtime
            .widget_state(pos, &target)
            .map_err(host_script_error)?;
        single_typed_json_return(scope, &value)
    }
}

struct WidgetParentFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for WidgetParentFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let target = widget_self(scope, "parent", args)?;
        let value = self
            .runtime
            .widget_parent(pos, &target)
            .map_err(host_script_error)?;
        optional_typed_json_table_return_with_metatable(scope, &value, WIDGET_METHODS)
    }
}

struct WidgetChildrenFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for WidgetChildrenFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let target = widget_self(scope, "children", args)?;
        let value = self
            .runtime
            .widget_children(pos, &target)
            .map_err(host_script_error)?;
        single_typed_json_array_return_with_metatable(scope, &value, WIDGET_METHODS)
    }
}

struct AssertFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for AssertFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let values = args.into_vec();
        let pos = script_position_from_caller(scope);
        let Some(condition) = values.first().copied() else {
            return Err(host_script_error(
                self.runtime
                    .type_error(pos, "assert expected a boolean condition"),
            ));
        };
        let condition = from_scoped_value::<bool>(scope, condition).map_err(|error| {
            host_script_error(
                self.runtime
                    .type_error(pos, format!("assert condition must be boolean: {error}")),
            )
        })?;
        let message = values
            .get(1)
            .copied()
            .map(|message| from_scoped_value::<String>(scope, message))
            .transpose()
            .map_err(|error| {
                host_script_error(
                    self.runtime
                        .type_error(pos, format!("assert message must be string: {error}")),
                )
            })?;
        self.runtime
            .assert_condition(pos, condition, message)
            .map_err(host_script_error)?;
        Ok(MultiValue::new())
    }
}

struct LogFn {
    runtime: Arc<ScriptRuntime>,
}

struct TryWidgetFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for TryWidgetFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let (id, options) = string_and_options_arg(scope, "try_widget", args)?;
        let value = self
            .runtime
            .try_widget_find(pos, id, options.as_ref().and_then(Value::as_object))
            .map_err(host_script_error)?;
        optional_typed_json_table_return_with_metatable(scope, &value, WIDGET_METHODS)
    }
}

struct CaptureFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for CaptureFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        no_args("capture", &args)?;
        let pos = script_position_from_caller(scope);
        let value = self.runtime.capture(pos).map_err(host_script_error)?;
        single_typed_json_table_return_with_metatable(scope, &value, CAPTURE_METHODS)
    }
}

struct CaptureDiffFn {
    runtime: Arc<ScriptRuntime>,
}

impl ScopedHostFunction for CaptureDiffFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        let pos = script_position_from_caller(scope);
        let (capture, options) = capture_self_and_options(scope, "diff", args)?;
        let value = self
            .runtime
            .capture_diff(pos, &capture, options.as_ref().and_then(Value::as_object))
            .map_err(host_script_error)?;
        single_typed_json_return(scope, &value)
    }
}

impl ScopedHostFunction for LogFn {
    fn call<'s>(
        &self,
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
        self.runtime.log(rendered);
        Ok(MultiValue::new())
    }
}

struct ArgsFn {
    args: Value,
}

impl ScopedHostFunction for ArgsFn {
    fn call<'s>(
        &self,
        scope: &Scope<'s>,
        args: MultiValue<'s>,
    ) -> Result<MultiValue<'s>, RuntimeError> {
        no_args("__eguidev_args", &args)?;
        let value = lossless_json_to_luau_scoped_value(scope, &self.args)?;
        let ScopedValue::Table(table) = value else {
            return Err(RuntimeError::runtime(
                "script args did not convert to a table",
            ));
        };
        table.freeze_deep(scope)?;
        Ok(MultiValue::from_values(vec![ScopedValue::Table(table)]))
    }
}

fn no_args(name: &str, args: &MultiValue<'_>) -> Result<(), RuntimeError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError::runtime(format!(
            "{name} expected no arguments, got {}",
            args.len()
        )))
    }
}

fn one_arg<'s>(name: &str, args: MultiValue<'s>) -> Result<ScopedValue<'s>, RuntimeError> {
    let mut values = args.into_vec();
    match values.len() {
        1 => Ok(values.remove(0)),
        got => Err(RuntimeError::runtime(format!(
            "{name} expected one argument, got {got}"
        ))),
    }
}

fn optional_json_arg<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<Option<Value>, RuntimeError> {
    let mut values = args.into_vec();
    match values.len() {
        0 => Ok(None),
        1 => scoped_value_to_json(scope, values.remove(0))
            .map(normalize_integral_numbers)
            .map(Some),
        got => Err(RuntimeError::runtime(format!(
            "{name} expected at most one argument, got {got}"
        ))),
    }
}

fn string_and_options_arg<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<(String, Option<Value>), RuntimeError> {
    let mut values = args.into_vec();
    if !(1..=2).contains(&values.len()) {
        return Err(RuntimeError::runtime(format!(
            "{name} expected string and optional options, got {} arguments",
            values.len()
        )));
    }
    let value = from_scoped_value::<String>(scope, values.remove(0))?;
    let options = values
        .pop()
        .map(|value| scoped_value_to_json(scope, value))
        .map(|value| value.map(normalize_integral_numbers))
        .transpose()?
        .filter(|value| !value.is_null());
    Ok((value, options))
}

fn capture_self_and_options<'s>(
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
    let capture = scoped_value_to_json(scope, values.remove(0)).map(normalize_integral_numbers)?;
    let options = values
        .pop()
        .map(|value| scoped_value_to_json(scope, value))
        .map(|value| value.map(normalize_integral_numbers))
        .transpose()?
        .filter(|value| !value.is_null());
    Ok((capture, options))
}

fn viewport_self_and_options<'s>(
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
        .map(|value| value.map(normalize_integral_numbers))
        .transpose()?
        .filter(|value| !value.is_null());
    Ok((viewport_id, options))
}

fn viewport_self<'s>(
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

fn viewport_self_and_string<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<(String, String), RuntimeError> {
    let mut values = args.into_vec();
    if values.len() != 2 {
        return Err(RuntimeError::runtime(format!(
            "{name} expected self and string argument, got {} arguments",
            values.len()
        )));
    }
    let viewport_id = viewport_id_from_self(scope, name, values.remove(0))?;
    let value = from_scoped_value::<String>(scope, values.remove(0))?;
    Ok((viewport_id, value))
}

fn viewport_self_point_and_options<'s>(
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
    let point = scoped_value_to_json(scope, values.remove(0)).map(normalize_integral_numbers)?;
    let options = widget_at_point_options(scope, values.pop())?;
    Ok((viewport_id, point, options))
}

fn viewport_id_from_self<'s>(
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

fn widget_self<'s>(
    scope: &Scope<'s>,
    name: &str,
    args: MultiValue<'s>,
) -> Result<Value, RuntimeError> {
    Ok(widget_receiver(scope, name, args)?.value)
}

fn widget_receiver<'s>(
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

fn widget_at_point_options<'s>(
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
        Some(value) => scoped_value_to_json(scope, value)
            .map(normalize_integral_numbers)
            .map(Some),
    }
}

fn inject_viewport_id(viewport_id: String, options: &mut Option<Value>) {
    let map = options
        .get_or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut();
    if let Some(map) = map {
        map.entry("viewport_id")
            .or_insert(Value::String(viewport_id));
    }
}

fn single_typed_json_return<'s>(
    scope: &Scope<'s>,
    value: &Value,
) -> Result<MultiValue<'s>, RuntimeError> {
    let value = match value {
        Value::Array(_) => typed_json_array_to_luau_scoped_value(scope, value)?,
        _ => typed_json_to_luau_scoped_value(scope, value)?,
    };
    Ok(MultiValue::from_values(vec![value]))
}

fn single_typed_json_table_return_with_metatable<'s>(
    scope: &Scope<'s>,
    value: &Value,
    methods: &'static [u8],
) -> Result<MultiValue<'s>, RuntimeError> {
    Ok(MultiValue::from_values(vec![
        typed_json_table_value_with_metatable(scope, value, methods)?,
    ]))
}

fn optional_typed_json_table_return_with_metatable<'s>(
    scope: &Scope<'s>,
    value: &Value,
    methods: &'static [u8],
) -> Result<MultiValue<'s>, RuntimeError> {
    if value.is_null() {
        return Ok(MultiValue::from_values(vec![ScopedValue::Nil]));
    }
    single_typed_json_table_return_with_metatable(scope, value, methods)
}

fn single_typed_json_array_return_with_metatable<'s>(
    scope: &Scope<'s>,
    value: &Value,
    methods: &'static [u8],
) -> Result<MultiValue<'s>, RuntimeError> {
    Ok(MultiValue::from_values(vec![
        typed_json_array_value_with_metatable(scope, value, methods)?,
    ]))
}

async fn typed_json_array_host_return_with_metatable(
    ctx: &AsyncHostContext,
    value: Value,
    methods: &'static [u8],
) -> Result<HostReturn, RuntimeError> {
    let value = ctx
        .scope(move |scope| {
            let scoped = typed_json_array_value_with_metatable(scope, &value, methods)?;
            Ok(scope.stash_value(scoped)?.into_owned_value())
        })
        .await?;
    Ok(HostReturn {
        values: vec![value],
    })
}

async fn typed_json_host_return_with_metatable(
    ctx: &AsyncHostContext,
    value: Value,
    methods: &'static [u8],
) -> Result<HostReturn, RuntimeError> {
    if value.is_null() {
        return typed_scalar_host_return(value);
    }
    let value = ctx
        .scope(move |scope| {
            let scoped = typed_json_table_value_with_metatable(scope, &value, methods)?;
            Ok(scope.stash_value(scoped)?.into_owned_value())
        })
        .await?;
    Ok(HostReturn {
        values: vec![value],
    })
}

async fn typed_json_host_return(
    ctx: &AsyncHostContext,
    value: Value,
) -> Result<HostReturn, RuntimeError> {
    match value {
        Value::Array(_) => {
            let value = ctx
                .scope(move |scope| {
                    let scoped = typed_json_array_to_luau_scoped_value(scope, &value)?;
                    Ok(scope.stash_value(scoped)?.into_owned_value())
                })
                .await?;
            Ok(HostReturn {
                values: vec![value],
            })
        }
        Value::Object(_) => {
            let value = ctx
                .scope(move |scope| {
                    let scoped = typed_json_to_luau_scoped_value(scope, &value)?;
                    Ok(scope.stash_value(scoped)?.into_owned_value())
                })
                .await?;
            Ok(HostReturn {
                values: vec![value],
            })
        }
        value => typed_scalar_host_return(value),
    }
}

fn typed_json_table_value_with_metatable<'s>(
    scope: &Scope<'s>,
    value: &Value,
    methods: &'static [u8],
) -> Result<ScopedValue<'s>, RuntimeError> {
    let value = typed_json_to_luau_scoped_value(scope, value)?;
    let ScopedValue::Table(table) = value else {
        return Err(RuntimeError::runtime(
            "JSON value did not convert to a table handle",
        ));
    };
    attach_metatable(scope, table, methods)?;
    Ok(ScopedValue::Table(table))
}

fn typed_json_array_value_with_metatable<'s>(
    scope: &Scope<'s>,
    value: &Value,
    methods: &'static [u8],
) -> Result<ScopedValue<'s>, RuntimeError> {
    let value = typed_json_array_to_luau_scoped_value(scope, value)?;
    let ScopedValue::Table(table) = value else {
        return Err(RuntimeError::runtime(
            "JSON value did not convert to a table array",
        ));
    };
    let len = table.len(scope)?;
    for index in 1..=len {
        let value = table.get::<_, ScopedValue<'_>>(scope, index as f64)?;
        if let ScopedValue::Table(item) = value {
            attach_metatable(scope, item, methods)?;
        }
    }
    Ok(ScopedValue::Table(table))
}

fn attach_metatable<'s>(
    scope: &Scope<'s>,
    table: Table<'s>,
    methods: &'static [u8],
) -> Result<(), RuntimeError> {
    let methods = scope
        .named_get(methods)
        .ok_or_else(|| RuntimeError::runtime("method table is not registered"))?;
    let metatable = scope.create_table()?;
    metatable.set(scope, "__index", methods)?;
    table.set_metatable(scope, Some(metatable))
}

fn optional_luau_number_to_json(value: Option<f64>) -> Result<Value, RuntimeError> {
    value.map_or(Ok(Value::Null), |value| {
        if value.fract() == 0.0 && value >= 0.0 && value <= u64::MAX as f64 {
            return Ok(Value::Number((value as u64).into()));
        }
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| RuntimeError::runtime("number argument must be finite"))
    })
}

fn typed_scalar_host_return(value: Value) -> Result<HostReturn, RuntimeError> {
    let value = match value {
        Value::Null => OwnedValue::Nil,
        Value::Bool(value) => OwnedValue::Boolean(value),
        Value::Number(number) => json_number_to_luau_owned_value(&number)?,
        Value::String(value) => OwnedValue::Bytes(value.into_bytes()),
        Value::Array(_) | Value::Object(_) => {
            return Err(RuntimeError::runtime(
                "host function returned a non-scalar JSON value",
            ));
        }
    };
    Ok(HostReturn {
        values: vec![value],
    })
}

fn json_number_to_luau_owned_value(
    number: &serde_json::Number,
) -> Result<OwnedValue, RuntimeError> {
    number
        .as_f64()
        .map(OwnedValue::Number)
        .ok_or_else(|| RuntimeError::runtime("number return must be finite"))
}

fn script_position_from_caller(scope: &Scope<'_>) -> ScriptPosition {
    script_position_from_location(scope.caller_location(0))
}

async fn script_position_from_context(
    ctx: &AsyncHostContext,
) -> Result<ScriptPosition, RuntimeError> {
    let location = ctx.scope(|scope| Ok(scope.caller_location(0))).await?;
    Ok(script_position_from_location(location))
}

fn script_position_from_location(location: Option<SourceLocation>) -> ScriptPosition {
    location
        .map(|location| ScriptPosition {
            line: Some(location.line as usize),
            column: None,
        })
        .unwrap_or_default()
}

fn host_script_error(info: ScriptErrorInfo) -> RuntimeError {
    RuntimeError::runtime(info.message.clone()).with_payload(info)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use serde_json::json;
    use tokio::runtime::Builder as TokioRuntimeBuilder;

    use super::{
        is_supported_by_initial_ruau_slice, promote_integer_numbers_to_luau_numbers,
        run_script_eval_blocking,
    };
    use crate::{
        DevMcp,
        fixtures::FixtureHandler,
        registry::Inner,
        runtime::{self, Runtime},
        tools::script::types::{ScriptArgValue, ScriptArgs},
        types::{
            FixtureParam, FixtureResponse, FixtureSpec, Pos2, Rect, WidgetRegistryEntry,
            WidgetRole, WidgetValue,
        },
    };

    #[test]
    fn initial_ruau_slice_accepts_value_and_log_scripts() {
        assert!(is_supported_by_initial_ruau_slice(
            r#"assert(type(args) == "table")
assert_widget_exists("status")
log("hello")
return { kind = type(args), value = 1 + 1 }"#
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_fixture_globals() {
        assert!(is_supported_by_initial_ruau_slice(
            r#"configure({ timeout_ms = 20, poll_interval_ms = 1, settle = false, animations = true })
fixture_raw("seed")
fixture("ready")
wait_for_capture()
wait_for_frames(1)
local catalog = fixtures()
local ready = diagnostic("ready")
local all = diagnostics()
wait_until(function()
    return ready.ok and all.values ~= nil
end)
return catalog[1].name"#
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_root_widget_list() {
        assert!(is_supported_by_initial_ruau_slice(
            r#"local widgets = root():widget_list({ id_prefix = "status" })
return widgets[1].id"#
        ));
        assert!(is_supported_by_initial_ruau_slice(
            r#"local found = widget("status")
local maybe = try_widget("missing")
expect("status", { visible = true })
expect_absent("missing")
return found.id ~= nil and maybe == nil"#
        ));
        assert!(is_supported_by_initial_ruau_slice(
            r#"local widget = root():widget_get("status")
return widget:state().role"#
        ));
        assert!(is_supported_by_initial_ruau_slice(
            r#"local widgets = root():widget_at_point({ x = 1, y = 1 }, true)
return #widgets"#
        ));
        assert!(is_supported_by_initial_ruau_slice(
            r#"local before = capture()
local diff = before:diff({ id_prefix = "status" })
return #diff.changes"#
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_widget_actions() {
        assert!(is_supported_by_initial_ruau_slice(
            r#"local viewport = root()
local first = viewport:widget_get("first")
local second = viewport:widget_get("second")
first:viewport():focus()
first:click({ settle = false })
first:hover({ settle = false })
first:type_text("hello", { settle = false })
first:focus()
first:set_value(true, { settle = false })
first:drag({ x = 5, y = 5 }, { settle = false })
first:drag_relative({ x = 0.8, y = 0.5 }, { x = 0.2, y = 0.5 }, { settle = false })
first:drag_to(second, { settle = false })
first:scroll({ x = 0, y = -10 }, { settle = false })
first:scroll_to({ align = "top", settle = false })
first:scroll_into_view({ settle = false })
return true"#
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_viewport_actions() {
        assert!(is_supported_by_initial_ruau_slice(
            r#"local viewport = root()
viewport:wait_for_settle()
viewport:wait_for_capture()
viewport:dismiss_popups()
viewport:key("enter", { settle = false })
viewport:paste("hello", { settle = false })
viewport:raw_pointer_move({ x = 1, y = 2 })
viewport:raw_pointer_button({ x = 1, y = 2 }, "primary", "press")
viewport:raw_key("enter", "release")
viewport:raw_text("hello")
viewport:raw_scroll({ x = 0, y = -10 })
viewport:focus()
viewport:set_inner_size({ x = 320, y = 240 })
viewport:set_resize_options({ resizable = true })
return true"#
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_visual_methods() {
        assert!(is_supported_by_initial_ruau_slice(
            r##"local viewport = root()
local widget = viewport:widget_get("status")
expect_left_of("left", "right")
expect_above("top", "bottom")
expect_no_overlap("first", "second")
expect_within("inner", "outer")
expect_text_fits("status")
expect_tree("parent", { "child" })
expect_painted("status", 2)
widget:text_measure()
widget:check_layout()
widget:screenshot()
widget:sample_pixels({ { x = 1, y = 1 } })
widget:sample_grid(2, 2)
widget:show_highlight("#ff0000")
widget:hide_highlight()
widget:show_debug_overlay("bounds", { show_labels = false })
widget:hide_debug_overlay()
viewport:check_layout()
viewport:screenshot()
viewport:sample_pixels({ { x = 1, y = 1 } })
viewport:show_highlight(
    { min = { x = 0, y = 0 }, max = { x = 10, y = 10 } },
    "#00ff00"
)
viewport:hide_highlight()
viewport:show_debug_overlay("bounds")
viewport:hide_debug_overlay()
return true"##
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_predicate_methods() {
        assert!(is_supported_by_initial_ruau_slice(
            r#"local viewport = root()
local widget = viewport:widget_get("status")
viewport:wait_for(function(current) return current.frame_count >= 0 end)
viewport:wait_for_widget("status", function(current) return current.visible end)
viewport:wait_for_widget_visible("status")
viewport:wait_for_widget_absent("missing", { timeout_ms = 10 })
widget:wait_for(function(current) return current.visible end, { timeout_ms = 10 })
widget:wait_for_visible()
widget:wait_for_scroll_ready()
widget:wait_for_absent({ timeout_ms = 10 })
return true"#
        ));
    }

    #[test]
    fn initial_ruau_slice_accepts_nil_literals() {
        assert!(is_supported_by_initial_ruau_slice("return { 1, nil, 3 }"));
    }

    #[test]
    fn initial_ruau_slice_rejects_parse_errors() {
        assert!(!is_supported_by_initial_ruau_slice("local x ="));
    }

    #[test]
    fn initial_ruau_slice_runs_value_and_log_script() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"log("hello")
return 1 + 1"#
                .to_string(),
            1_000,
            "probe.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(2)));
        assert_eq!(outcome.logs, vec!["hello"]);
    }

    #[test]
    fn initial_ruau_slice_runs_diagnostics_and_wait_until() {
        let devmcp = runtime::attach_for_tests(
            DevMcp::new()
                .diagnostic("ready", || Ok(json!({ "ready": true, "count": 2 })))
                .expect("diagnostic"),
        );
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let outcome = runtime.block_on(runtime::eval_script(
            &devmcp,
            r#"wait_until(function()
    return diagnostic("ready").ready
end)
return diagnostics()"#,
            Some(1_000),
            crate::ScriptEvalOptions {
                source_name: Some("diagnostics.luau".to_string()),
                args: ScriptArgs::default(),
            },
        ));

        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "values": {
                    "ready": {
                        "ready": true,
                        "count": 2,
                    },
                },
                "errors": {},
            }))
        );
    }

    #[test]
    fn initial_ruau_slice_wait_until_respects_configured_timeout() {
        let devmcp = runtime::attach_for_tests(DevMcp::new());
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let outcome = runtime.block_on(runtime::eval_script(
            &devmcp,
            r#"configure({ timeout_ms = 20, poll_interval_ms = 1 })
wait_until(function()
    return false
end)
"#,
            Some(1_000),
            crate::ScriptEvalOptions {
                source_name: Some("wait-until-timeout.luau".to_string()),
                args: ScriptArgs::default(),
            },
        ));

        assert!(!outcome.success, "{outcome:?}");
        let error = outcome.error.as_ref().expect("timeout error");
        assert_eq!(error.error_type, "timeout");
        assert_eq!(error.code.as_deref(), Some("timeout"));
        assert!(
            error
                .message
                .contains("Timed out waiting for a fresh capture"),
            "{error:?}"
        );
        assert!(
            outcome.timing.total_ms < 500,
            "wait_until should honor the configured timeout: {outcome:?}"
        );
    }

    #[test]
    fn initial_ruau_slice_collects_diagnostic_errors() {
        let devmcp = runtime::attach_for_tests(
            DevMcp::new()
                .diagnostic("broken", || {
                    Err(eguidev::DiagnosticError::new("broken", "diagnostic failed")
                        .with_details(json!({ "reason": "test" })))
                })
                .expect("diagnostic"),
        );
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let outcome = runtime.block_on(runtime::eval_script(
            &devmcp,
            r#"return diagnostics()"#,
            Some(1_000),
            crate::ScriptEvalOptions {
                source_name: Some("diagnostics.luau".to_string()),
                args: ScriptArgs::default(),
            },
        ));

        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "values": {},
                "errors": {
                    "broken": {
                        "code": "broken",
                        "message": "diagnostic failed",
                        "details": {
                            "reason": "test",
                        },
                    },
                },
            }))
        );
    }

    #[test]
    fn initial_ruau_slice_records_assertion_failures() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"assert(false, "nope")"#.to_string(),
            1_000,
            "probe.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(!outcome.success, "{outcome:?}");
        let error = outcome.error.expect("assertion error");
        assert_eq!(error.error_type, "assertion");
        assert_eq!(error.message, "nope");
        assert_eq!(outcome.assertions.len(), 1);
        assert!(!outcome.assertions[0].passed);
        assert_eq!(outcome.assertions[0].message, "nope");
        assert_eq!(outcome.assertions[0].location, "probe.luau:1");
    }

    #[test]
    fn initial_ruau_slice_runs_assert_widget_exists() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"assert_widget_exists("status")
return true"#
                .to_string(),
            1_000,
            "assert-widget.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(true)));
        assert_eq!(outcome.assertions.len(), 1);
        assert!(outcome.assertions[0].passed);
        assert_eq!(outcome.assertions[0].message, "widget exists");
        assert_eq!(outcome.assertions[0].location, "assert-widget.luau:1");
    }

    #[test]
    fn initial_ruau_slice_runs_configure_fixture_raw_and_fixtures() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("zeta", "Z fixture.")
                .anchor("status")
                .param(
                    FixtureParam::text("mode", "Selection mode.")
                        .default("fast")
                        .choices(["fast", "slow"]),
                ),
            FixtureSpec::new("alpha", "A fixture.").anchor("status"),
        ]);
        let applied = Arc::new(AtomicBool::new(false));
        let applied_c = Arc::clone(&applied);
        inner
            .fixtures
            .set_handler(FixtureHandler::Runtime(Arc::new(move |call| {
                assert_eq!(call.name, "zeta");
                assert_eq!(call.params.text("mode"), "slow");
                applied_c.store(true, Ordering::SeqCst);
                Ok(FixtureResponse::new())
            })))
            .expect("fixture handler");

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            Arc::clone(&inner),
            runtime,
            r#"configure({ timeout_ms = 20, poll_interval_ms = 1, settle = false, animations = true })
	fixture_raw("zeta", { mode = "slow" })
	local frame = wait_for_frames(0)
	local catalog = fixtures()
	log(catalog[1].name)
	return { first = catalog[1].name, count = #catalog, frame = frame, params = catalog[2].params[1].name }"#
                .to_string(),
            1_000,
            "fixtures.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert!(applied.load(Ordering::SeqCst));
        assert_eq!(outcome.logs, vec!["alpha"]);
        assert_eq!(outcome.fixtures.len(), 1);
        assert_eq!(outcome.fixtures[0].name, "zeta");
        assert_eq!(
            outcome.fixtures[0].params.get("mode"),
            Some(&WidgetValue::Text("slow".to_string()))
        );
        assert_eq!(
            outcome.value,
            Some(json!({ "first": "alpha", "count": 2, "frame": 0, "params": "mode" }))
        );
        assert!(inner.automation_options().animations);
    }

    #[test]
    fn initial_ruau_slice_runs_root_widget_list() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
        inner
            .widgets
            .record_widget(viewport_id, make_entry("other", 2, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"local widgets = root():widget_list({ id_prefix = "status" })
return { count = #widgets, id = widgets[1].id, viewport = widgets[1].viewport_id }"#
                .to_string(),
            1_000,
            "root-widget-list.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({ "count": 1, "id": "status", "viewport": "root" }))
        );
    }

    #[test]
    fn initial_ruau_slice_runs_widget_handle_reads() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        let mut root = make_entry("panel", 1, WidgetRole::Window);
        root.label = Some("Panel".to_string());
        inner.widgets.record_widget(viewport_id, root);
        let mut child = make_entry("status", 2, WidgetRole::Button);
        child.parent_id = Some("panel".to_string());
        child.label = Some("Ready".to_string());
        inner.widgets.record_widget(viewport_id, child);
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"local viewport = root()
local widget = viewport:widget_get("status")
local state = widget:state()
local parent = widget:parent()
local hits = viewport:widget_at_point({ x = 1, y = 1 }, true)
return {
    role = state.role,
    label = state.label,
    parent_id = parent and parent.id or "",
    sibling_count = #parent:children(),
    hit_count = #hits,
    top_hit = hits[1].id,
}"#
            .to_string(),
            1_000,
            "widget-reads.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "role": "button",
                "label": "Ready",
                "parent_id": "panel",
                "sibling_count": 1,
                "hit_count": 2,
                "top_hit": "status",
            }))
        );
    }

    #[test]
    fn initial_ruau_slice_runs_widget_actions() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("button", 1, WidgetRole::Button));
        inner
            .widgets
            .record_widget(viewport_id, make_entry("checkbox", 2, WidgetRole::Checkbox));
        inner
            .widgets
            .record_widget(viewport_id, make_entry("input", 3, WidgetRole::TextEdit));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            Arc::clone(&inner),
            runtime,
            r#"configure({ settle = false })
local viewport = root()
local button = viewport:widget_get("button")
local checkbox = viewport:widget_get("checkbox")
local input = viewport:widget_get("input")
button:click({ settle = false, click_count = 2 })
button:hover({ settle = false })
button:drag_relative({ x = 0.8, y = 0.5 }, { x = 0.2, y = 0.5 }, { settle = false })
checkbox:set_value(true, { settle = false })
input:type_text("hello", { settle = false })
input:focus()
return { viewport = button:viewport().id }"#
                .to_string(),
            1_000,
            "widget-actions.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!({ "viewport": "root" })));
        assert!(
            !inner
                .actions
                .drain_actions(viewport_id, inner.frame_count())
                .is_empty()
        );
    }

    #[test]
    fn initial_ruau_slice_runs_viewport_actions() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"configure({ settle = false })
local viewport = root()
viewport:dismiss_popups()
viewport:key("enter", { settle = false })
viewport:paste("hello", { settle = false })
viewport:raw_pointer_move({ x = 1, y = 2 })
viewport:raw_pointer_button({ x = 1, y = 2 }, "primary", "press")
viewport:raw_key("enter", "release")
viewport:raw_text("hello")
viewport:raw_scroll({ x = 0, y = -10 })
viewport:set_inner_size({ x = 320, y = 240 })
viewport:set_resize_options({ resizable = true })
return true"#
                .to_string(),
            1_000,
            "viewport-actions.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(true)));
    }

    #[test]
    fn initial_ruau_slice_runs_visual_methods_without_screenshots() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r##"local viewport = root()
local widget = viewport:widget_get("status")
local widget_issues = widget:check_layout()
local viewport_issues = viewport:check_layout()
widget:show_highlight("#ff0000")
widget:hide_highlight()
widget:show_debug_overlay("bounds", { show_labels = false })
widget:hide_debug_overlay()
viewport:show_highlight(
    { min = { x = 0, y = 0 }, max = { x = 10, y = 10 } },
    "#00ff00"
)
viewport:hide_highlight()
viewport:show_debug_overlay("bounds")
viewport:hide_debug_overlay()
return { widget_issues = #widget_issues, viewport_issues = #viewport_issues }"##
                .to_string(),
            1_000,
            "visual-methods.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({ "widget_issues": 0, "viewport_issues": 0 }))
        );
    }

    #[test]
    fn initial_ruau_slice_runs_predicate_methods() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.label = Some("Ready".to_string());
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);
        inner.viewports.update_viewports(&egui::Context::default());

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"configure({ timeout_ms = 20, poll_interval_ms = 1 })
local viewport = root()
local widget = viewport:widget_get("status")
local from_viewport = viewport:wait_for_widget("status", function(current)
    log("widget:" .. current.label)
    return root():state().frame_count ~= nil and current.label == "Ready"
end)
local from_widget = widget:wait_for(function(current)
    return current.visible and current.label == "Ready"
end, { timeout_ms = 20, poll_interval_ms = 1 })
local viewport_state = viewport:wait_for(function(current)
    return current.frame_count ~= nil
end, { timeout_ms = 20, poll_interval_ms = 1 })
local visible = viewport:wait_for_widget_visible("status")
viewport:wait_for_widget_absent("missing", { timeout_ms = 20, poll_interval_ms = 1 })
return {
    viewport_label = from_viewport.label,
    widget_label = from_widget.label,
    visible = visible.visible,
    frame_count = viewport_state.frame_count,
}"#
            .to_string(),
            1_000,
            "predicate-methods.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "viewport_label": "Ready",
                "widget_label": "Ready",
                "visible": true,
                "frame_count": 0,
            }))
        );
        assert_eq!(outcome.logs, vec!["widget:Ready"]);
    }

    #[test]
    fn initial_ruau_slice_keeps_widget_state_numbers_comparable_to_luau_numbers() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("choice", 1, WidgetRole::ComboBox);
        entry.value = Some(WidgetValue::Int(2));
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);
        inner.viewports.update_viewports(&egui::Context::default());

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"configure({ timeout_ms = 20, poll_interval_ms = 1 })
local viewport = root()
local current = viewport:widget_get("choice"):state()
assert(current.value == 2)
local matched = viewport:wait_for_widget("choice", function(widget)
    return widget.value == 2
end)
return matched.value"#
                .to_string(),
            1_000,
            "widget-state-numbers.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(2)));
    }

    #[test]
    fn initial_ruau_slice_keeps_integer_args_comparable_to_luau_numbers() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"assert(args.count == 4)
return args.count"#
                .to_string(),
            1_000,
            "args.luau".to_string(),
            ScriptArgs::from([("count".to_string(), ScriptArgValue::Int(4))]),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(4)));
    }

    #[test]
    fn nested_sample_arrays_are_promoted_for_luau_arithmetic() {
        let mut value = json!({
            "samples": [
                {
                    "position": { "x": 12.5, "y": 8.0 },
                    "physical": [25, 16],
                    "rgba": [47, 128, 237, 255],
                    "hex": "#2f80edff",
                }
            ]
        });

        promote_integer_numbers_to_luau_numbers(&mut value);

        let sample = &value["samples"][0];
        assert_eq!(sample["rgba"][0].as_f64(), Some(47.0));
        assert_eq!(sample["rgba"][0].as_i64(), None);
        assert_eq!(sample["rgba"][0], json!(47.0));
        assert_eq!(sample["physical"][0].as_f64(), Some(25.0));
    }

    fn make_entry(id: &str, native_id: u64, role: WidgetRole) -> WidgetRegistryEntry {
        let rect = Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        };
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
            parent_id: None,
            enabled: true,
            visible: true,
            focused: false,
        }
    }
}
