#![allow(clippy::missing_docs_in_private_items, clippy::result_large_err)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use ruau::{
    bytecode::{CompileError, CompileErrorKind, CompileOptions},
    declaration::DeclarationSource,
    module::{self},
    vm::{
        Ambient, AsyncHostContext, AsyncHostFunction, CallOptions, Deadline, FromLua, FromLuaMulti,
        HostReturn, IntoLuaMulti, Limits, LoadedModule, MarshaledScriptError, ModuleBinding,
        MultiValue, NativeModule, OwnedValue, RuntimeCapabilities, RuntimeError, RuntimeErrorKind,
        Scope, ScopedValue, ScriptErrorField, SourceLocation, StashedClosure, StashedValue,
        TracebackFrame, ValueSnapshot, Vm, async_host_fn,
        serde::{
            JsonDecodeOptions, from_scoped_value, json_to_scoped_value,
            json_to_scoped_value_with_options, marshaled_to_json, scoped_value_to_json,
        },
    },
};
use serde_json::Value;
use tokio::{
    runtime::Builder as TokioRuntimeBuilder,
    task::{JoinError, LocalSet, spawn_blocking},
};

use super::{
    outcome::{build_error_outcome, build_success_outcome, finalize_outcome},
    runtime::ScriptRuntime,
    types::{
        ScriptArgs, ScriptErrorInfo, ScriptEvalOutcome, ScriptLocation, ScriptPosition,
        ScriptResult, ScriptTiming,
    },
    value::{script_args_to_json, script_return_value_from_json_values, script_value_from_json},
};
use crate::{
    registry::Inner,
    runtime::Runtime,
    script::{CheckFailure, check_source, library},
    types::WidgetRef,
};

const EGUIDEV_SEED: u64 = 0x00e9_d1de;

pub async fn run_script_eval(
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
    let start = Instant::now();
    let compile_start = Instant::now();
    if let Err(error) = check_source(&source_name, &script) {
        let mut outcome = ScriptEvalOutcome::error_only(check_error_info(error));
        outcome.timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return outcome;
    }

    if start.elapsed() >= Duration::from_millis(timeout_ms) {
        let mut outcome = ScriptEvalOutcome::error_only(fatal_error_info(
            RuntimeErrorKind::Deadline,
            "source checking exceeded the shared deadline",
            timeout_ms,
        ));
        outcome.timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return outcome;
    }

    let script_runtime = Arc::new(ScriptRuntime::new_started_at(
        inner,
        runtime,
        source_name.clone(),
        timeout_ms,
        start,
    ));
    let runtime_capabilities = RuntimeCapabilities::default();

    let (module, _surface) = EguidevModule {
        args: script_args_to_luau_json(&args),
        runtime: Arc::clone(&script_runtime),
        declaration: library::DECLARATION.to_string(),
    }
    .build();
    let mut vm = match Vm::builder()
        .ambient(Ambient::production(EGUIDEV_SEED))
        .limits(base_limits())
        .runtime_capabilities(runtime_capabilities.clone())
        .module(module)
        .trusted_host()
        .build()
    {
        Ok(vm) => vm,
        Err(error) => {
            let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
            return finalize_outcome(
                &script_runtime,
                build_error_outcome(
                    &script_runtime,
                    runtime_error(format!("failed to build Ruau VM: {error}")),
                    timing,
                ),
            )
            .await;
        }
    };

    if let Err(error) = vm.sandbox_for_untrusted() {
        let timing = timing(start, compile_start.elapsed(), Duration::ZERO);
        return finalize_outcome(
            &script_runtime,
            build_error_outcome(
                &script_runtime,
                runtime_error(format!("failed to install Ruau sandbox: {error}")),
                timing,
            ),
        )
        .await;
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
            return finalize_outcome(
                &script_runtime,
                build_error_outcome(&script_runtime, error, timing),
            )
            .await;
        }
    };

    let compile_elapsed = compile_start.elapsed();
    if start.elapsed() >= Duration::from_millis(timeout_ms) {
        let timing = timing(start, compile_elapsed, Duration::ZERO);
        return finalize_outcome(
            &script_runtime,
            build_error_outcome(
                &script_runtime,
                script_runtime.script_timeout_error(ScriptPosition::default()),
                timing,
            ),
        )
        .await;
    }
    let exec_start = Instant::now();
    let outcome = vm
        .exec_async(
            &module,
            CallOptions::new().limits(invocation_limits(start, timeout_ms)),
        )
        .await;
    let timing = timing(start, compile_elapsed, exec_start.elapsed());

    let outcome = match outcome {
        Ok(values) => match values_to_script_value(&script_runtime, &values) {
            Ok(script_value) => build_success_outcome(&script_runtime, script_value, timing),
            Err(error) => build_error_outcome(&script_runtime, error, timing),
        },
        Err(error) => {
            if let Some(error) = error.script_error() {
                build_error_outcome(&script_runtime, ruau_script_error_info(error), timing)
            } else {
                let rendered_error = error.to_string();
                build_error_outcome(
                    &script_runtime,
                    fatal_error_info(error.kind(), &rendered_error, timeout_ms),
                    timing,
                )
            }
        }
    };
    finalize_outcome(&script_runtime, outcome).await
}

fn script_args_to_luau_json(args: &ScriptArgs) -> Value {
    script_args_to_json(args)
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

fn values_to_script_value(
    runtime: &ScriptRuntime,
    values: &[ValueSnapshot],
) -> Result<super::types::ScriptValue, ScriptErrorInfo> {
    let json_values = values
        .iter()
        .map(marshaled_script_value_to_json)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| type_error(format!("failed to convert Ruau result to JSON: {error}")))?;
    let Some(value) = script_return_value_from_json_values(json_values) else {
        return Ok(super::types::ScriptValue::default());
    };
    Ok(script_value_from_json(runtime, value))
}

fn marshaled_script_value_to_json(value: &ValueSnapshot) -> Result<Value, RuntimeError> {
    match marshaled_to_json(value) {
        Ok(value) => Ok(value),
        Err(error) => marshaled_sparse_array_to_json(value).unwrap_or(Err(error)),
    }
}

fn marshaled_sparse_array_to_json(value: &ValueSnapshot) -> Option<Result<Value, RuntimeError>> {
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

fn marshaled_positive_array_index(value: &ValueSnapshot) -> Option<usize> {
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

fn public_error_info(
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

fn check_error_info(error: CheckFailure) -> ScriptErrorInfo {
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
        message: value.display_lua(),
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

fn marshaled_error_text(value: &ValueSnapshot) -> String {
    match marshaled_to_json(value) {
        Ok(Value::String(message)) => message,
        Ok(value) if !value.is_null() => value.to_string(),
        Ok(_) => "null".to_string(),
        Err(error) => error.to_string(),
    }
}

fn owned_values_shape(values: &[OwnedValue]) -> String {
    if values.is_empty() {
        return "no values".to_string();
    }
    values
        .iter()
        .map(OwnedValue::type_name)
        .collect::<Vec<_>>()
        .join(", ")
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

fn invocation_limits(started_at: Instant, timeout_ms: u64) -> Limits {
    Limits {
        deadline: started_at
            .checked_add(Duration::from_millis(timeout_ms))
            .map(Deadline::Wall),
        ..base_limits()
    }
}

struct EguidevModule {
    args: Value,
    runtime: Arc<ScriptRuntime>,
    declaration: String,
}

/// Names the module registers, grouped by the table that receives them.
///
/// Ruau audits declared globals against the declaration source, but a hidden
/// method table has no declaration to audit. This record is what the
/// declaration parity test compares `eguidev.d.luau` against.
#[derive(Debug, Default)]
struct ModuleSurface {
    /// Global function names.
    globals: BTreeSet<String>,
    /// Method names keyed by hidden table name.
    methods: BTreeMap<String, BTreeSet<String>>,
}

impl ModuleSurface {
    fn record(&mut self, name: &str, binding: &ModuleBinding) {
        match binding {
            ModuleBinding::Global | ModuleBinding::GlobalOverride => {
                self.globals.insert(name.to_string());
            }
            ModuleBinding::Hidden(table) => {
                self.methods
                    .entry(table.to_string())
                    .or_default()
                    .insert(name.to_string());
            }
            ModuleBinding::Library(_) | ModuleBinding::LibraryOverride(_) => {}
        }
    }
}

struct DeclaredModuleBuilder<'a> {
    builder: &'a mut module::Builder,
    surface: ModuleSurface,
}

impl DeclaredModuleBuilder<'_> {
    fn borrowed_function<F>(&mut self, name: &str, binding: ModuleBinding, function: F)
    where
        F: for<'s> Fn(&Scope<'s>, MultiValue<'s>) -> Result<MultiValue<'s>, RuntimeError>
            + Send
            + Sync
            + 'static,
    {
        self.surface.record(name, &binding);
        self.builder
            .borrowed_function(name, library::declared_binding(binding), function);
    }

    fn async_function(
        &mut self,
        name: &str,
        binding: ModuleBinding,
        function: Box<dyn AsyncHostFunction>,
    ) {
        self.surface.record(name, &binding);
        self.builder.async_function(
            name,
            library::declared_binding(binding),
            Arc::from(function),
        );
    }
}

impl EguidevModule {
    /// Build the native module and report the surface it registered.
    fn build(self) -> (Arc<dyn NativeModule>, ModuleSurface) {
        let mut native = module::Builder::from_declaration(
            "eguidev_initial",
            DeclarationSource::Text(&self.declaration),
        );
        let mut builder = DeclaredModuleBuilder {
            builder: &mut native,
            surface: ModuleSurface::default(),
        };
        self.register_core_globals(&mut builder);
        self.register_viewport_methods(&mut builder);
        self.register_widget_methods(&mut builder);
        self.register_capture_methods(&mut builder);
        self.register_script_utility_globals(&mut builder);
        let surface = builder.surface;
        library::register(&mut native);
        let module = native
            .build()
            .expect("Eguidev declaration matches its runtime bindings");
        (module, surface)
    }

    fn register_core_globals(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "assertion",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, args| assert_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "configure",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, args| configure_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "apply",
            ModuleBinding::hidden("eguidev.fixture"),
            async_host_fn(move |ctx: AsyncHostContext, args: FixtureArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .fixture_apply(pos, args.name, args.params)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "list",
            ModuleBinding::hidden("eguidev.fixture"),
            move |scope, args| fixtures_host(&runtime, scope, &args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "get",
            ModuleBinding::hidden("eguidev.diagnostic"),
            async_host_fn(move |ctx: AsyncHostContext, name: String| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .diagnostic(pos, name)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "all",
            ModuleBinding::hidden("eguidev.diagnostic"),
            async_host_fn(move |ctx: AsyncHostContext, (): ()| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime.diagnostics(pos).await.map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "dump",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| dump_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "dump_text",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| dump_text_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_list",
            ModuleBinding::hidden("eguidev.query"),
            async_host_fn(move |ctx: AsyncHostContext, (): ()| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewports_list(pos, None)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
    }

    fn register_capture_methods(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "diff",
            ModuleBinding::hidden("eguidev.capture"),
            move |scope, args| capture_diff_host(&runtime, scope, args),
        );
    }

    fn register_viewport_methods(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "viewport_state",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| viewport_state_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "egui",
            ModuleBinding::hidden("eguidev.diagnostic"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .egui_diagnostics(pos, viewport.id)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "clear_egui",
            ModuleBinding::hidden("eguidev.diagnostic"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    runtime
                        .clear_egui_diagnostics(pos, viewport.id)
                        .await
                        .map_err(host_script_error)?;
                    Ok(HostReturn::default())
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "viewport_widget_list",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| viewport_widget_list_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "viewport_widget_at_point",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| viewport_widget_at_point_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_wait",
            ModuleBinding::hidden("eguidev.wait"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_focus",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .focus_window(
                            pos,
                            args.receiver.id,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_settle",
            ModuleBinding::hidden("eguidev.wait"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .wait_for_settle(pos, options.as_ref().and_then(Value::as_object))
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_dismiss_popups",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .viewport_dismiss_popups(
                            pos,
                            Some(args.receiver.id),
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_key",
            ModuleBinding::hidden("eguidev.action"),
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
                        ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                            .await
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_paste",
            ModuleBinding::hidden("eguidev.action"),
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
                        ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                            .await
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_input",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewport_input(pos, &args.value, args.receiver.id)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_resize",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .viewport_resize(pos, &args.value, args.receiver.id)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_screenshot",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let target = serde_json::json!({ "viewport_id": viewport.id });
                    let value = runtime
                        .screenshot(pos, Some(&target))
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_native_screenshot",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .native_screenshot(pos, viewport.id)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_sample_pixels",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportValueArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .sample_pixels(pos, &args.value, Some(args.receiver.id))
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_layout_issues",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, viewport: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .check_layout(pos, Some(viewport.id))
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_show_highlight",
            ModuleBinding::hidden("eguidev.action"),
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
                        ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                            .await
                    }
                },
            ),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_clear_highlights",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, _: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .clear_highlights(pos)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_show_debug_overlay",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_clear_debug_overlay",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, _: ViewportReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .clear_debug_overlay(pos)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
    }

    fn register_widget_methods(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_viewport",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_viewport_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_click",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_hover",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_type_text",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_focus",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let value = runtime
                        .action_focus(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_set_value",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_drag_position",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_drag_relative",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_drag_to",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_scroll",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_scroll_to",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_scroll_into_view",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_text_measure",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .text_measure(pos, &receiver.value)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_layout_issues",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .check_layout_widget(pos, &receiver.value, receiver.viewport_id)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_screenshot",
            ModuleBinding::hidden("eguidev.capture"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .screenshot(pos, Some(&receiver.value))
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_sample_pixels",
            ModuleBinding::hidden("eguidev.capture"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_sample_grid",
            ModuleBinding::hidden("eguidev.capture"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_show_highlight",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_clear_highlight",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, receiver: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .clear_widget_highlight(pos, &receiver.value, receiver.viewport_id)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_show_debug_overlay",
            ModuleBinding::hidden("eguidev.action"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_clear_debug_overlay",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(move |ctx: AsyncHostContext, _: WidgetReceiver| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = runtime
                        .clear_debug_overlay(pos)
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_wait",
            ModuleBinding::hidden("eguidev.wait"),
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
                            &args.condition,
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_wait_absent",
            ModuleBinding::hidden("eguidev.wait"),
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
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_state",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_state_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_parent",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_parent_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_children",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_children_host(&runtime, scope, args),
        );
    }

    fn register_script_utility_globals(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "capture",
            ModuleBinding::hidden("eguidev.wait"),
            async_host_fn(move |ctx: AsyncHostContext, options: OptionalJsonArg| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    runtime
                        .wait_for_fresh_capture(pos, options.0.as_ref().and_then(Value::as_object))
                        .await
                        .map_err(host_script_error)?;
                    Ok(HostReturn::default())
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "frames",
            ModuleBinding::hidden("eguidev.wait"),
            async_host_fn(move |ctx: AsyncHostContext, args: FrameCountOptionsArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let count = optional_luau_number_to_json(args.count)?;
                    let value = runtime
                        .wait_for_frames(
                            pos,
                            &count,
                            args.options.as_ref().and_then(Value::as_object),
                        )
                        .await
                        .map_err(host_script_error)?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "log",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, args| log_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "capture",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| capture_host(&runtime, scope, &args),
        );
        let args = self.args.clone();
        builder.borrowed_function(
            "args",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, values| args_host(&args, scope, &values),
        );
        builder.borrowed_function(
            "array",
            ModuleBinding::hidden("eguidev.record"),
            frozen_array_host,
        );
    }
}

struct JsonArg(Value);

impl<'s> FromLua<'s> for JsonArg {
    fn from_lua(value: ScopedValue<'s>, scope: &Scope<'s>) -> Result<Self, RuntimeError> {
        scoped_value_to_json(scope, value).map(Self)
    }
}

struct FrameCountOptionsArgs {
    count: Option<f64>,
    options: Option<Value>,
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

struct OptionalJsonArg(Option<Value>);

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

struct ViewportOptionsArgs {
    receiver: ViewportReceiver,
    options: Option<Value>,
}

impl ViewportOptionsArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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
    condition: String,
}

impl WidgetPredicateArgs {
    fn options_with_viewport(&self) -> Option<Value> {
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
        let value = json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?;
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

fn configure_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let options = optional_json_arg(scope, "configure", args)?;
    runtime
        .configure(pos, options.as_ref().and_then(Value::as_object))
        .map_err(host_script_error)?;
    Ok(MultiValue::new())
}

fn fixtures_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: &MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    no_args("fixtures", args)?;
    let pos = script_position_from_caller(scope);
    let value = runtime.fixtures(pos).map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn dump_host<'s>(
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
    let value = runtime.dump(pos, options).map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn dump_text_host<'s>(
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
    let value = runtime.dump_text(pos, options).map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn viewport_widget_list_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let (viewport_id, mut options) = viewport_self_and_options(scope, "widget_list", args)?;
    inject_viewport_id(viewport_id, &mut options);
    let value = runtime
        .widget_list(pos, options.as_ref().and_then(Value::as_object))
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn viewport_state_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let viewport_id = viewport_self(scope, "state", args)?;
    let value = runtime
        .viewport_state(pos, viewport_id)
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn viewport_widget_at_point_host<'s>(
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
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn widget_viewport_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let receiver = widget_receiver(scope, "viewport", args)?;
    let viewport_id = receiver.viewport_id.as_deref().unwrap_or("root");
    let value = runtime
        .viewport_handle(pos, viewport_id)
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn widget_state_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let target = widget_self(scope, "state", args)?;
    let value = runtime
        .widget_state(pos, &target)
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn widget_parent_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let target = widget_self(scope, "parent", args)?;
    let value = runtime
        .widget_parent(pos, &target)
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn widget_children_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let target = widget_self(scope, "children", args)?;
    let value = runtime
        .widget_children(pos, &target)
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn assert_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let values = args.into_vec();
    let pos = script_position_from_caller(scope);
    let Some(condition) = values.first().copied() else {
        return Err(host_script_error(
            runtime.type_error(pos, "assert expected a boolean condition"),
        ));
    };
    let condition = from_scoped_value::<bool>(scope, condition).map_err(|error| {
        host_script_error(
            runtime.type_error(pos, format!("assert condition must be boolean: {error}")),
        )
    })?;
    let message = values
        .get(1)
        .copied()
        .map(|message| from_scoped_value::<String>(scope, message))
        .transpose()
        .map_err(|error| {
            host_script_error(
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
    runtime.record_assertion_outcome(pos, condition, message);
    Ok(MultiValue::new())
}

fn capture_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: &MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    no_args("capture", args)?;
    let pos = script_position_from_caller(scope);
    let value = runtime.capture(pos).map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn capture_diff_host<'s>(
    runtime: &ScriptRuntime,
    scope: &Scope<'s>,
    args: MultiValue<'s>,
) -> Result<MultiValue<'s>, RuntimeError> {
    let pos = script_position_from_caller(scope);
    let (capture, options) = capture_self_and_options(scope, "diff", args)?;
    let value = runtime
        .capture_diff(pos, &capture, options.as_ref().and_then(Value::as_object))
        .map_err(host_script_error)?;
    json_to_scoped_value_with_options(scope, &value, JsonDecodeOptions::typed())?
        .into_lua_multi(scope)
}

fn log_host<'s>(
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

fn frozen_array_host<'s>(
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

fn args_host<'s>(
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
        1 => scoped_value_to_json(scope, values.remove(0)).map(Some),
        got => Err(RuntimeError::runtime(format!(
            "{name} expected at most one argument, got {got}"
        ))),
    }
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
    let capture = scoped_value_to_json(scope, values.remove(0))?;
    let options = values
        .pop()
        .map(|value| scoped_value_to_json(scope, value))
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
    let point = scoped_value_to_json(scope, values.remove(0))?;
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
        Some(value) => scoped_value_to_json(scope, value).map(Some),
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
    let mut fields = vec![ScriptErrorField::new("error_type", info.error_type.clone())];
    if let Some(code) = info.code.clone() {
        fields.push(ScriptErrorField::new("code", code));
    }
    if let Some(details) = info.details.as_ref() {
        fields.push(ScriptErrorField::new("details", details.to_string()));
    }
    RuntimeError::structured(info.message.clone(), fields).with_payload(info)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use eguidev::AutomationOptions;
    use ruau::vm::{
        Ambient, Function, Limits, RuntimeCapabilities, Vm, serde::json_to_scoped_value,
    };
    use serde_json::json;
    use tokio::runtime::Builder as TokioRuntimeBuilder;

    use super::{EguidevModule, run_script_eval_blocking};
    use crate::{
        DevMcp,
        automation::script::types::{ScriptArgValue, ScriptArgs},
        fixtures::FixtureHandler,
        registry::{Inner, viewport_id_to_string},
        runtime::{self, Runtime},
        types::{
            FixtureParam, FixtureResponse, FixtureSpec, Pos2, Rect, WidgetRegistryEntry,
            WidgetRole, WidgetValue,
        },
    };

    #[test]
    fn native_kernel_surface_matches_the_seven_private_capabilities_exactly() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let script_runtime = Arc::new(super::ScriptRuntime::new(
            inner,
            runtime,
            "kernel-surface.luau".to_string(),
            1_000,
        ));
        let (_, surface) = EguidevModule {
            args: serde_json::Value::Object(serde_json::Map::new()),
            runtime: script_runtime,
            declaration: super::library::DECLARATION.to_string(),
        }
        .build();

        let expected = BTreeMap::from([
            (
                "eguidev.action".to_string(),
                BTreeSet::from(
                    [
                        "viewport_clear_debug_overlay",
                        "viewport_clear_highlights",
                        "viewport_dismiss_popups",
                        "viewport_focus",
                        "viewport_input",
                        "viewport_key",
                        "viewport_paste",
                        "viewport_resize",
                        "viewport_show_debug_overlay",
                        "viewport_show_highlight",
                        "widget_clear_debug_overlay",
                        "widget_clear_highlight",
                        "widget_click",
                        "widget_drag_position",
                        "widget_drag_relative",
                        "widget_drag_to",
                        "widget_focus",
                        "widget_hover",
                        "widget_scroll",
                        "widget_scroll_into_view",
                        "widget_scroll_to",
                        "widget_set_value",
                        "widget_show_debug_overlay",
                        "widget_show_highlight",
                        "widget_type_text",
                    ]
                    .map(str::to_string),
                ),
            ),
            (
                "eguidev.capture".to_string(),
                BTreeSet::from(
                    [
                        "diff",
                        "viewport_layout_issues",
                        "viewport_sample_pixels",
                        "viewport_screenshot",
                        "viewport_native_screenshot",
                        "widget_layout_issues",
                        "widget_sample_grid",
                        "widget_sample_pixels",
                        "widget_screenshot",
                        "widget_text_measure",
                    ]
                    .map(str::to_string),
                ),
            ),
            (
                "eguidev.diagnostic".to_string(),
                BTreeSet::from(["all", "clear_egui", "egui", "get"].map(str::to_string)),
            ),
            (
                "eguidev.fixture".to_string(),
                BTreeSet::from(["apply", "list"].map(str::to_string)),
            ),
            (
                "eguidev.query".to_string(),
                BTreeSet::from(
                    [
                        "capture",
                        "dump",
                        "dump_text",
                        "viewport_list",
                        "viewport_state",
                        "viewport_widget_at_point",
                        "viewport_widget_list",
                        "widget_children",
                        "widget_parent",
                        "widget_state",
                        "widget_viewport",
                    ]
                    .map(str::to_string),
                ),
            ),
            (
                "eguidev.record".to_string(),
                BTreeSet::from(
                    ["args", "array", "assertion", "configure", "log"].map(str::to_string),
                ),
            ),
            (
                "eguidev.wait".to_string(),
                BTreeSet::from(
                    [
                        "capture",
                        "frames",
                        "viewport_settle",
                        "viewport_wait",
                        "widget_wait",
                        "widget_wait_absent",
                    ]
                    .map(str::to_string),
                ),
            ),
        ]);

        assert!(
            surface.globals.is_empty(),
            "unexpected globals: {surface:?}"
        );
        assert_eq!(surface.methods, expected);
    }

    #[test]
    fn initial_ruau_slice_runs_value_and_log_script() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.log("hello")
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
    fn typecheck_failure_prevents_tenant_execution() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.log("must not execute")
local state: WidgetState = eguidev.widget("missing"):state()
return state"#
                .to_string(),
            1_000,
            "typecheck_side_effect.luau".to_string(),
            ScriptArgs::default(),
        );

        assert!(!outcome.success, "{outcome:?}");
        assert!(outcome.logs.is_empty(), "{outcome:?}");
        assert_eq!(
            outcome
                .error
                .as_ref()
                .map(|error| error.error_type.as_str()),
            Some("typecheck")
        );
        let location = outcome
            .error
            .as_ref()
            .and_then(|error| error.location.as_ref())
            .expect("typecheck location");
        assert_eq!(location.line, 2);
        assert!(location.column.is_some());
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
            r#"eguidev.wait(function()
    return eguidev.diagnostic("ready").ready
end)
return eguidev.diagnostics()"#,
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
            r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 })
eguidev.wait(function()
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
            outcome.timing.exec_ms < 500,
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
            r#"return eguidev.diagnostics()"#,
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
            r#"eguidev.widget("missing"):expect(
    { present = true },
    { timeout_ms = 10, poll_interval_ms = 1 }
)"#
            .to_string(),
            1_000,
            "probe.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(!outcome.success, "{outcome:?}");
        let error = outcome.error.expect("assertion error");
        assert_eq!(error.error_type, "eguidev");
        assert_eq!(error.code.as_deref(), Some("expectation_failed"));
        assert!(error.message.contains("expectation failed"), "{error:?}");
        assert_eq!(outcome.assertions.len(), 1);
        assert!(!outcome.assertions[0].passed);
        assert!(outcome.assertions[0].message.contains("expectation failed"));
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
            r#"local state = eguidev.widget("status"):expect({ present = true })
return state ~= nil"#
                .to_string(),
            1_000,
            "assert-widget.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(true)));
        assert_eq!(outcome.assertions.len(), 1);
        assert!(outcome.assertions[0].passed);
        assert!(outcome.assertions[0].message.contains("expectation"));
    }

    #[test]
    fn script_eval_widget_predicate_receives_nil_when_absent() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.configure({ timeout_ms = 50, poll_interval_ms = 1 })
local seen_nil = false
local state = eguidev.widget("missing"):wait(function(current)
    if current == nil then
        seen_nil = true
        return true
    end
    return false
end)
return { seen_nil = seen_nil, state_nil = state == nil }"#
                .to_string(),
            1_000,
            "absent-predicate.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({ "seen_nil": true, "state_nil": true }))
        );
    }

    #[test]
    fn script_eval_expect_text_fits_on_absent_widget_records_failure() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.widget("missing"):expect(
    { present = false, text_fits = true },
    { timeout_ms = 20, poll_interval_ms = 1 }
)"#
            .to_string(),
            1_000,
            "expect-absent-text-fits.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(!outcome.success, "{outcome:?}");
        let error = outcome.error.expect("expectation error");
        assert_eq!(error.error_type, "eguidev");
        assert_eq!(error.code.as_deref(), Some("expectation_failed"));
        assert_eq!(outcome.assertions.len(), 1);
        assert!(!outcome.assertions[0].passed);
        assert!(outcome.assertions[0].message.contains("expectation failed"));
    }

    #[test]
    fn script_eval_data_condition_matches_tables_and_zero_based_array_index() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.data = Some(json!({
            "palette": { "name": "analysis", "rgba": [28, 34, 44] },
            "pass": "analysis"
        }));
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.configure({ timeout_ms = 50, poll_interval_ms = 1 })
local table_match = eguidev.widget("status"):wait({
    data = { pointer = "/palette", equals = { name = "analysis", rgba = { 28, 34, 44 } } },
})
local zero = eguidev.widget("status"):wait({
    data = { pointer = "/palette/rgba/0", equals = 28 },
})
local one = eguidev.widget("status"):wait({
    data = { pointer = "/palette/rgba/1", equals = 34 },
})
return { table_match = table_match ~= nil, zero = zero ~= nil, one = one ~= nil }"#
                .to_string(),
            1_000,
            "data-table-pointer.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({ "table_match": true, "zero": true, "one": true }))
        );
    }

    #[test]
    fn initial_ruau_slice_runs_configure_fixture_and_fixtures() {
        let inner = Arc::new(Inner::new());
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("zeta", "Z fixture.")
                .ready("status")
                .param(
                    FixtureParam::text("mode", "Selection mode.")
                        .default("fast")
                        .choices(["fast", "slow"]),
                ),
            FixtureSpec::new("alpha", "A fixture.").ready("status"),
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
            r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1, settle = false, animations = true })
	eguidev.fixture("zeta", { mode = "slow" }, { wait = false })
	local frame = eguidev.wait_frames(0)
	local catalog = eguidev.fixtures()
	eguidev.log(catalog[1].name)
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
        assert!(!inner.automation_options().animations);
    }

    #[test]
    fn configure_animations_does_not_leak_across_evaluations() {
        let inner = Arc::new(Inner::new());
        inner.set_automation_options(AutomationOptions {
            keep_alive: true,
            animations: false,
        });
        let runtime = Runtime::ensure_for_inner(&inner);
        let first = run_script_eval_blocking(
            Arc::clone(&inner),
            Arc::clone(&runtime),
            r#"eguidev.configure({ animations = true })
return true"#
                .to_string(),
            1_000,
            "configure-animations-first.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(first.success, "{first:?}");
        assert!(!inner.automation_options().animations);

        let second = run_script_eval_blocking(
            Arc::clone(&inner),
            runtime,
            r#"eguidev.configure({ animations = true })
return true"#
                .to_string(),
            1_000,
            "configure-animations-second.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(second.success, "{second:?}");
        assert!(!inner.automation_options().animations);
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
            r#"local widgets = eguidev.root:widgets({ id_prefix = "status" })
local state = widgets[1]:state()
assert(state ~= nil)
return { count = #widgets, id = widgets[1].id, viewport = state.viewport_id }"#
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
            r#"local viewport = eguidev.root
local widget = eguidev.widget("status")
local state = widget:state()
local parent = widget:parent()
local hits = viewport:widgets_at({ x = 1, y = 1 })
assert(state ~= nil and parent ~= nil)
local parent_state = parent:state()
assert(state.parent_id == "panel")
assert(#state.child_ids == 0)
assert(parent_state ~= nil and parent_state.child_ids[1] == "status")
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
            r#"eguidev.configure({ settle = false })
local button = eguidev.widget("button")
local checkbox = eguidev.widget("checkbox")
local input = eguidev.widget("input")
button:click({ settle = false, click_count = 2 })
button:hover({ settle = false })
button:drag_relative(
    { x = 0.8, y = 0.5 },
    { from = { x = 0.2, y = 0.5 }, settle = false }
)
checkbox:set_value(true, { settle = false })
input:type_text("hello", { settle = false })
input:focus()
local viewport = button:viewport()
assert(viewport ~= nil)
return { viewport = viewport.id }"#
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
            r#"eguidev.configure({ settle = false })
local viewport = eguidev.root
viewport:dismiss_popups()
viewport:key("enter", { settle = false })
viewport:paste("hello", { settle = false })
viewport:input({ type = "pointer_move", position = { x = 1, y = 2 } })
viewport:input({
    type = "pointer_button",
    position = { x = 1, y = 2 },
    button = "primary",
    action = "press",
})
viewport:input({ type = "key", key = "enter", action = "release" })
viewport:input({ type = "text", text = "hello" })
viewport:input({ type = "scroll", delta = { x = 0, y = -10 } })
viewport:resize({ inner_size = { x = 320, y = 240 }, resizable = true })
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
            r##"local viewport = eguidev.root
local widget = eguidev.widget("status")
local widget_issues = widget:layout_issues()
local viewport_issues = viewport:layout_issues()
widget:show_highlight("#ff0000")
widget:clear_highlight()
widget:show_debug_overlay({ mode = "bounds", show_labels = false })
widget:clear_debug_overlay()
viewport:show_highlight(
    { min = { x = 0, y = 0 }, max = { x = 10, y = 10 } },
    "#00ff00"
)
viewport:clear_highlights()
viewport:show_debug_overlay({ mode = "bounds" })
viewport:clear_debug_overlay()
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
            r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 })
local viewport = eguidev.root
local widget = eguidev.widget("status")
local from_widget = widget:wait(function(current)
    if current == nil then return false end
    eguidev.log("widget:" .. (current.label or ""))
    return current.visible and current.label == "Ready"
end, { timeout_ms = 20, poll_interval_ms = 1 })
local viewport_state = viewport:wait(function(current)
    return current ~= nil and current.frame_count >= 0
end, { timeout_ms = 20, poll_interval_ms = 1 })
local visible = widget:wait({ visible = true })
eguidev.widget("missing"):wait(
    { present = false },
    { timeout_ms = 20, poll_interval_ms = 1 }
)
assert(from_widget ~= nil and viewport_state ~= nil and visible ~= nil)
assert(viewport_state.id == viewport.id)
return {
    viewport_label = from_widget.label,
    widget_label = from_widget.label,
    visible = visible.visible,
    frame_count = viewport_state.frame_count,
    viewport_id = viewport_state.id,
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
                "viewport_id": "root",
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
            r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 })
local widget = eguidev.widget("choice")
local current = widget:state()
assert(current ~= nil)
assert(current.value == 2)
local matched = widget:wait(function(state)
    return state ~= nil and state.value == 2
end)
assert(matched ~= nil)
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
            r#"assert(eguidev.args.count == 4)
return eguidev.args.count"#
                .to_string(),
            1_000,
            "args.luau".to_string(),
            ScriptArgs::from([("count".to_string(), ScriptArgValue::Int(4))]),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(4)));
    }

    #[test]
    fn rejected_configure_does_not_poison_luau_defaults() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.configure({ timeout_ms = 30, poll_interval_ms = 1 })
local configured = pcall(function()
    eguidev.configure({ timeout_ms = -1 })
end)
assert(configured == false)
eguidev.wait(function()
    return false
end)
"#
            .to_string(),
            1_000,
            "configure-reject.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(!outcome.success, "{outcome:?}");
        let error = outcome.error.as_ref().expect("timeout error");
        assert_eq!(error.error_type, "timeout");
        assert!(
            outcome.timing.exec_ms < 500,
            "rejected configure must keep the previous timeout: {outcome:?}"
        );
    }

    #[test]
    fn capture_snapshots_are_immutable() {
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
            r#"local cap = eguidev.capture()
local anyCap: any = cap
local frameOk = pcall(function()
    anyCap.frame = 1
end)
local nestedOk = pcall(function()
    anyCap.__widgets[1].id = "mutated"
end)
local diff = cap:diff()
return {
    frame_ok = frameOk,
    nested_ok = nestedOk,
    change_count = #diff.changes,
}
"#
            .to_string(),
            1_000,
            "capture-freeze.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "frame_ok": false,
                "nested_ok": false,
                "change_count": 0,
            }))
        );
    }

    #[test]
    fn wait_rejects_invalid_condition_combinations() {
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
            r#"local function code_of(run: () -> ()): string
    local ok, err = pcall(run)
    assert(ok == false)
    return (err :: Error).code
end
local present = code_of(function()
    eguidev.widget("status"):wait({ present = false, visible = true })
end)
local label = code_of(function()
    eguidev.widget("status"):wait({ label = "" })
end)
local pointer = code_of(function()
    eguidev.widget("status"):wait({ data = { pointer = "not-a-pointer", equals = true } })
end)
local viewport = code_of(function()
    eguidev.root:wait({ present = false, focused = true })
end)
return {
    present = present,
    label = label,
    pointer = pointer,
    viewport = viewport,
}
"#
            .to_string(),
            1_000,
            "wait-validate.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "present": "invalid_argument",
                "label": "invalid_argument",
                "pointer": "invalid_argument",
                "viewport": "invalid_argument",
            }))
        );
    }

    #[test]
    fn click_settle_shares_one_timeout_budget() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("button", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"eguidev.widget("button"):click({ timeout_ms = 200, poll_interval_ms = 1 })
"#
            .to_string(),
            3_000,
            "click-timeout.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(!outcome.success, "{outcome:?}");
        let error = outcome.error.as_ref().expect("timeout error");
        assert_eq!(error.error_type, "timeout");
        assert!(
            !error.message.contains("Script timed out after"),
            "settle must use the call timeout, not the script deadline: {error:?}"
        );
        assert!(
            outcome.timing.exec_ms < 800,
            "click wait plus settle must stay near 200 ms: {outcome:?}"
        );
    }

    #[test]
    fn widget_list_filters_focused_widgets() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        let mut focused = make_entry("focused", 1, WidgetRole::Button);
        focused.focused = true;
        inner.widgets.record_widget(viewport_id, focused);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("other", 2, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"local focused = eguidev.root:widgets({ focused = true })
local selected = eguidev.root:widgets({ selected = true })
return { focused = focused[1].id, focused_count = #focused, selected_count = #selected }
"#
            .to_string(),
            1_000,
            "widget-filter.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "focused": "focused",
                "focused_count": 1,
                "selected_count": 0,
            }))
        );
    }

    #[test]
    fn widget_list_filters_selected_toggle() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        inner.widgets.clear_registry(viewport_id);
        let mut toggle = make_entry("feature", 1, WidgetRole::Toggle);
        toggle.value = Some(WidgetValue::Bool(true));
        inner.widgets.record_widget(viewport_id, toggle);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("other", 2, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"local selected = eguidev.root:widgets({ selected = true })
local state = eguidev.widget("feature"):state()
assert(state ~= nil)
return { selected = selected[1].id, selected_count = #selected, state_selected = state.selected }
"#
            .to_string(),
            1_000,
            "widget-selected-toggle.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "selected": "feature",
                "selected_count": 1,
                "state_selected": true,
            }))
        );
    }

    #[test]
    fn key_target_keeps_scoped_widget_viewport() {
        let inner = Arc::new(Inner::new());
        let secondary = egui::ViewportId::from_hash_of("script.key.target.secondary");
        let secondary_id = viewport_id_to_string(secondary);
        inner.viewports.remember_viewport_id(secondary);
        inner.widgets.clear_registry(secondary);
        let mut field = make_entry("field", 1, WidgetRole::TextEdit);
        field.viewport_id = secondary_id.clone();
        field.focused = true;
        inner.widgets.record_widget(secondary, field);
        inner.widgets.finalize_registry(secondary);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            format!(
                r#"eguidev.configure({{ settle = false }})
local viewport = eguidev.viewport("{secondary_id}")
viewport:key("a", {{ target = viewport:widget("field"), settle = false }})
return true
"#
            ),
            1_000,
            "key-target.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.value, Some(json!(true)));
    }

    #[test]
    fn fixture_catalog_keeps_viewport_scoped_targets() {
        let inner = Arc::new(Inner::new());
        let secondary = egui::ViewportId::from_hash_of("fixture.list.secondary");
        let secondary_id = viewport_id_to_string(secondary);
        inner.fixtures.set_fixtures(vec![
            FixtureSpec::new("multi", "Multi viewport fixture.").ready_in("status", secondary),
        ]);

        let runtime = Runtime::ensure_for_inner(&inner);
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            r#"local catalog = eguidev.fixtures()
local ready = catalog[1].ready[1]
local widget = ready.widget :: any
return { id = ready.widget.id, viewport = widget.__viewport_id }
"#
            .to_string(),
            1_000,
            "fixture-catalog.luau".to_string(),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.value,
            Some(json!({
                "id": "status",
                "viewport": secondary_id,
            }))
        );
    }

    #[test]
    fn nested_sample_arrays_are_promoted_for_luau_arithmetic() {
        let mut vm = Vm::builder()
            .ambient(Ambient::deterministic(0))
            .limits(Limits::unlimited())
            .runtime_capabilities(RuntimeCapabilities::default().enable_runtime_compilation())
            .trusted_host()
            .build()
            .expect("sample vm builds");
        vm.step(|scope| {
            let encoded = json_to_scoped_value(
                scope,
                &json!({
                    "samples": [{
                        "physical": [25, 16],
                        "rgba": [47, 128, 237, 255],
                    }]
                }),
            )?;
            let probe = scope.load_chunk(
                br#"
                return function(d)
                    local sample = d.samples[1]
                    return sample.rgba[1] + 1 == 48 and sample.physical[1] * 2 == 50
                end
                "#,
                b"=sample-arrays",
            )?;
            let probe: Function<'_> = scope.call(probe, ())?;
            let ok: bool = scope.call(probe, (encoded,))?;
            assert!(ok);
            Ok(())
        })
        .expect("sample arrays add and multiply as numbers");
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
            layer_order: 0,
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
