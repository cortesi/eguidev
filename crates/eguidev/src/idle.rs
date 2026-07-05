//! App-level idle hooks used by settle waits.

use std::{
    any::Any,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};

use egui::Context;

use crate::{diagnostics::DevMcpConfigError, registry::lock};

type RuntimeIdleProvider = Arc<dyn Fn() -> bool + Send + Sync>;
type UiIdleProvider = Arc<Mutex<Box<dyn FnMut(&Context) -> bool + Send>>>;

#[derive(Clone)]
enum IdleProvider {
    Runtime(RuntimeIdleProvider),
    Ui(UiIdleProvider),
}

/// Most recent app idle state from either a runtime or UI-thread provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleStatus {
    /// Whether the app reports itself idle.
    pub idle: bool,
    /// Human-readable status detail for settle reports.
    pub detail: String,
    /// Root frame count that produced this status, for UI-thread providers.
    pub frame: Option<u64>,
}

/// Registry for the optional app idle provider.
#[derive(Clone, Default)]
pub struct IdleRegistry {
    provider: Arc<Mutex<Option<IdleProvider>>>,
    latest_ui: Arc<Mutex<Option<IdleStatus>>>,
}

impl fmt::Debug for IdleRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdleRegistry")
            .field(
                "configured",
                &lock(&self.provider, "idle provider lock").is_some(),
            )
            .field("latest_ui", &lock(&self.latest_ui, "idle latest UI lock"))
            .finish()
    }
}

impl IdleRegistry {
    /// Create an empty idle registry.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_from(&self, other: &Self) {
        let provider = lock(&other.provider, "idle provider lock").clone();
        *lock(&self.provider, "idle provider lock") = provider;
        *lock(&self.latest_ui, "idle latest UI lock") = None;
    }

    pub(crate) fn insert_runtime<F>(&self, provider: F) -> Result<(), DevMcpConfigError>
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        self.insert_provider(IdleProvider::Runtime(Arc::new(provider)))
    }

    pub(crate) fn insert_ui<F>(&self, provider: F) -> Result<(), DevMcpConfigError>
    where
        F: FnMut(&Context) -> bool + Send + 'static,
    {
        self.insert_provider(IdleProvider::Ui(Arc::new(Mutex::new(Box::new(provider)))))
    }

    fn insert_provider(&self, provider: IdleProvider) -> Result<(), DevMcpConfigError> {
        let mut stored = lock(&self.provider, "idle provider lock");
        if stored.is_some() {
            return Err(DevMcpConfigError::new(
                "duplicate_idle_provider",
                "idle provider is already registered",
            ));
        }
        *stored = Some(provider);
        Ok(())
    }

    /// Return whether an idle provider has been configured.
    pub fn is_configured(&self) -> bool {
        lock(&self.provider, "idle provider lock").is_some()
    }

    /// Evaluate the currently configured idle provider for settle reporting.
    pub fn status(&self) -> Option<IdleStatus> {
        match lock(&self.provider, "idle provider lock").clone()? {
            IdleProvider::Runtime(provider) => Some(run_runtime_provider(&provider)),
            IdleProvider::Ui(_) => lock(&self.latest_ui, "idle latest UI lock")
                .clone()
                .or_else(|| {
                    Some(IdleStatus {
                        idle: false,
                        detail: "UI idle provider has not run on a root frame yet".to_string(),
                        frame: None,
                    })
                }),
        }
    }

    /// Run the UI-thread provider at root frame end and cache its result.
    pub fn update_ui(&self, ctx: &Context, frame: u64) {
        let Some(IdleProvider::Ui(provider)) = lock(&self.provider, "idle provider lock").clone()
        else {
            return;
        };
        let status = run_ui_provider(&provider, ctx, frame);
        *lock(&self.latest_ui, "idle latest UI lock") = Some(status);
    }
}

fn run_runtime_provider(provider: &RuntimeIdleProvider) -> IdleStatus {
    match catch_unwind(AssertUnwindSafe(|| provider())) {
        Ok(true) => IdleStatus {
            idle: true,
            detail: "app reports idle".to_string(),
            frame: None,
        },
        Ok(false) => IdleStatus {
            idle: false,
            detail: "app reports busy".to_string(),
            frame: None,
        },
        Err(panic) => IdleStatus {
            idle: false,
            detail: format!(
                "app idle provider panicked: {}",
                panic_message(panic.as_ref())
            ),
            frame: None,
        },
    }
}

fn run_ui_provider(provider: &UiIdleProvider, ctx: &Context, frame: u64) -> IdleStatus {
    match catch_unwind(AssertUnwindSafe(|| {
        let mut provider = lock(provider, "UI idle provider lock");
        provider(ctx)
    })) {
        Ok(true) => IdleStatus {
            idle: true,
            detail: format!("UI idle provider reported idle on root frame {frame}"),
            frame: Some(frame),
        },
        Ok(false) => IdleStatus {
            idle: false,
            detail: format!("UI idle provider reported busy on root frame {frame}"),
            frame: Some(frame),
        },
        Err(panic) => IdleStatus {
            idle: false,
            detail: format!(
                "UI idle provider panicked on root frame {frame}: {}",
                panic_message(panic.as_ref())
            ),
            frame: Some(frame),
        },
    }
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::IdleRegistry;

    #[test]
    fn idle_registry_rejects_duplicate_providers() {
        let registry = IdleRegistry::new();
        registry.insert_runtime(|| true).expect("first provider");

        let error = registry
            .insert_ui(|_| true)
            .expect_err("second provider should fail");

        assert_eq!(error.code, "duplicate_idle_provider");
    }

    #[test]
    fn runtime_idle_provider_reports_current_state() {
        let registry = IdleRegistry::new();
        let idle = Arc::new(AtomicBool::new(false));
        let idle_for_provider = Arc::clone(&idle);
        registry
            .insert_runtime(move || idle_for_provider.load(Ordering::Relaxed))
            .expect("provider");

        assert!(!registry.status().expect("status").idle);
        idle.store(true, Ordering::Relaxed);
        assert!(registry.status().expect("status").idle);
    }

    #[test]
    fn ui_idle_provider_caches_root_frame_result() {
        let registry = IdleRegistry::new();
        registry.insert_ui(|_| true).expect("provider");

        assert!(!registry.status().expect("status").idle);
        registry.update_ui(&egui::Context::default(), 7);

        let status = registry.status().expect("status");
        assert!(status.idle);
        assert_eq!(status.frame, Some(7));
    }
}
