//! Diagnostic provider registry for app-owned automation state.

use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    },
    time::Duration,
};

use egui::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::registry::lock;

/// Result returned by an app diagnostic provider.
pub type DiagnosticResult = Result<Value, DiagnosticError>;

/// Structured failure returned by an app diagnostic provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Optional machine-readable error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl DiagnosticError {
    /// Create a diagnostic error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Attach structured error details.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn provider_panic(name: &str, panic: &(dyn Any + Send)) -> Self {
        Self::new(
            "panic",
            format!(
                "diagnostic provider {name:?} panicked: {}",
                panic_message(panic)
            ),
        )
    }

    fn disconnected(name: &str) -> Self {
        Self::new(
            "internal",
            format!("diagnostic provider {name:?} did not return a result"),
        )
    }

    fn not_found(name: &str) -> Self {
        Self::new("not_found", format!("unknown diagnostic provider: {name}"))
    }
}

/// Configuration error returned while building a [`crate::DevMcp`] handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Error)]
#[error("{message}")]
pub struct DevMcpConfigError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Optional machine-readable error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl DevMcpConfigError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn duplicate_diagnostic(name: &str) -> Self {
        Self::new(
            "duplicate_diagnostic",
            format!("duplicate diagnostic provider: {name}"),
        )
    }

    pub(crate) fn empty_diagnostic_name() -> Self {
        Self::new(
            "empty_diagnostic_name",
            "diagnostic provider name must not be empty",
        )
    }
}

type RuntimeDiagnosticProvider = Arc<dyn Fn() -> DiagnosticResult + Send + Sync>;
type UiDiagnosticProvider = Arc<Mutex<Box<dyn FnMut(&Context) -> DiagnosticResult + Send>>>;

#[derive(Clone)]
enum DiagnosticProvider {
    Runtime(RuntimeDiagnosticProvider),
    Ui(UiDiagnosticProvider),
}

struct UiDiagnosticRequest {
    name: String,
    provider: UiDiagnosticProvider,
    sender: mpsc::Sender<DiagnosticResult>,
}

/// Pending UI-thread diagnostic result.
pub struct DiagnosticReceiver {
    name: String,
    receiver: mpsc::Receiver<DiagnosticResult>,
}

impl DiagnosticReceiver {
    /// Wait until the UI-thread provider returns or the caller's timeout expires.
    pub fn recv_timeout(self, timeout: Duration) -> DiagnosticResult {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(DiagnosticError::new(
                "timeout",
                format!("diagnostic provider {:?} timed out", self.name),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(DiagnosticError::disconnected(&self.name)),
        }
    }
}

/// Started diagnostic execution.
pub enum DiagnosticExecution {
    /// The provider completed immediately.
    Ready(DiagnosticResult),
    /// The provider must be awaited after the UI thread drains the request.
    Queued(DiagnosticReceiver),
}

/// Registry of named app diagnostic providers.
#[derive(Clone, Default)]
pub struct DiagnosticRegistry {
    providers: Arc<Mutex<BTreeMap<String, DiagnosticProvider>>>,
    pending_ui: Arc<Mutex<VecDeque<UiDiagnosticRequest>>>,
}

impl fmt::Debug for DiagnosticRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticRegistry")
            .field(
                "providers",
                &lock(&self.providers, "diagnostic providers lock").len(),
            )
            .field(
                "pending_ui",
                &lock(&self.pending_ui, "pending diagnostic lock").len(),
            )
            .finish()
    }
}

impl DiagnosticRegistry {
    /// Create an empty diagnostic registry.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_providers_from(&self, other: &Self) {
        let providers = lock(&other.providers, "diagnostic providers lock").clone();
        *lock(&self.providers, "diagnostic providers lock") = providers;
    }

    pub(crate) fn insert_runtime<F>(
        &self,
        name: String,
        provider: F,
    ) -> Result<(), DevMcpConfigError>
    where
        F: Fn() -> DiagnosticResult + Send + Sync + 'static,
    {
        self.insert_provider(name, DiagnosticProvider::Runtime(Arc::new(provider)))
    }

    pub(crate) fn insert_ui<F>(&self, name: String, provider: F) -> Result<(), DevMcpConfigError>
    where
        F: FnMut(&Context) -> DiagnosticResult + Send + 'static,
    {
        self.insert_provider(
            name,
            DiagnosticProvider::Ui(Arc::new(Mutex::new(Box::new(provider)))),
        )
    }

    fn insert_provider(
        &self,
        name: String,
        provider: DiagnosticProvider,
    ) -> Result<(), DevMcpConfigError> {
        if name.is_empty() {
            return Err(DevMcpConfigError::empty_diagnostic_name());
        }
        let mut providers = lock(&self.providers, "diagnostic providers lock");
        if providers.contains_key(&name) {
            return Err(DevMcpConfigError::duplicate_diagnostic(&name));
        }
        providers.insert(name, provider);
        Ok(())
    }

    /// Return sorted diagnostic provider names.
    pub fn names(&self) -> Vec<String> {
        lock(&self.providers, "diagnostic providers lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Start one diagnostic provider by name.
    pub fn start(&self, name: &str) -> DiagnosticExecution {
        let provider = lock(&self.providers, "diagnostic providers lock")
            .get(name)
            .cloned();
        match provider {
            Some(DiagnosticProvider::Runtime(provider)) => {
                DiagnosticExecution::Ready(run_runtime_provider(name, &provider))
            }
            Some(DiagnosticProvider::Ui(provider)) => {
                let (sender, receiver) = mpsc::channel();
                lock(&self.pending_ui, "pending diagnostic lock").push_back(UiDiagnosticRequest {
                    name: name.to_string(),
                    provider,
                    sender,
                });
                DiagnosticExecution::Queued(DiagnosticReceiver {
                    name: name.to_string(),
                    receiver,
                })
            }
            None => DiagnosticExecution::Ready(Err(DiagnosticError::not_found(name))),
        }
    }

    /// Run every queued UI-thread diagnostic provider against the current root context.
    pub fn drain_ui(&self, ctx: &Context) {
        let requests = {
            let mut pending = lock(&self.pending_ui, "pending diagnostic lock");
            pending.drain(..).collect::<Vec<_>>()
        };
        for request in requests {
            let result = run_ui_provider(&request.name, &request.provider, ctx);
            if request.sender.send(result).is_err() {}
        }
    }
}

fn run_runtime_provider(name: &str, provider: &RuntimeDiagnosticProvider) -> DiagnosticResult {
    catch_unwind(AssertUnwindSafe(|| provider()))
        .unwrap_or_else(|panic| Err(DiagnosticError::provider_panic(name, panic.as_ref())))
}

fn run_ui_provider(name: &str, provider: &UiDiagnosticProvider, ctx: &Context) -> DiagnosticResult {
    catch_unwind(AssertUnwindSafe(|| {
        let mut provider = lock(provider, "ui diagnostic provider lock");
        provider(ctx)
    }))
    .unwrap_or_else(|panic| Err(DiagnosticError::provider_panic(name, panic.as_ref())))
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use serde_json::json;

    use super::{DiagnosticExecution, DiagnosticRegistry};

    #[test]
    fn registry_rejects_empty_and_duplicate_names() {
        let registry = DiagnosticRegistry::new();

        let empty = registry
            .insert_runtime(String::new(), || Ok(json!(null)))
            .expect_err("empty name");
        assert_eq!(
            empty.to_string(),
            "diagnostic provider name must not be empty"
        );

        registry
            .insert_runtime("ready".to_string(), || Ok(json!(true)))
            .expect("first provider");
        let duplicate = registry
            .insert_runtime("ready".to_string(), || Ok(json!(false)))
            .expect_err("duplicate name");
        assert_eq!(
            duplicate.to_string(),
            "duplicate diagnostic provider: ready"
        );
    }

    #[test]
    fn runtime_provider_panics_become_diagnostic_errors() {
        let registry = DiagnosticRegistry::new();
        registry
            .insert_runtime("panic".to_string(), || panic!("boom"))
            .expect("provider");

        let DiagnosticExecution::Ready(result) = registry.start("panic") else {
            panic!("runtime provider should complete immediately");
        };
        let error = result.expect_err("panic error");
        assert_eq!(error.code, "panic");
        assert!(error.message.contains("boom"));
    }

    #[test]
    fn missing_provider_returns_not_found() {
        let registry = DiagnosticRegistry::new();

        let DiagnosticExecution::Ready(result) = registry.start("missing") else {
            panic!("missing provider should complete immediately");
        };
        let error = result.expect_err("missing error");
        assert_eq!(error.code, "not_found");
        assert_eq!(error.message, "unknown diagnostic provider: missing");
    }

    #[test]
    fn ui_provider_runs_when_root_frame_drains_queue() {
        let registry = DiagnosticRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_provider = Arc::clone(&calls);
        registry
            .insert_ui("ui".to_string(), move |ctx| {
                calls_for_provider.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "pixels_per_point": ctx.pixels_per_point() }))
            })
            .expect("provider");

        let DiagnosticExecution::Queued(receiver) = registry.start("ui") else {
            panic!("ui provider should queue");
        };
        registry.drain_ui(&egui::Context::default());

        let value = receiver
            .recv_timeout(Duration::from_millis(10))
            .expect("ui result");
        assert_eq!(value, json!({ "pixels_per_point": 1.0 }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn syncing_providers_does_not_cancel_pending_ui_requests() {
        let registry = DiagnosticRegistry::new();
        registry
            .insert_ui("ui".to_string(), |_ctx| Ok(json!({ "ready": true })))
            .expect("provider");
        let DiagnosticExecution::Queued(receiver) = registry.start("ui") else {
            panic!("ui provider should queue");
        };

        let replacement = DiagnosticRegistry::new();
        replacement
            .insert_runtime("runtime".to_string(), || Ok(json!({ "ready": true })))
            .expect("replacement");
        registry.set_providers_from(&replacement);
        registry.drain_ui(&egui::Context::default());

        let value = receiver
            .recv_timeout(Duration::from_millis(10))
            .expect("ui result");
        assert_eq!(value, json!({ "ready": true }));
        assert_eq!(registry.names(), vec!["runtime"]);
    }
}
