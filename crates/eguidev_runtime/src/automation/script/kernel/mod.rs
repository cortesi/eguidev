#![allow(clippy::missing_docs_in_private_items, clippy::result_large_err)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use ruau::{
    bytecode::{BytecodeChunk, CompileError, CompileErrorKind, CompileOptions, encode_chunk},
    declaration::DeclarationSource,
    source::{InMemorySource, ModuleId, SourceMetadata, SourceProvider},
    vm::{
        Ambient, AsyncHostContext, AsyncHostFunction, CallOptions, Deadline, FromLua, FromLuaMulti,
        HostReturn, IntoLuaMulti, Limits, LoadedModule, MarshaledScriptError, ModuleBinding,
        MultiValue, NativeModule, OwnedValue, RuntimeCapabilities, RuntimeCompileContext,
        RuntimeCompiler, RuntimeError, RuntimeErrorKind, Scope, ScopedValue, ScriptErrorField,
        SourceLocation, StashedClosure, StashedValue, TracebackFrame, ValueSnapshot, Vm,
        async_host_fn,
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
        ScriptErrorInfo, ScriptEvalOptions, ScriptEvalOutcome, ScriptLocation, ScriptModules,
        ScriptPosition, ScriptResult, ScriptTiming,
    },
    value::{script_args_to_json, script_return_value_from_json_values, script_value_from_json},
};
use crate::{
    registry::Inner,
    runtime::Runtime,
    script::{CheckFailure, check_source, library, typecheck::check_source_graph},
    types::WidgetRef,
};

mod args;
mod errors;
mod hosts;
mod module;

use args::*;
use errors::*;
use hosts::*;
use module::*;

const EGUIDEV_SEED: u64 = 0x00e9_d1de;

pub async fn run_script_eval_with_modules(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    options: ScriptEvalOptions,
) -> ScriptEvalOutcome {
    let _guard = super::SCRIPT_EVAL_LOCK.lock().await;
    match spawn_blocking(move || {
        run_script_eval_blocking_with_modules(inner, runtime, script, timeout_ms, options)
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => script_eval_task_error(&error),
    }
}

#[cfg(test)]
fn run_script_eval_blocking(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    source_name: String,
    args: super::types::ScriptArgs,
) -> ScriptEvalOutcome {
    run_script_eval_blocking_with_modules(
        inner,
        runtime,
        script,
        timeout_ms,
        ScriptEvalOptions {
            source_name: Some(source_name),
            args,
            ..ScriptEvalOptions::default()
        },
    )
}

fn run_script_eval_blocking_with_modules(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    options: ScriptEvalOptions,
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
        run_script_eval_local(inner, runtime, script, timeout_ms, options),
    )
}

async fn run_script_eval_local(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    options: ScriptEvalOptions,
) -> ScriptEvalOutcome {
    let ScriptEvalOptions {
        source_name,
        max_instructions,
        args,
        modules,
    } = options;
    let source_name = source_name.unwrap_or_else(|| "script.luau".to_string());
    let start = Instant::now();
    let compile_start = Instant::now();
    let module_source = match script_module_source(&modules) {
        Ok(source) => source,
        Err(error) => {
            let mut outcome = ScriptEvalOutcome::error_only(error);
            outcome.timing = timing(start, compile_start.elapsed(), Duration::ZERO);
            return outcome;
        }
    };
    let check = module_source.as_ref().map_or_else(
        || check_source(&source_name, &script),
        |source| {
            let source: Arc<dyn SourceProvider> = source.clone();
            check_source_graph(&source_name, &script, source)
        },
    );
    if let Err(error) = check {
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
        args: script_args_to_json(&args),
        runtime: Arc::clone(&script_runtime),
        declaration: library::DECLARATION.to_string(),
    }
    .build();
    let mut vm_builder = Vm::builder()
        .ambient(Ambient::production(EGUIDEV_SEED))
        .limits(base_limits(max_instructions))
        .runtime_capabilities(runtime_capabilities.clone())
        .module(module)
        .trusted_host();
    if let Some(source) = module_source.as_ref() {
        let source: Arc<dyn SourceProvider> = source.clone();
        vm_builder = vm_builder.module_source(source);
    }
    let mut vm = match vm_builder.build() {
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
        module_source
            .as_ref()
            .map(|_| ModuleId::canonicalized(&source_name)),
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
    let mut call_options =
        CallOptions::new().limits(invocation_limits(start, timeout_ms, max_instructions));
    if module_source.is_some() {
        call_options = call_options.runtime_compiler(Arc::new(ScriptModuleCompiler {
            runtime_capabilities,
        }));
    }
    let outcome = vm.exec_async(&module, call_options).await;
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

fn load(
    vm: &mut Vm,
    runtime_capabilities: &RuntimeCapabilities,
    chunk_name: &[u8],
    source: &[u8],
    source_name: &str,
    module_id: Option<ModuleId>,
) -> Result<LoadedModule, ScriptErrorInfo> {
    let chunk = runtime_capabilities
        .compile_source(source, &CompileOptions::new())
        .map_err(|error| compile_error_info(&error, source_name))?;
    match module_id {
        Some(module_id) => vm.load_named_module(&chunk, module_id, chunk_name),
        None => vm.load_named(&chunk, chunk_name),
    }
    .map_err(|error| runtime_error(format!("failed to load Ruau chunk: {error}")))
}

fn script_module_source(
    modules: &ScriptModules,
) -> Result<Option<Arc<InMemorySource>>, ScriptErrorInfo> {
    if modules.is_empty() {
        return Ok(None);
    }
    let mut source = InMemorySource::new();
    let mut names = BTreeMap::new();
    for (name, contents) in modules {
        let id = ModuleId::canonicalized(name);
        if id.as_bytes().is_empty() {
            return Err(type_error(
                "script module name must not be empty".to_string(),
            ));
        }
        if let Some(previous) = names.insert(id.clone(), name) {
            return Err(type_error(format!(
                "script module names {previous:?} and {name:?} resolve to the same module"
            )));
        }
        source.insert_with_metadata(id, contents, SourceMetadata::new(name.clone()));
    }
    Ok(Some(Arc::new(source)))
}

struct ScriptModuleCompiler {
    runtime_capabilities: RuntimeCapabilities,
}

impl RuntimeCompiler for ScriptModuleCompiler {
    fn compile(
        &self,
        source: &[u8],
        context: RuntimeCompileContext,
    ) -> Result<BytecodeChunk, Vec<u8>> {
        context.check_cancelled()?;
        enforce_runtime_compile_limit(
            "source byte",
            source.len(),
            context.limits.max_source_bytes,
        )?;
        let chunk = self
            .runtime_capabilities
            .compile_source(source, &CompileOptions::new())
            .map_err(|error| error.to_string().into_bytes())?;
        context.check_cancelled()?;
        let instructions = match &chunk {
            BytecodeChunk::Valid { protos, .. } => protos
                .iter()
                .flat_map(|proto| &proto.code)
                .map(|instruction| instruction.word_len() as usize)
                .sum(),
            BytecodeChunk::Error { .. } => 0,
        };
        enforce_runtime_compile_limit(
            "compiled instruction",
            instructions,
            context.limits.max_compiled_instructions,
        )?;
        let encoded = encode_chunk(&chunk).map_err(|error| error.to_string().into_bytes())?;
        enforce_runtime_compile_limit(
            "compiled bytecode byte",
            encoded.len(),
            context.limits.max_compiled_bytecode_bytes,
        )?;
        Ok(chunk)
    }
}

fn enforce_runtime_compile_limit(label: &str, used: usize, cap: usize) -> Result<(), Vec<u8>> {
    if used <= cap {
        return Ok(());
    }
    Err(format!("runtime compilation exceeded {label} limit ({used} > {cap})").into_bytes())
}

fn timing(start: Instant, compile_elapsed: Duration, exec_elapsed: Duration) -> ScriptTiming {
    ScriptTiming {
        compile_ms: compile_elapsed.as_millis() as u64,
        exec_ms: exec_elapsed.as_millis() as u64,
        total_ms: start.elapsed().as_millis() as u64,
    }
}

fn base_limits(max_instructions: Option<u64>) -> Limits {
    Limits {
        gas: Some(max_instructions.unwrap_or(super::DEFAULT_SCRIPT_MAX_INSTRUCTIONS)),
        max_memory_bytes: Some(16 * 1024 * 1024),
        max_native_depth: Some(16),
        quantum: Some(1_000),
        ..Limits::unlimited()
    }
}

fn invocation_limits(
    started_at: Instant,
    timeout_ms: u64,
    max_instructions: Option<u64>,
) -> Limits {
    Limits {
        deadline: started_at
            .checked_add(Duration::from_millis(timeout_ms))
            .map(Deadline::Wall),
        ..base_limits(max_instructions)
    }
}

#[cfg(test)]
mod tests;
