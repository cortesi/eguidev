//! Fixture management for test data injection.

use std::{collections::VecDeque, fmt, sync::Mutex};

#[cfg(feature = "devtools")]
use tokio::sync::Notify;

use crate::{registry::lock, types::FixtureSpec};

type FixtureResponder = Box<dyn FnOnce(Result<(), String>) + Send>;

/// Fixture request waiting to be handled by the app.
pub struct FixtureRequest {
    /// Fixture name to apply.
    pub name: String,
    responder: Option<FixtureResponder>,
}

impl fmt::Debug for FixtureRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FixtureRequest")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl FixtureRequest {
    /// Send the fixture result back to the waiting tool call.
    pub fn respond(mut self, result: Result<(), String>) -> bool {
        let Some(responder) = self.responder.take() else {
            return false;
        };
        responder(result);
        true
    }
}

pub struct FixtureManager {
    fixtures: Mutex<Vec<FixtureSpec>>,
    fixture_requests: Mutex<VecDeque<FixtureRequest>>,
}

impl FixtureManager {
    pub(crate) fn new() -> Self {
        Self {
            fixtures: Mutex::new(Vec::new()),
            fixture_requests: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn set_fixtures(&self, fixtures: Vec<FixtureSpec>) {
        let mut stored = lock(&self.fixtures, "fixtures lock");
        *stored = fixtures;
    }

    pub(crate) fn fixtures(&self) -> Vec<FixtureSpec> {
        lock(&self.fixtures, "fixtures lock").clone()
    }

    pub(crate) fn fixtures_sorted(&self) -> Vec<FixtureSpec> {
        let mut fixtures = self.fixtures();
        fixtures.sort_by(|a, b| a.name.cmp(&b.name));
        fixtures
    }

    pub(crate) fn has_fixture(&self, name: &str) -> bool {
        lock(&self.fixtures, "fixtures lock")
            .iter()
            .any(|fixture| fixture.name == name)
    }

    pub(crate) fn enqueue_fixture_request(
        &self,
        name: String,
        responder: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        let mut queue = lock(&self.fixture_requests, "fixture requests lock");
        queue.push_back(FixtureRequest {
            name,
            responder: Some(Box::new(responder)),
        });
    }

    pub(crate) fn collect_fixture_requests(&self) -> Vec<FixtureRequest> {
        let mut queue = lock(&self.fixture_requests, "fixture requests lock");
        queue.drain(..).collect()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn has_fixture_requests(&self) -> bool {
        !lock(&self.fixture_requests, "fixture requests lock").is_empty()
    }
}

#[cfg(feature = "devtools")]
pub struct FixtureRuntime {
    notify: Notify,
}

#[cfg(feature = "devtools")]
impl FixtureRuntime {
    pub(crate) fn new() -> Self {
        Self {
            notify: Notify::new(),
        }
    }

    pub(crate) fn notify_request(&self) {
        self.notify.notify_one();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn wait_for_request(&self, fixtures: &FixtureManager) {
        if fixtures.has_fixture_requests() {
            return;
        }
        self.notify.notified().await;
    }
}
