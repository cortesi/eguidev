use std::sync::LazyLock;

use tokio::sync::Mutex as AsyncMutex;

mod kernel;
pub mod library;
mod outcome;
mod parse;
mod runtime;
mod typecheck;
mod types;
mod value;

pub use kernel::run_script_eval_with_modules;
pub use typecheck::{CheckFailure, check_source, warm_checker_baseline};
pub use types::{
    FixtureApplication, ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo,
    ScriptEvalOptions, ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation,
    ScriptModules, ScriptTiming,
};

pub const DEFAULT_SCRIPT_TIMEOUT_MS: u64 = 60_000;
pub(super) const DEFAULT_SCRIPT_MAX_INSTRUCTIONS: u64 = 10_000_000;

pub(super) static SCRIPT_EVAL_LOCK: LazyLock<AsyncMutex<()>> =
    LazyLock::new(|| AsyncMutex::new(()));
