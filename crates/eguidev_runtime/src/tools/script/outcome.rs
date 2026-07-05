use super::{
    runtime::ScriptRuntime,
    types::{ScriptErrorInfo, ScriptEvalOutcome, ScriptTiming, ScriptValue},
};

/// Build the common success envelope for script evaluation results.
pub(super) fn build_success_outcome(
    runtime: &ScriptRuntime,
    script_value: ScriptValue,
    timing: ScriptTiming,
) -> ScriptEvalOutcome {
    ScriptEvalOutcome {
        success: true,
        value: script_value.value,
        images: script_value.images,
        logs: runtime.logs(),
        assertions: runtime.assertions(),
        fixtures: runtime.fixture_applications(),
        timing,
        error: None,
        content: script_value.content,
    }
}

/// Build the common error envelope for script evaluation failures.
pub(super) fn build_error_outcome(
    runtime: &ScriptRuntime,
    info: ScriptErrorInfo,
    timing: ScriptTiming,
) -> ScriptEvalOutcome {
    ScriptEvalOutcome {
        success: false,
        value: None,
        images: None,
        logs: runtime.logs(),
        assertions: runtime.assertions(),
        fixtures: runtime.fixture_applications(),
        timing,
        error: Some(info),
        content: Vec::new(),
    }
}
