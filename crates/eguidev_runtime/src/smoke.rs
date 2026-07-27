//! Smoketest suite runner for Luau scripts against a live DevMCP app.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use glob::Pattern;
use serde::Serialize;
use tokio::runtime::Handle;

use crate::{
    DevMcp, EguiDiagnosticBatch, EguiDiagnosticKind, FixtureApplication, ScriptArgs,
    ScriptErrorInfo, ScriptEvalOptions, ScriptEvalOutcome, runtime,
};

const SUITE_RESULT_PATH: &str = "<suite>";

/// Configuration for a smoketest suite run.
#[derive(Debug, Clone, PartialEq)]
pub struct SuiteConfig {
    /// Directory containing `.luau` test scripts.
    pub suite_dir: PathBuf,
    /// Explicit script paths to run. Empty means discover all `.luau` files
    /// under `suite_dir` recursively in lexicographic order.
    pub scripts: Vec<PathBuf>,
    /// Discovery glob filters matched against display paths. Empty means all
    /// discovered scripts. Cannot be combined with explicit `scripts`.
    pub only: Vec<String>,
    /// Wall-clock deadline for the entire suite.
    pub suite_timeout: Duration,
    /// Per-script timeout. `None` uses the script-eval default.
    pub script_timeout: Option<Duration>,
    /// Stop after the first failure.
    pub fail_fast: bool,
    /// Fail scripts that leave egui identity diagnostics undismissed.
    pub fail_on_egui_diagnostics: bool,
    /// Repetition behavior for the selected suite.
    pub run_mode: SuiteRunMode,
    /// Args passed to every script in the suite.
    pub args: ScriptArgs,
}

impl SuiteConfig {
    /// Maximum number of rounds this suite can run.
    pub fn round_limit(&self) -> u32 {
        self.run_mode.round_limit()
    }

    fn stop_on_failure(&self) -> bool {
        self.fail_fast || self.run_mode.stop_on_failure()
    }

    fn stop_after_failure_reason(&self) -> Option<&'static str> {
        self.stop_on_failure()
            .then_some("stopped after earlier smoketest failure")
    }

    /// Apply suite-level egui diagnostic policy to one script outcome.
    pub fn apply_egui_diagnostic_policy(&self, outcome: &mut ScriptEvalOutcome) {
        if !self.fail_on_egui_diagnostics || outcome.egui_diagnostics.is_empty() {
            return;
        }
        if outcome
            .error
            .as_ref()
            .is_some_and(error_has_egui_diagnostic_details)
        {
            return;
        }
        let diagnostic_details =
            serde_json::to_value(&outcome.egui_diagnostics).unwrap_or(serde_json::Value::Null);
        if outcome.success {
            outcome.success = false;
            outcome.error = Some(ScriptErrorInfo {
                error_type: "egui_diagnostics".to_string(),
                message: egui_diagnostic_failure_message(&outcome.egui_diagnostics),
                location: None,
                backtrace: None,
                code: Some("egui_diagnostics".to_string()),
                details: Some(serde_json::json!({
                    "egui_diagnostics": diagnostic_details,
                })),
            });
            return;
        }
        if let Some(error) = outcome.error.as_mut() {
            let mut details = match error.details.take() {
                Some(serde_json::Value::Object(details)) => details,
                Some(original) => {
                    serde_json::Map::from_iter([("original_details".to_string(), original)])
                }
                None => serde_json::Map::new(),
            };
            details.insert("egui_diagnostics".to_string(), diagnostic_details);
            error.details = Some(serde_json::Value::Object(details));
        }
    }
}

/// Repetition behavior for one smoke suite invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteRunMode {
    /// Run the selected scripts this many times. Values below 1 are treated as
    /// one round.
    Repeat(u32),
    /// Repeat until the first failure, stopping after at most this many rounds.
    /// Values below 1 are treated as one round.
    UntilFail(u32),
}

impl SuiteRunMode {
    /// Run the selected scripts once.
    pub const ONCE: Self = Self::Repeat(1);

    /// Return the maximum number of rounds this mode can run.
    pub fn round_limit(self) -> u32 {
        match self {
            Self::Repeat(count) | Self::UntilFail(count) => count.max(1),
        }
    }

    fn stop_on_failure(self) -> bool {
        matches!(self, Self::UntilFail(_))
    }
}

/// Discovered smoke script metadata for list mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuiteScriptInfo {
    /// Forward-slash-normalized display path.
    pub path: String,
    /// Script file size in bytes.
    pub size: u64,
}

/// Outcome for an individual smoketest script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptStatus {
    /// Script completed successfully.
    Pass,
    /// Script failed or the suite hit a setup error.
    Fail,
    /// Script was skipped because the suite timed out or fail-fast triggered.
    Skip,
}

/// Result of a single script execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptResult {
    /// One-based round index for repeated suite runs.
    pub round: u32,
    /// Forward-slash-normalized relative script path, or `<suite>` for suite-level failures.
    pub path: String,
    /// Final script status.
    pub status: ScriptStatus,
    /// Script runtime in milliseconds. Skipped scripts report `0`.
    pub elapsed_ms: u64,
    /// Failure or skip message when present.
    pub message: Option<String>,
    /// Logs emitted by the script.
    pub logs: Vec<String>,
    /// Fixtures applied during the script.
    pub fixtures: Vec<FixtureApplication>,
    /// Undismissed egui diagnostics retained by the evaluation.
    pub egui_diagnostics: EguiDiagnosticBatch,
    /// Verbose failure details from the script runtime.
    pub details: Option<String>,
}

impl ScriptResult {
    #[cfg(test)]
    fn pass(round: u32, path: String, elapsed_ms: u64, logs: Vec<String>) -> Self {
        Self {
            round,
            path,
            status: ScriptStatus::Pass,
            elapsed_ms,
            message: None,
            logs,
            fixtures: Vec::new(),
            egui_diagnostics: EguiDiagnosticBatch::default(),
            details: None,
        }
    }

    fn pass_with_fixtures(
        round: u32,
        path: String,
        elapsed_ms: u64,
        logs: Vec<String>,
        fixtures: Vec<FixtureApplication>,
        egui_diagnostics: EguiDiagnosticBatch,
    ) -> Self {
        Self {
            round,
            path,
            status: ScriptStatus::Pass,
            elapsed_ms,
            message: None,
            logs,
            fixtures,
            egui_diagnostics,
            details: None,
        }
    }

    fn fail(round: u32, path: String, elapsed_ms: u64, message: String, logs: Vec<String>) -> Self {
        Self::fail_with_details(round, path, elapsed_ms, message, logs, None)
    }

    fn fail_with_details(
        round: u32,
        path: String,
        elapsed_ms: u64,
        message: String,
        logs: Vec<String>,
        details: Option<String>,
    ) -> Self {
        Self {
            round,
            path,
            status: ScriptStatus::Fail,
            elapsed_ms,
            message: Some(message),
            logs,
            fixtures: Vec::new(),
            egui_diagnostics: EguiDiagnosticBatch::default(),
            details,
        }
    }

    fn fail_with_outcome(
        round: u32,
        path: String,
        elapsed_ms: u64,
        message: String,
        outcome: ScriptEvalOutcome,
        details: Option<String>,
    ) -> Self {
        let egui_diagnostics = outcome.egui_diagnostics.clone();
        Self {
            round,
            path,
            status: ScriptStatus::Fail,
            elapsed_ms,
            message: Some(message),
            logs: outcome.logs,
            fixtures: outcome.fixtures,
            egui_diagnostics,
            details,
        }
    }

    fn skip(round: u32, path: String, message: String) -> Self {
        Self {
            round,
            path,
            status: ScriptStatus::Skip,
            elapsed_ms: 0,
            message: Some(message),
            logs: Vec::new(),
            fixtures: Vec::new(),
            egui_diagnostics: EguiDiagnosticBatch::default(),
            details: None,
        }
    }
}

/// Runtime summary for one repeated suite round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteRoundResult {
    /// One-based round index.
    pub round: u32,
    /// Round runtime in milliseconds.
    pub elapsed_ms: u64,
}

/// Result of running a full suite.
#[derive(Debug, Clone, PartialEq)]
pub struct SuiteResult {
    /// Per-script results in execution order.
    pub results: Vec<ScriptResult>,
    /// Per-round timings.
    pub rounds: Vec<SuiteRoundResult>,
    /// Maximum number of rounds requested for this suite invocation.
    pub requested_rounds: u32,
    /// Total suite runtime in milliseconds.
    pub elapsed_ms: u64,
}

impl SuiteResult {
    /// Returns `true` when every discovered script passed.
    pub fn success(&self) -> bool {
        self.failed() == 0 && self.skipped() == 0
    }

    /// Count passing scripts.
    pub fn passed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.status == ScriptStatus::Pass)
            .count()
    }

    /// Count failing scripts.
    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.status == ScriptStatus::Fail)
            .count()
    }

    /// Count skipped scripts.
    pub fn skipped(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.status == ScriptStatus::Skip)
            .count()
    }

    /// Render suite results as printable lines.
    pub fn render_lines(&self, verbose: bool) -> Vec<String> {
        let mut lines = Vec::new();
        let show_rounds = self.requested_rounds > 1;
        for script in &self.results {
            if verbose {
                lines.extend(
                    script.logs.iter().map(|log| {
                        format!("LOG: {}", serde_json::to_string(log).unwrap_or_default())
                    }),
                );
                if !script.egui_diagnostics.is_empty() {
                    lines.push(format!(
                        "EGUI: {}",
                        egui_diagnostic_failure_message(&script.egui_diagnostics)
                    ));
                }
            }
            match script.status {
                ScriptStatus::Pass => {
                    lines.push(format!(
                        "[PASS] {}{} ({}ms)",
                        round_prefix(show_rounds, script.round),
                        script.path,
                        script.elapsed_ms
                    ));
                }
                ScriptStatus::Fail => {
                    let message = script
                        .message
                        .as_deref()
                        .unwrap_or("script failed without a message");
                    lines.push(format!(
                        "[FAIL] {}{} ({}ms): {}",
                        round_prefix(show_rounds, script.round),
                        script.path,
                        script.elapsed_ms,
                        message
                    ));
                    if verbose && let Some(details) = &script.details {
                        lines.extend(details.lines().map(|line| format!("DETAIL: {line}")));
                    }
                }
                ScriptStatus::Skip => {
                    lines.push(format!(
                        "[SKIP] {}{}: {}",
                        round_prefix(show_rounds, script.round),
                        script.path,
                        script.message.as_deref().unwrap_or("skipped")
                    ));
                }
            }
        }
        if show_rounds {
            lines.extend(
                self.rounds
                    .iter()
                    .map(|round| format!("[ROUND] {} ({}ms)", round.round, round.elapsed_ms)),
            );
        }
        if verbose {
            lines.push(format!(
                "smoketest summary: {} total, {} passed, {} failed, {} skipped in {}ms",
                self.results.len(),
                self.passed(),
                self.failed(),
                self.skipped(),
                self.elapsed_ms
            ));
        }
        lines
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Input passed to a caller-supplied per-script smoke executor.
pub struct ScriptRunRequest {
    /// Forward-slash-normalized relative script path used for diagnostics.
    pub path: String,
    /// One-based round index for repeated suite runs.
    pub round: u32,
    /// Luau source code loaded from disk.
    pub source: String,
    /// Optional per-script timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Suite-wide args passed to the script.
    pub args: ScriptArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuiteScript {
    display_path: String,
    source_path: PathBuf,
    size: u64,
}

/// Discover the selected smoke scripts without running them.
pub fn discover_suite_scripts(config: &SuiteConfig) -> io::Result<Vec<SuiteScriptInfo>> {
    collect_suite_scripts(config).map(|scripts| {
        scripts
            .into_iter()
            .map(|script| SuiteScriptInfo {
                path: script.display_path,
                size: script.size,
            })
            .collect()
    })
}

/// Run a smoketest suite against a live `DevMcp` instance.
pub fn run_suite(devmcp: &DevMcp, handle: &Handle, config: &SuiteConfig) -> SuiteResult {
    run_suite_with(config, |request| {
        Ok(handle.block_on(runtime::eval_script(
            devmcp,
            &request.source,
            request.timeout_ms,
            ScriptEvalOptions {
                source_name: Some(request.path),
                args: request.args,
            },
        )))
    })
}

/// Run a smoketest suite through a caller-supplied script executor.
pub fn run_suite_with<F>(config: &SuiteConfig, mut execute: F) -> SuiteResult
where
    F: FnMut(ScriptRunRequest) -> Result<ScriptEvalOutcome, String>,
{
    let suite_start = Instant::now();
    let round_limit = config.round_limit();
    let scripts = match collect_suite_scripts(config) {
        Ok(paths) if !paths.is_empty() => paths,
        Ok(_) => {
            return suite_failure_result(
                suite_start.elapsed().as_millis() as u64,
                empty_suite_message(config),
                round_limit,
            );
        }
        Err(error) => {
            return suite_failure_result(
                suite_start.elapsed().as_millis() as u64,
                format!(
                    "failed to discover smoketests under {}: {error}",
                    config.suite_dir.display()
                ),
                round_limit,
            );
        }
    };

    let suite_deadline = suite_start
        .checked_add(config.suite_timeout)
        .unwrap_or_else(Instant::now);
    let mut results = Vec::new();
    let mut rounds = Vec::new();
    let mut stop_suite = false;

    for round in 1..=round_limit {
        let round_start = Instant::now();
        for (index, script) in scripts.iter().enumerate() {
            if Instant::now() >= suite_deadline {
                append_skipped(
                    &mut results,
                    round,
                    &scripts[index..],
                    "suite deadline exceeded before test started",
                );
                stop_suite = true;
                break;
            }

            let relative_display = script.display_path.clone();
            let source_path = &script.source_path;
            let script_start = Instant::now();
            let source = match fs::read_to_string(source_path) {
                Ok(source) => source,
                Err(error) => {
                    let elapsed_ms = script_start.elapsed().as_millis() as u64;
                    let message = format!("failed to read script: {error}");
                    results.push(ScriptResult::fail(
                        round,
                        relative_display,
                        elapsed_ms,
                        message,
                        Vec::new(),
                    ));
                    if let Some(reason) = config.stop_after_failure_reason() {
                        append_skipped(&mut results, round, &scripts[index + 1..], reason);
                        stop_suite = true;
                        break;
                    }
                    continue;
                }
            };

            let outcome = execute(ScriptRunRequest {
                path: relative_display.clone(),
                round,
                source,
                timeout_ms: config.script_timeout.map(duration_to_millis),
                args: config.args.clone(),
            });
            let elapsed_ms = script_start.elapsed().as_millis() as u64;

            let mut outcome = match outcome {
                Ok(outcome) => outcome,
                Err(message) => {
                    results.push(ScriptResult::fail(
                        round,
                        relative_display,
                        elapsed_ms,
                        message,
                        Vec::new(),
                    ));
                    if let Some(reason) = config.stop_after_failure_reason() {
                        append_skipped(&mut results, round, &scripts[index + 1..], reason);
                        stop_suite = true;
                        break;
                    }
                    continue;
                }
            };
            config.apply_egui_diagnostic_policy(&mut outcome);

            if outcome.success {
                results.push(ScriptResult::pass_with_fixtures(
                    round,
                    relative_display,
                    elapsed_ms,
                    outcome.logs,
                    outcome.fixtures,
                    outcome.egui_diagnostics,
                ));
                continue;
            }

            let message = script_failure_summary(&outcome);
            let details = script_failure_details(&outcome);
            results.push(ScriptResult::fail_with_outcome(
                round,
                relative_display,
                elapsed_ms,
                message,
                outcome,
                details,
            ));
            if let Some(reason) = config.stop_after_failure_reason() {
                append_skipped(&mut results, round, &scripts[index + 1..], reason);
                stop_suite = true;
                break;
            }
        }
        rounds.push(SuiteRoundResult {
            round,
            elapsed_ms: round_start.elapsed().as_millis() as u64,
        });
        if stop_suite {
            break;
        }
    }

    SuiteResult {
        results,
        rounds,
        requested_rounds: round_limit,
        elapsed_ms: suite_start.elapsed().as_millis() as u64,
    }
}

fn suite_failure_result(elapsed_ms: u64, message: String, requested_rounds: u32) -> SuiteResult {
    SuiteResult {
        results: vec![ScriptResult::fail(
            1,
            SUITE_RESULT_PATH.to_string(),
            elapsed_ms,
            message,
            Vec::new(),
        )],
        rounds: Vec::new(),
        requested_rounds,
        elapsed_ms,
    }
}

fn empty_suite_message(config: &SuiteConfig) -> String {
    if config.only.is_empty() {
        return format!("no smoketests found under {}", config.suite_dir.display());
    }
    format!(
        "no smoketests under {} matched --only {}",
        config.suite_dir.display(),
        config
            .only
            .iter()
            .map(|filter| format!("{filter:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn append_skipped(
    results: &mut Vec<ScriptResult>,
    round: u32,
    paths: &[SuiteScript],
    reason: &str,
) {
    results.extend(
        paths
            .iter()
            .map(|path| ScriptResult::skip(round, path.display_path.clone(), reason.into())),
    );
}

fn duration_to_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn collect_suite_scripts(config: &SuiteConfig) -> io::Result<Vec<SuiteScript>> {
    if !config.scripts.is_empty() && !config.only.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "`--only` cannot be combined with explicit smoke script paths",
        ));
    }

    if config.scripts.is_empty() {
        let scripts = collect_suite_paths(&config.suite_dir)?
            .into_iter()
            .map(|path| {
                let source_path = config.suite_dir.join(&path);
                let size = fs::metadata(&source_path).map(|metadata| metadata.len())?;
                Ok(SuiteScript {
                    display_path: normalize_path(&path),
                    source_path,
                    size,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        return filter_suite_scripts(scripts, &config.only);
    }

    config
        .scripts
        .iter()
        .map(|path| {
            let metadata = fs::metadata(path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("failed to access smoketest {}: {error}", path.display()),
                )
            })?;
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("smoketest path is not a file: {}", path.display()),
                ));
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("luau") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("smoketest path must end in .luau: {}", path.display()),
                ));
            }
            Ok(SuiteScript {
                display_path: normalize_path(path),
                source_path: path.clone(),
                size: metadata.len(),
            })
        })
        .collect()
}

fn filter_suite_scripts(
    scripts: Vec<SuiteScript>,
    filters: &[String],
) -> io::Result<Vec<SuiteScript>> {
    if filters.is_empty() {
        return Ok(scripts);
    }
    let patterns = filters
        .iter()
        .map(|filter| {
            Pattern::new(filter).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid --only glob {filter:?}: {error}"),
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    // Repeating `--only` selects more scripts. A script runs when it matches
    // any pattern, which is what a repeatable selector reads as; narrow with
    // one more specific glob instead.
    Ok(scripts
        .into_iter()
        .filter(|script| {
            patterns
                .iter()
                .any(|pattern| pattern.matches(&script.display_path))
        })
        .collect())
}

fn collect_suite_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_suite_paths_recursive(root, root, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn collect_suite_paths_recursive(
    root: &Path,
    current: &Path,
    paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_suite_paths_recursive(root, &path, paths)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("luau") {
            continue;
        }
        let relative = path.strip_prefix(root).map_err(|error| {
            io::Error::other(format!("failed to strip smoketest prefix: {error}"))
        })?;
        paths.push(relative.to_path_buf());
    }

    Ok(())
}

fn script_failure_summary(outcome: &ScriptEvalOutcome) -> String {
    let Some(error) = &outcome.error else {
        return "script failed without an error payload".to_string();
    };
    let location = error.location.as_ref().map(|location| {
        let column = location.column.unwrap_or(1);
        format!(":{}:{}", location.line, column)
    });
    let summary = match location {
        Some(location) => format!("{} at{}", error.message, location),
        None => error.message.clone(),
    };
    if outcome.egui_diagnostics.is_empty() || error.error_type == "egui_diagnostics" {
        summary
    } else {
        format!(
            "{summary}; {}",
            egui_diagnostic_failure_message(&outcome.egui_diagnostics)
        )
    }
}

fn script_failure_details(outcome: &ScriptEvalOutcome) -> Option<String> {
    let details = outcome.error.as_ref()?.details.as_ref()?;
    serde_json::to_string_pretty(details)
        .ok()
        .or_else(|| Some(details.to_string()))
}

fn error_has_egui_diagnostic_details(error: &ScriptErrorInfo) -> bool {
    error.error_type == "egui_diagnostics"
        || error
            .details
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .is_some_and(|details| details.contains_key("egui_diagnostics"))
}

fn egui_diagnostic_failure_message(batch: &EguiDiagnosticBatch) -> String {
    let mut diagnostics = batch
        .entries
        .iter()
        .map(|entry| {
            let kind = match entry.kind {
                EguiDiagnosticKind::IdClash => "id_clash",
                EguiDiagnosticKind::RectChangedId => "rect_changed_id",
            };
            let rect = entry
                .rect
                .as_ref()
                .map_or_else(|| "none".to_string(), |rect| format!("{rect:?}"));
            format!(
                "{kind} viewport={} frame={} rect={rect} message={:?}",
                entry.viewport_id, entry.frame, entry.message
            )
        })
        .collect::<Vec<_>>();
    if batch.dropped > 0 {
        diagnostics.push(format!("dropped={}", batch.dropped));
    }
    format!("undismissed egui diagnostics: {}", diagnostics.join("; "))
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn round_prefix(show_rounds: bool, round: u32) -> String {
    if show_rounds {
        format!("round {round} ")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        hint::spin_loop,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use tokio::runtime::Builder;

    use super::{
        ScriptRunRequest, ScriptStatus, SuiteConfig, SuiteResult, SuiteRunMode,
        collect_suite_paths, collect_suite_scripts, discover_suite_scripts, normalize_path,
        run_suite, run_suite_with,
    };
    use crate::{
        DevMcp, EguiDiagnostic, EguiDiagnosticBatch, EguiDiagnosticKind, EguiDiagnosticSeverity,
        ScriptArgValue, ScriptArgs, ScriptEvalOutcome, runtime,
        types::{Pos2, Rect},
    };

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_root(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        PathBuf::from("tmp")
            .join("smoke_tests")
            .join(format!("{name}_{id}"))
    }

    fn suite_config(suite_dir: PathBuf) -> SuiteConfig {
        SuiteConfig {
            suite_dir,
            scripts: Vec::new(),
            only: Vec::new(),
            suite_timeout: Duration::from_secs(10),
            script_timeout: None,
            fail_fast: false,
            fail_on_egui_diagnostics: true,
            run_mode: SuiteRunMode::ONCE,
            args: ScriptArgs::default(),
        }
    }

    fn success_outcome() -> ScriptEvalOutcome {
        serde_json::from_value(serde_json::json!({
            "success": true,
            "logs": [],
            "assertions": [],
            "timing": {
                "compile_ms": 0,
                "exec_ms": 0,
                "total_ms": 0
            }
        }))
        .expect("deserialize success outcome")
    }

    fn failure_outcome(message: &str) -> ScriptEvalOutcome {
        serde_json::from_value(serde_json::json!({
            "success": false,
            "logs": ["boom"],
            "assertions": [],
            "timing": {
                "compile_ms": 0,
                "exec_ms": 0,
                "total_ms": 0
            },
            "error": {
                "type": "runtime",
                "message": message
            }
        }))
        .expect("deserialize failure outcome")
    }

    fn diagnostic_batch(dropped: u64) -> EguiDiagnosticBatch {
        EguiDiagnosticBatch {
            entries: vec![EguiDiagnostic {
                kind: EguiDiagnosticKind::RectChangedId,
                severity: EguiDiagnosticSeverity::Warning,
                message: "rectangle changed identity".to_string(),
                viewport_id: "root".to_string(),
                frame: 12,
                rect: Some(Rect {
                    min: Pos2 { x: 1.0, y: 2.0 },
                    max: Pos2 { x: 3.0, y: 4.0 },
                }),
            }],
            dropped,
        }
    }

    #[test]
    fn diagnostic_policy_fails_success_and_lists_exact_evidence() {
        let config = suite_config(PathBuf::from("unused"));
        let mut outcome = success_outcome();
        outcome.egui_diagnostics = diagnostic_batch(2);

        config.apply_egui_diagnostic_policy(&mut outcome);

        assert!(!outcome.success);
        let error = outcome.error.expect("diagnostic error");
        assert_eq!(error.code.as_deref(), Some("egui_diagnostics"));
        for evidence in [
            "rect_changed_id",
            "viewport=root",
            "frame=12",
            "rect=Rect",
            "rectangle changed identity",
            "dropped=2",
        ] {
            assert!(error.message.contains(evidence), "{error:?}");
        }
    }

    #[test]
    fn diagnostic_policy_opt_out_preserves_success_and_batch() {
        let mut config = suite_config(PathBuf::from("unused"));
        config.fail_on_egui_diagnostics = false;
        let mut outcome = success_outcome();
        outcome.egui_diagnostics = diagnostic_batch(0);

        config.apply_egui_diagnostic_policy(&mut outcome);

        assert!(outcome.success);
        assert_eq!(outcome.egui_diagnostics.entries.len(), 1);
    }

    #[test]
    fn diagnostic_policy_preserves_original_failure_and_adds_details() {
        let config = suite_config(PathBuf::from("unused"));
        let mut outcome = failure_outcome("original failure");
        outcome.egui_diagnostics = diagnostic_batch(0);

        config.apply_egui_diagnostic_policy(&mut outcome);

        let error = outcome.error.expect("original error");
        assert_eq!(error.message, "original failure");
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|details| details.get("egui_diagnostics"))
                .and_then(|batch| batch.get("entries"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn suite_results_retain_diagnostics_when_policy_is_disabled() {
        let root = test_root("suite_results_retain_diagnostics_when_policy_is_disabled");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite");
        fs::write(suite_dir.join("10_probe.luau"), "return true").expect("write script");
        let mut config = suite_config(suite_dir);
        config.fail_on_egui_diagnostics = false;

        let result = run_suite_with(&config, |_request| {
            let mut outcome = success_outcome();
            outcome.egui_diagnostics = diagnostic_batch(0);
            Ok(outcome)
        });

        assert!(result.success());
        assert_eq!(result.results[0].egui_diagnostics.entries.len(), 1);
        assert!(
            result
                .render_lines(true)
                .iter()
                .any(|line| line.starts_with("EGUI: undismissed egui diagnostics:"))
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn collect_suite_paths_sorts_recursively() {
        let root = test_root("collect_suite_paths_sorts_recursively");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(root.join("nested")).expect("create suite dir");
        fs::write(root.join("20_second.luau"), "return true").expect("write second");
        fs::write(root.join("nested").join("10_first.luau"), "return true").expect("write first");

        let all = collect_suite_paths(&root).expect("all paths");
        assert_eq!(
            all,
            vec![
                PathBuf::from("20_second.luau"),
                PathBuf::from("nested/10_first.luau"),
            ]
        );

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn collect_suite_scripts_uses_explicit_paths_in_given_order() {
        let root = test_root("collect_suite_scripts_uses_explicit_paths_in_given_order");
        drop(fs::remove_dir_all(&root));
        let suite_dir = root.join("suite");
        let external_dir = root.join("external");
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::create_dir_all(&external_dir).expect("create external dir");
        let suite_script = suite_dir.join("20_suite.luau");
        let external_script = external_dir.join("10_external.luau");
        fs::write(&suite_script, "return true").expect("write suite script");
        fs::write(&external_script, "return true").expect("write external script");

        let mut config = suite_config(suite_dir);
        config.scripts = vec![external_script.clone(), suite_script.clone()];
        let scripts = collect_suite_scripts(&config).expect("collect scripts");
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts[0].display_path, normalize_path(&external_script));
        assert_eq!(scripts[0].source_path, external_script);
        assert_eq!(scripts[1].display_path, normalize_path(&suite_script));
        assert_eq!(scripts[1].source_path, suite_script);

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn discover_suite_scripts_lists_sizes_and_unions_only_filters() {
        let root = test_root("discover_suite_scripts_lists_sizes_and_unions_only_filters");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(suite_dir.join("nested")).expect("create suite dir");
        fs::write(suite_dir.join("10_bootstrap.luau"), "return true").expect("write first");
        fs::write(suite_dir.join("20_layout.luau"), "return 12345").expect("write second");
        fs::write(
            suite_dir.join("nested").join("30_layout.luau"),
            "return false",
        )
        .expect("write nested");

        let mut config = suite_config(suite_dir);
        config.only = vec!["10_bootstrap.luau".to_string(), "nested/*".to_string()];
        let scripts = discover_suite_scripts(&config).expect("discover scripts");

        let selected = scripts
            .iter()
            .map(|script| script.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(selected, vec!["10_bootstrap.luau", "nested/30_layout.luau"]);
        assert_eq!(scripts[1].size, "return false".len() as u64);

        // One glob still narrows.
        config.only = vec!["*layout.luau".to_string()];
        let narrowed = discover_suite_scripts(&config).expect("discover scripts");
        assert_eq!(narrowed.len(), 2);

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn collect_suite_scripts_rejects_only_with_explicit_paths() {
        let root = test_root("collect_suite_scripts_rejects_only_with_explicit_paths");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        let script = suite_dir.join("10_bootstrap.luau");
        fs::write(&script, "return true").expect("write script");

        let mut config = suite_config(suite_dir);
        config.scripts = vec![script];
        config.only = vec!["*.luau".to_string()];
        let error = collect_suite_scripts(&config).expect_err("conflict should fail");

        assert!(error.to_string().contains("cannot be combined"));

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_with_reports_empty_only_selection() {
        let root = test_root("run_suite_with_reports_empty_only_selection");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(suite_dir.join("10_bootstrap.luau"), "return true").expect("write script");

        let result = run_suite_with(
            &{
                let mut config = suite_config(suite_dir);
                config.only = vec!["*missing*".to_string()];
                config
            },
            |_request| Ok(success_outcome()),
        );

        assert_eq!(result.failed(), 1);
        assert!(
            result.results[0]
                .message
                .as_deref()
                .expect("failure message")
                .contains("matched --only \"*missing*\"")
        );

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_propagates_args_and_fail_fast() {
        let root = test_root("run_suite_propagates_args_and_fail_fast");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(
            suite_dir.join("10_args.luau"),
            "assert(args.name == \"Sky\")\nassert(args.count == 4)\nreturn true",
        )
        .expect("write args script");
        fs::write(suite_dir.join("20_fail.luau"), "assert(false, \"boom\")").expect("write fail");
        fs::write(suite_dir.join("30_skip.luau"), "return true").expect("write skip");

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let devmcp = runtime::attach_for_tests(DevMcp::new());
        let result = run_suite(&devmcp, runtime.handle(), &{
            let mut config = suite_config(suite_dir);
            config.fail_fast = true;
            config.args = ScriptArgs::from([
                (
                    "name".to_string(),
                    ScriptArgValue::String("Sky".to_string()),
                ),
                ("count".to_string(), ScriptArgValue::Int(4)),
            ]);
            config
        });

        assert_eq!(result.passed(), 1);
        assert_eq!(result.failed(), 1);
        assert_eq!(result.skipped(), 1);
        assert_eq!(result.results[0].status, ScriptStatus::Pass);
        assert_eq!(result.results[1].status, ScriptStatus::Fail);
        assert_eq!(result.results[2].status, ScriptStatus::Skip);

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_with_callback_propagates_args_and_fail_fast() {
        let root = test_root("run_suite_with_callback_propagates_args_and_fail_fast");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(suite_dir.join("10_args.luau"), "return true").expect("write args script");
        fs::write(suite_dir.join("20_fail.luau"), "return true").expect("write fail");
        fs::write(suite_dir.join("30_skip.luau"), "return true").expect("write skip");

        let result = run_suite_with(
            &{
                let mut config = suite_config(suite_dir);
                config.script_timeout = Some(Duration::from_secs(7));
                config.fail_fast = true;
                config.args = ScriptArgs::from([
                    (
                        "name".to_string(),
                        ScriptArgValue::String("Sky".to_string()),
                    ),
                    ("count".to_string(), ScriptArgValue::Int(4)),
                ]);
                config
            },
            |request: ScriptRunRequest| {
                assert_eq!(request.timeout_ms, Some(7_000));
                assert_eq!(
                    request.args.get("name"),
                    Some(&ScriptArgValue::String("Sky".to_string()))
                );
                assert_eq!(request.args.get("count"), Some(&ScriptArgValue::Int(4)));
                if request.path == "20_fail.luau" {
                    return Ok(failure_outcome("boom"));
                }
                Ok(success_outcome())
            },
        );

        assert_eq!(result.passed(), 1);
        assert_eq!(result.failed(), 1);
        assert_eq!(result.skipped(), 1);

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_with_measures_elapsed_after_execution() {
        let root = test_root("run_suite_with_measures_elapsed_after_execution");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(suite_dir.join("10_slow.luau"), "return true").expect("write script");

        let result = run_suite_with(
            &{
                let mut config = suite_config(suite_dir);
                config.fail_fast = true;
                config
            },
            |_request: ScriptRunRequest| {
                let start = Instant::now();
                while start.elapsed() < Duration::from_millis(5) {
                    spin_loop();
                }
                Ok(success_outcome())
            },
        );

        assert_eq!(result.passed(), 1);
        assert!(result.results[0].elapsed_ms >= 1);

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_with_repeats_selected_set() {
        let root = test_root("run_suite_with_repeats_selected_set");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(suite_dir.join("10_first.luau"), "return true").expect("write first");
        fs::write(suite_dir.join("20_second.luau"), "return true").expect("write second");

        let mut seen = Vec::new();
        let result = run_suite_with(
            &{
                let mut config = suite_config(suite_dir);
                config.run_mode = SuiteRunMode::Repeat(3);
                config
            },
            |request: ScriptRunRequest| {
                seen.push((request.round, request.path));
                Ok(success_outcome())
            },
        );

        assert_eq!(result.passed(), 6);
        assert_eq!(result.rounds.len(), 3);
        assert_eq!(
            seen,
            vec![
                (1, "10_first.luau".to_string()),
                (1, "20_second.luau".to_string()),
                (2, "10_first.luau".to_string()),
                (2, "20_second.luau".to_string()),
                (3, "10_first.luau".to_string()),
                (3, "20_second.luau".to_string()),
            ]
        );
        assert!(
            result
                .render_lines(false)
                .iter()
                .any(|line| line.starts_with("[PASS] round 2 10_first.luau ("))
        );

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_with_until_fail_stops_on_first_failure() {
        let root = test_root("run_suite_with_until_fail_stops_on_first_failure");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(suite_dir.join("10_first.luau"), "return true").expect("write first");
        fs::write(suite_dir.join("20_second.luau"), "return true").expect("write second");

        let mut seen = Vec::new();
        let result = run_suite_with(
            &{
                let mut config = suite_config(suite_dir);
                config.run_mode = SuiteRunMode::UntilFail(5);
                config
            },
            |request: ScriptRunRequest| {
                seen.push((request.round, request.path.clone()));
                if request.round == 2 && request.path == "10_first.luau" {
                    return Ok(failure_outcome("boom"));
                }
                Ok(success_outcome())
            },
        );

        assert_eq!(result.passed(), 2);
        assert_eq!(result.failed(), 1);
        assert_eq!(result.skipped(), 1);
        assert_eq!(result.rounds.len(), 2);
        assert_eq!(
            result
                .results
                .last()
                .expect("skip result")
                .message
                .as_deref(),
            Some("stopped after earlier smoketest failure")
        );
        assert_eq!(
            seen,
            vec![
                (1, "10_first.luau".to_string()),
                (1, "20_second.luau".to_string()),
                (2, "10_first.luau".to_string()),
            ]
        );

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn run_suite_with_until_fail_first_round_renders_round_prefix() {
        let root = test_root("run_suite_with_until_fail_first_round_renders_round_prefix");
        let suite_dir = root.join("suite");
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&suite_dir).expect("create suite dir");
        fs::write(suite_dir.join("10_fail.luau"), "return true").expect("write script");

        let result = run_suite_with(
            &{
                let mut config = suite_config(suite_dir);
                config.run_mode = SuiteRunMode::UntilFail(5);
                config
            },
            |_request| Ok(failure_outcome("boom")),
        );

        assert_eq!(result.failed(), 1);
        assert!(
            result
                .render_lines(false)
                .iter()
                .any(|line| line.starts_with("[FAIL] round 1 10_fail.luau ("))
        );

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn render_lines_emits_summary_and_logs_in_verbose_mode() {
        let result = SuiteResult {
            results: vec![
                super::ScriptResult::pass(
                    1,
                    "10_pass.luau".to_string(),
                    12,
                    vec!["hello".to_string()],
                ),
                super::ScriptResult::fail_with_details(
                    1,
                    "20_fail.luau".to_string(),
                    18,
                    "boom".to_string(),
                    Vec::new(),
                    Some("{\n  \"kind\": \"widget\"\n}".to_string()),
                ),
            ],
            rounds: Vec::new(),
            requested_rounds: 1,
            elapsed_ms: 30,
        };

        let lines = result.render_lines(true);
        assert!(lines.iter().any(|line| line == "LOG: \"hello\""));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("[FAIL] 20_fail.luau (18ms): boom"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "DETAIL:   \"kind\": \"widget\"")
        );
        assert!(
            lines
                .last()
                .expect("summary line")
                .contains("smoketest summary: 2 total, 1 passed, 1 failed, 0 skipped in 30ms")
        );
    }
}
