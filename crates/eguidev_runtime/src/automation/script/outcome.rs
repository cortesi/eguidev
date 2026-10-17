use super::{
    runtime::ScriptRuntime,
    types::{ScriptErrorInfo, ScriptEvalOutcome, ScriptTiming, ScriptValue},
};
use crate::EguiDiagnosticBatch;

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
        egui_diagnostics: EguiDiagnosticBatch::default(),
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
        egui_diagnostics: EguiDiagnosticBatch::default(),
        content: Vec::new(),
    }
}

/// Complete the diagnostic barrier and attach evaluation-scoped diagnostics.
pub(super) async fn finalize_outcome(
    runtime: &ScriptRuntime,
    mut outcome: ScriptEvalOutcome,
) -> ScriptEvalOutcome {
    runtime.restore_automation_options();
    let (batch, barrier_error) = runtime.finish_egui_diagnostics().await;
    outcome.egui_diagnostics = batch;
    outcome.timing.total_ms = runtime.elapsed_ms();
    let Some(barrier_error) = barrier_error else {
        return outcome;
    };
    if outcome.success {
        outcome.success = false;
        outcome.error = Some(barrier_error);
        return outcome;
    }

    let barrier_details = serde_json::to_value(&barrier_error).unwrap_or(serde_json::Value::Null);
    if let Some(error) = outcome.error.as_mut() {
        let mut details = match error.details.take() {
            Some(serde_json::Value::Object(details)) => details,
            Some(original) => {
                serde_json::Map::from_iter([("original_details".to_string(), original)])
            }
            None => serde_json::Map::new(),
        };
        details.insert("diagnostic_barrier_error".to_string(), barrier_details);
        error.details = Some(serde_json::Value::Object(details));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{automation::script::types::ScriptValue, registry::Inner, runtime::Runtime};

    fn runtime_with_expired_barrier() -> ScriptRuntime {
        let inner = Arc::new(Inner::new());
        inner.remember_context(egui::ViewportId::ROOT, &egui::Context::default());
        let runtime = Runtime::ensure_for_inner_with_diagnostic_barriers(&inner);
        let script = ScriptRuntime::new(inner, runtime, "barrier.luau".to_string(), 0);
        script.record_targeted_viewport("root");
        script
    }

    #[tokio::test]
    async fn barrier_timeout_fails_an_otherwise_successful_outcome() {
        let runtime = runtime_with_expired_barrier();
        let outcome = build_success_outcome(&runtime, ScriptValue::default(), ScriptTiming::zero());

        let outcome = finalize_outcome(&runtime, outcome).await;

        assert!(!outcome.success);
        let error = outcome.error.expect("barrier error");
        assert_eq!(error.code.as_deref(), Some("diagnostic_barrier_timeout"));
    }

    #[tokio::test]
    async fn original_script_error_precedes_barrier_timeout() {
        let runtime = runtime_with_expired_barrier();
        let original = ScriptErrorInfo {
            error_type: "runtime".to_string(),
            message: "original failure".to_string(),
            location: None,
            backtrace: None,
            code: Some("original".to_string()),
            details: None,
        };
        let outcome = build_error_outcome(&runtime, original, ScriptTiming::zero());

        let outcome = finalize_outcome(&runtime, outcome).await;

        let error = outcome.error.expect("original error");
        assert_eq!(error.message, "original failure");
        assert_eq!(error.code.as_deref(), Some("original"));
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|details| details.get("diagnostic_barrier_error"))
                .and_then(|details| details.get("code"))
                .and_then(serde_json::Value::as_str),
            Some("diagnostic_barrier_timeout")
        );
    }
}
