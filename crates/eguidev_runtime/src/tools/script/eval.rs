use std::sync::Arc;

use super::types::{ScriptArgs, ScriptEvalOutcome};
use crate::{registry::Inner, runtime::Runtime};

/// Evaluate a Luau script against the DevMCP runtime and return the structured outcome.
pub async fn run_script_eval(
    inner: Arc<Inner>,
    runtime: Arc<Runtime>,
    script: String,
    timeout_ms: u64,
    source_name: String,
    args: ScriptArgs,
) -> ScriptEvalOutcome {
    super::oxau_adapter::run_script_eval(inner, runtime, script, timeout_ms, source_name, args)
        .await
}
