//! Fixture management for test data injection.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    time::Duration,
};

use egui::Context;

use crate::{
    diagnostics::DevMcpConfigError,
    registry::lock,
    types::{FixtureCall, FixtureError, FixtureResult, FixtureSpec},
};

/// Handler that applies a fixture on the automation runtime thread.
pub type RuntimeFixtureHandler = Arc<dyn Fn(&FixtureCall) -> FixtureResult + Send + Sync>;

/// Handler that applies a fixture on the UI thread.
pub type UiFixtureHandler =
    Arc<Mutex<Box<dyn FnMut(&Context, &FixtureCall) -> FixtureResult + Send>>>;

/// Configured fixture handler.
#[derive(Clone)]
pub enum FixtureHandler {
    /// Runtime-thread handler.
    Runtime(RuntimeFixtureHandler),
    /// UI-thread handler.
    Ui(UiFixtureHandler),
}

struct UiFixtureRequest {
    call: FixtureCall,
    handler: UiFixtureHandler,
    sender: mpsc::Sender<FixtureResult>,
    cancelled: Arc<AtomicBool>,
}

/// Pending UI-thread fixture result.
pub struct UiFixtureReceiver {
    name: String,
    receiver: mpsc::Receiver<FixtureResult>,
    cancelled: Arc<AtomicBool>,
}

impl UiFixtureReceiver {
    /// Wait until the UI-thread fixture returns or the caller's timeout expires.
    pub fn recv_timeout(self, timeout: Duration) -> FixtureResult {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.cancelled.store(true, Ordering::Release);
                Err(FixtureError::new(
                    "timeout",
                    format!("fixture handler {:?} timed out", self.name),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.cancelled.store(true, Ordering::Release);
                Err(FixtureError::new(
                    "internal",
                    format!("fixture handler {:?} did not return a result", self.name),
                ))
            }
        }
    }
}

/// Started fixture execution.
pub enum FixtureExecution {
    /// The fixture handler completed immediately.
    Ready(FixtureResult),
    /// The fixture handler must be awaited after the UI thread drains the request.
    Queued(UiFixtureReceiver),
}

/// Manages fixture metadata and dispatches fixture application through a registered handler.
pub struct FixtureManager {
    fixtures: Mutex<Vec<FixtureSpec>>,
    handler: Mutex<Option<FixtureHandler>>,
    pending_ui: Mutex<VecDeque<UiFixtureRequest>>,
}

impl FixtureManager {
    /// Create a new fixture manager with no fixtures or handler.
    pub fn new() -> Self {
        Self {
            fixtures: Mutex::new(Vec::new()),
            handler: Mutex::new(None),
            pending_ui: Mutex::new(VecDeque::new()),
        }
    }

    /// Replace the registered fixture catalog.
    pub fn set_fixtures(&self, fixtures: Vec<FixtureSpec>) {
        for fixture in &fixtures {
            if let Err(error) = fixture.validate(true) {
                panic!("invalid fixture {}: {error}", fixture.name);
            }
        }
        let mut stored = lock(&self.fixtures, "fixtures lock");
        *stored = fixtures;
    }

    /// Return a snapshot of the current fixture catalog.
    pub fn fixtures(&self) -> Vec<FixtureSpec> {
        lock(&self.fixtures, "fixtures lock").clone()
    }

    /// Return the fixture catalog sorted by name.
    pub fn fixtures_sorted(&self) -> Vec<FixtureSpec> {
        let mut fixtures = self.fixtures();
        fixtures.sort_by(|a, b| a.name.cmp(&b.name));
        fixtures
    }

    /// Check whether a fixture with the given name is registered.
    pub fn has_fixture(&self, name: &str) -> bool {
        self.fixture(name).is_some()
    }

    /// Return a registered fixture by name.
    pub fn fixture(&self, name: &str) -> Option<FixtureSpec> {
        lock(&self.fixtures, "fixtures lock")
            .iter()
            .find(|fixture| fixture.name == name)
            .cloned()
    }

    /// Register the configured fixture handler.
    pub fn set_handler(&self, handler: FixtureHandler) -> Result<(), DevMcpConfigError> {
        let mut stored = lock(&self.handler, "fixture handler lock");
        if stored.is_some() {
            return Err(DevMcpConfigError::new(
                "duplicate_fixture_handler",
                "fixture handler is already registered",
            ));
        }
        *stored = Some(handler);
        Ok(())
    }

    /// Start applying a validated fixture call.
    pub fn start_fixture(&self, call: FixtureCall) -> FixtureExecution {
        let handler = lock(&self.handler, "fixture handler lock").clone();
        match handler {
            Some(FixtureHandler::Runtime(handler)) => {
                FixtureExecution::Ready(run_runtime_handler(&handler, &call))
            }
            Some(FixtureHandler::Ui(handler)) => {
                let (sender, receiver) = mpsc::channel();
                let name = call.name.clone();
                let cancelled = Arc::new(AtomicBool::new(false));
                lock(&self.pending_ui, "pending fixture lock").push_back(UiFixtureRequest {
                    call,
                    handler,
                    sender,
                    cancelled: Arc::clone(&cancelled),
                });
                FixtureExecution::Queued(UiFixtureReceiver {
                    name,
                    receiver,
                    cancelled,
                })
            }
            None => FixtureExecution::Ready(Err(FixtureError::new(
                "no_handler",
                "no fixture handler registered",
            ))),
        }
    }

    /// Run one queued UI-thread fixture handler against the current root context.
    pub fn drain_ui(&self, ctx: &Context) {
        let request = loop {
            let request = {
                let mut pending = lock(&self.pending_ui, "pending fixture lock");
                pending.pop_front()
            };
            match request {
                Some(request) if request.cancelled.load(Ordering::Acquire) => {}
                request => break request,
            }
        };
        if let Some(request) = request {
            let result = run_ui_handler(&request.handler, ctx, &request.call);
            drop(request.sender.send(result));
        }
    }
}

fn run_runtime_handler(handler: &RuntimeFixtureHandler, call: &FixtureCall) -> FixtureResult {
    catch_unwind(AssertUnwindSafe(|| handler(call)))
        .unwrap_or_else(|panic| Err(FixtureError::handler_panic(&call.name, panic.as_ref())))
}

fn run_ui_handler(handler: &UiFixtureHandler, ctx: &Context, call: &FixtureCall) -> FixtureResult {
    catch_unwind(AssertUnwindSafe(|| {
        let mut handler = lock(handler, "UI fixture handler lock");
        handler(ctx, call)
    }))
    .unwrap_or_else(|panic| Err(FixtureError::handler_panic(&call.name, panic.as_ref())))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use egui::Context;

    use super::{FixtureExecution, FixtureHandler, FixtureManager};
    use crate::types::{FixtureCall, FixtureResponse, FixtureSpec, WidgetValue};

    fn empty_call(name: &str) -> FixtureCall {
        FixtureCall {
            name: name.to_string(),
            params: FixtureSpec::new(name, "Test fixture")
                .validate_params(BTreeMap::new())
                .expect("params"),
        }
    }

    #[test]
    fn ui_fixture_drains_queued_call() {
        let manager = FixtureManager::new();
        let called = Arc::new(Mutex::new(None));
        let called_clone = Arc::clone(&called);
        manager
            .set_handler(FixtureHandler::Ui(Arc::new(Mutex::new(Box::new(
                move |_ctx, call| {
                    *called_clone.lock().expect("called lock") = Some(call.name.clone());
                    Ok(FixtureResponse::new().value("done", true))
                },
            )))))
            .expect("handler");

        let receiver = match manager.start_fixture(empty_call("ui.ready")) {
            FixtureExecution::Queued(receiver) => receiver,
            FixtureExecution::Ready(_) => panic!("expected queued UI fixture"),
        };
        manager.drain_ui(&Context::default());

        let response = receiver
            .recv_timeout(Duration::from_millis(1))
            .expect("fixture response");
        assert_eq!(
            called.lock().expect("called lock").as_deref(),
            Some("ui.ready")
        );
        assert_eq!(response.values.get("done"), Some(&WidgetValue::Bool(true)));
    }

    #[test]
    fn timed_out_ui_fixture_is_not_applied_later() {
        let manager = FixtureManager::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);
        manager
            .set_handler(FixtureHandler::Ui(Arc::new(Mutex::new(Box::new(
                move |_ctx, _call| {
                    calls_clone.fetch_add(1, Ordering::Relaxed);
                    Ok(FixtureResponse::new())
                },
            )))))
            .expect("handler");

        let receiver = match manager.start_fixture(empty_call("ui.timeout")) {
            FixtureExecution::Queued(receiver) => receiver,
            FixtureExecution::Ready(_) => panic!("expected queued UI fixture"),
        };
        let error = receiver
            .recv_timeout(Duration::ZERO)
            .expect_err("fixture should time out");
        assert_eq!(error.code, "timeout");

        manager.drain_ui(&Context::default());

        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
