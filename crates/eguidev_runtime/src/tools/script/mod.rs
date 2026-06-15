use std::sync::LazyLock;

use tokio::sync::Mutex as AsyncMutex;

mod eval;
mod outcome;
mod parse;
mod ruau_adapter;
mod runtime;
mod types;
mod value;

pub use eval::run_script_eval;
pub use types::{
    ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo, ScriptEvalOptions,
    ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation, ScriptTiming,
};

pub(super) const DEFAULT_SCRIPT_TIMEOUT_MS: u64 = 60_000;

pub(super) static SCRIPT_EVAL_LOCK: LazyLock<AsyncMutex<()>> =
    LazyLock::new(|| AsyncMutex::new(()));
