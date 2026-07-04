//! DevMCP instrumentation helpers and public API.
#![allow(missing_docs)]

use std::{
    any::Any,
    fmt,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use egui::Context;

use crate::{
    actions::InputAction,
    fixtures::FixtureHandler,
    instrument::{ACTIVE, swallow_panic},
    registry::Inner,
    types::FixtureSpec,
};

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default)]
enum DevMcpState {
    #[default]
    Inactive,
    Active(Arc<Inner>),
}

pub trait RuntimeHooks: Send + Sync {
    fn as_any(&self) -> &(dyn Any + Send + Sync);

    fn on_raw_input(&self, _inner: &Inner, _events: &[egui::Event]) {}

    fn on_frame_end(&self, _inner: &Inner, _ctx: &Context) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomationOptions {
    pub keep_alive: bool,
    pub animations: bool,
}

impl Default for AutomationOptions {
    fn default() -> Self {
        Self {
            keep_alive: true,
            animations: false,
        }
    }
}

/// Egui plugin that injects DevMCP-queued input into every pass of every
/// viewport.
///
/// Registered automatically on the first instrumented frame (see
/// [`DevMcp::begin_frame`]), so apps do not need to wire anything up
/// themselves. Because `egui::Plugin::input_hook` runs inside the public
/// `Context::begin_pass` for root, deferred, and immediate viewports alike,
/// this makes injected input reach immediate viewports, which the old
/// app-side `raw_input_hook` override could never do (by the time an
/// immediate viewport's render callback ran, `begin_pass` had already
/// consumed that pass's `RawInput`).
///
/// Holding a `DevMcp` here (which transitively remembers `Context`s through
/// `Inner::remember_context`) creates a reference cycle with the `Context`
/// that owns this plugin. That cycle is benign: both live for the process
/// lifetime and are torn down together at process exit.
struct InputInjectionPlugin {
    /// The DevMCP handle whose queued actions should be drained into raw
    /// input for every pass.
    devmcp: DevMcp,
}

impl egui::Plugin for InputInjectionPlugin {
    fn debug_name(&self) -> &'static str {
        "eguidev_input_injection"
    }

    fn input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        let Some(inner) = self.devmcp.inner() else {
            return;
        };
        swallow_panic("input_injection_plugin", || {
            inner.remember_context(raw_input.viewport_id, ctx);
            self.devmcp
                .drain_actions_into_raw_input(inner, raw_input.viewport_id, raw_input);
        });
    }
}

/// DevMCP handle stored in app state.
#[derive(Clone, Default)]
pub struct DevMcp {
    state: DevMcpState,
    fixtures: Vec<FixtureSpec>,
    verbose_logging: bool,
    fixture_handler: Option<FixtureHandler>,
    automation_options: AutomationOptions,
}

impl fmt::Debug for DevMcp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DevMcp")
            .field("state", &self.state)
            .field("fixtures", &self.fixtures)
            .field("verbose_logging", &self.verbose_logging)
            .field("automation_options", &self.automation_options)
            .finish()
    }
}

impl DevMcp {
    /// Create a new inert DevMCP handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable verbose internal logging for DevMCP operations.
    pub fn verbose_logging(mut self, verbose_logging: bool) -> Self {
        self.verbose_logging = verbose_logging;
        if let Some(inner) = self.inner() {
            inner.set_verbose_logging(verbose_logging);
        }
        self
    }

    /// Configure runtime-owned automation behavior.
    pub fn automation_options(mut self, options: AutomationOptions) -> Self {
        self.automation_options = options;
        if let Some(inner) = self.inner() {
            inner.set_automation_options(options);
        }
        self
    }

    /// Enable or disable runtime repaint keep-alive while automation is attached.
    pub fn keep_alive(mut self, keep_alive: bool) -> Self {
        self.automation_options.keep_alive = keep_alive;
        if let Some(inner) = self.inner() {
            inner.set_automation_options(self.automation_options);
        }
        self
    }

    /// Enable or disable egui animations while automation is attached.
    pub fn animations(mut self, animations: bool) -> Self {
        self.automation_options.animations = animations;
        if let Some(inner) = self.inner() {
            inner.set_automation_options(self.automation_options);
        }
        self
    }

    /// Register fixture metadata for discovery and validation.
    pub fn fixtures(mut self, fixtures: impl IntoIterator<Item = FixtureSpec>) -> Self {
        self.fixtures = fixtures.into_iter().collect();
        if let Some(inner) = self.inner() {
            inner.fixtures.set_fixtures(self.fixtures.clone());
        }
        self
    }

    /// Register a callback that applies named fixtures to app state.
    ///
    /// The handler is called directly from the tokio runtime when a fixture
    /// tool call arrives, removing the need for frame-driven polling.
    pub fn on_fixture<F>(mut self, handler: F) -> Self
    where
        F: Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    {
        let handler: FixtureHandler = Arc::new(handler);
        if let Some(inner) = self.inner() {
            inner.fixtures.set_fixture_handler(handler.clone());
        }
        self.fixture_handler = Some(handler);
        self
    }

    /// Returns true if DevMCP automation is attached.
    pub fn is_enabled(&self) -> bool {
        matches!(self.state, DevMcpState::Active(_))
    }

    #[doc(hidden)]
    pub fn inner_arc(&self) -> Option<Arc<Inner>> {
        self.inner().map(Arc::clone)
    }

    #[doc(hidden)]
    pub fn runtime_hooks(&self) -> Option<Arc<dyn RuntimeHooks>> {
        self.inner().and_then(|inner| inner.runtime_hooks())
    }

    fn verbose_logging_enabled(&self) -> bool {
        self.inner()
            .map_or(self.verbose_logging, |inner| inner.verbose_logging())
    }

    #[cfg(test)]
    fn context_for(&self, viewport_id: egui::ViewportId) -> Option<Context> {
        self.inner()
            .and_then(|inner| inner.context_for(viewport_id))
    }

    #[doc(hidden)]
    pub fn activate_runtime(mut self, inner: Arc<Inner>, hooks: Arc<dyn RuntimeHooks>) -> Self {
        inner.set_runtime_hooks(hooks);
        inner.set_verbose_logging(self.verbose_logging);
        inner.set_automation_options(self.automation_options);
        if !self.fixtures.is_empty() {
            inner.fixtures.set_fixtures(self.fixtures.clone());
        }
        if let Some(handler) = &self.fixture_handler {
            inner.fixtures.set_fixture_handler(handler.clone());
        }
        self.state = DevMcpState::Active(inner);
        self
    }

    pub(crate) fn inner(&self) -> Option<&Arc<Inner>> {
        match &self.state {
            DevMcpState::Inactive => None,
            DevMcpState::Active(inner) => Some(inner),
        }
    }

    /// Begin a frame, enabling widget tracking for this thread.
    ///
    /// Prefer [`FrameGuard`] over calling this directly.
    pub(crate) fn begin_frame(&self, ctx: &Context) {
        let Some(inner) = self.inner() else {
            return;
        };
        if inner.try_install_input_plugin() {
            ctx.add_plugin(InputInjectionPlugin {
                devmcp: self.clone(),
            });
        }
        swallow_panic("begin_frame", || {
            let viewport_id = ctx.viewport_id();
            inner.begin_frame(viewport_id);
            inner.capture_context(viewport_id, ctx);
            if let Some(hooks) = inner.runtime_hooks() {
                let events = ctx.input(|input| input.events.clone());
                hooks.on_raw_input(inner, &events);
            }
            inner.widgets.clear_registry(viewport_id);
            ACTIVE.with(|active| {
                if let Ok(mut active) = active.try_borrow_mut() {
                    *active = Some(Arc::clone(inner));
                } else {
                    eprintln!("eguidev: begin_frame skipped; active already borrowed");
                }
            });
        });
    }

    /// End a frame, finalizing widget registry and handling automation state.
    ///
    /// Prefer [`FrameGuard`] over calling this directly.
    pub(crate) fn end_frame(&self, ctx: &Context) {
        let Some(inner) = self.inner() else {
            return;
        };
        swallow_panic("end_frame", || {
            self.finish_frame(inner, ctx);
            ACTIVE.with(|active| {
                if let Ok(mut active) = active.try_borrow_mut() {
                    *active = None;
                } else {
                    eprintln!("eguidev: end_frame skipped; active already borrowed");
                }
            });
        });
    }

    fn finish_frame(&self, inner: &Arc<Inner>, ctx: &Context) {
        let viewport_id = ctx.viewport_id();
        inner.widgets.finalize_registry(viewport_id);
        let next_frame = inner.frame_count() + 1;
        let fixture_epoch = inner
            .finish_frame_fixture_epoch(viewport_id)
            .unwrap_or_else(|| inner.fixture_epoch());
        inner
            .viewports
            .capture_input_snapshot(ctx, fixture_epoch, next_frame);
        inner.advance_frame();
        if let Some(hooks) = inner.runtime_hooks() {
            hooks.on_frame_end(inner, ctx);
            if inner.automation_options().keep_alive {
                ctx.request_repaint_after(KEEP_ALIVE_INTERVAL);
            }
        }
    }

    /// Clear script-visible widgets for a viewport that the app has hidden.
    pub fn clear_viewport(&self, viewport_id: egui::ViewportId) {
        let Some(inner) = self.inner() else {
            return;
        };
        inner.widgets.clear_registry(viewport_id);
        inner.widgets.finalize_registry(viewport_id);
    }

    fn drain_actions_into_raw_input(
        &self,
        inner: &Arc<Inner>,
        viewport_id: egui::ViewportId,
        raw_input: &mut egui::RawInput,
    ) {
        let actions = inner
            .actions
            .drain_actions(viewport_id, inner.frame_count());
        if !actions.is_empty() {
            inner
                .last_action_frame
                .store(inner.frame_count(), Ordering::Relaxed);
            if self.verbose_logging_enabled() {
                eprintln!(
                    "eguidev: input_hook viewport={:?} actions={}",
                    viewport_id,
                    actions.len()
                );
            }
        }
        let base_modifiers = raw_input.modifiers;
        let mut current_modifiers = base_modifiers;
        let mut force_focus = false;
        for action in &actions {
            if let InputAction::Key {
                pressed, modifiers, ..
            } = action
            {
                current_modifiers = if *pressed {
                    base_modifiers.plus((*modifiers).into())
                } else {
                    base_modifiers
                };
            }
            if matches!(
                action,
                InputAction::Key { .. } | InputAction::Text { .. } | InputAction::Paste { .. }
            ) {
                force_focus = true;
            }
        }
        raw_input.modifiers = current_modifiers;
        if force_focus {
            raw_input.focused = true;
        }
        for action in actions {
            action.apply(raw_input);
        }
    }
}

/// RAII guard that calls `begin_frame` and `end_frame` automatically.
#[must_use = "FrameGuard must be held for the duration of the frame"]
pub struct FrameGuard<'a> {
    /// DevMcp handle for the active frame.
    devmcp: &'a DevMcp,
    /// Egui context for the current frame.
    ctx: &'a egui::Context,
}

impl<'a> FrameGuard<'a> {
    /// Create a new frame guard for the provided DevMcp.
    pub fn new(devmcp: &'a DevMcp, ctx: &'a Context) -> Self {
        devmcp.begin_frame(ctx);
        Self { devmcp, ctx }
    }
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        self.devmcp.end_frame(self.ctx);
    }
}

/// Clear script-visible widgets for a viewport that is no longer rendered.
pub fn clear_viewport(devmcp: &DevMcp, viewport_id: egui::ViewportId) {
    devmcp.clear_viewport(viewport_id);
}

#[cfg(test)]
#[allow(deprecated)]
#[allow(clippy::tests_outside_test_module)]
mod inactive_tests {
    use std::{
        any::Any,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
    };

    use egui::{Context, Plugin};

    use super::*;
    use crate::{actions::InputAction, instrument, registry::Inner, ui_ext::DevUiExt};

    #[derive(Default)]
    struct CountingRuntimeHooks {
        raw_input_calls: AtomicUsize,
        raw_input_events: AtomicUsize,
        frame_end_calls: AtomicUsize,
    }

    impl RuntimeHooks for CountingRuntimeHooks {
        fn as_any(&self) -> &(dyn Any + Send + Sync) {
            self
        }

        fn on_raw_input(&self, _inner: &Inner, events: &[egui::Event]) {
            self.raw_input_calls.fetch_add(1, AtomicOrdering::Relaxed);
            self.raw_input_events
                .fetch_add(events.len(), AtomicOrdering::Relaxed);
        }

        fn on_frame_end(&self, _inner: &Inner, _ctx: &Context) {
            self.frame_end_calls.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }

    #[test]
    fn inactive_input_hook_plugin_is_a_noop() {
        let devmcp = DevMcp::new();
        let ctx = Context::default();
        let mut plugin = InputInjectionPlugin { devmcp };
        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            focused: false,
            ..Default::default()
        };

        plugin.input_hook(&ctx, &mut raw_input);

        assert!(!raw_input.focused);
        assert!(raw_input.events.is_empty());
    }

    #[test]
    fn input_hook_plugin_injects_queued_actions_for_viewport() {
        let inner = Arc::new(Inner::new());
        let hooks: Arc<dyn RuntimeHooks> = Arc::new(CountingRuntimeHooks::default());
        let viewport_id = egui::ViewportId::from_hash_of("secondary");
        inner.queue_action(
            viewport_id,
            InputAction::Text {
                text: "event".to_string(),
            },
        );
        let devmcp = DevMcp::new().activate_runtime(inner, hooks);
        let mut plugin = InputInjectionPlugin { devmcp };
        let ctx = Context::default();
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };

        plugin.input_hook(&ctx, &mut raw_input);

        assert_eq!(
            raw_input.events,
            vec![egui::Event::Text("event".to_string())],
            "plugin should inject queued actions for the pass's viewport"
        );
    }

    #[test]
    fn frame_guard_forwards_input_events_to_runtime_hooks() {
        let inner = Arc::new(Inner::new());
        let hooks = Arc::new(CountingRuntimeHooks::default());
        let runtime_hooks: Arc<dyn RuntimeHooks> = hooks.clone();
        let devmcp = DevMcp::new().activate_runtime(inner, runtime_hooks);
        let ctx = Context::default();
        let raw_input = egui::RawInput {
            events: vec![egui::Event::Text("event".to_string())],
            ..Default::default()
        };

        let _output = ctx.run_ui(raw_input, |ui| {
            let _guard = FrameGuard::new(&devmcp, ui.ctx());
        });

        assert_eq!(
            hooks.raw_input_calls.load(AtomicOrdering::Relaxed),
            1,
            "frame guard should notify runtime hooks about input events"
        );
        assert_eq!(
            hooks.raw_input_events.load(AtomicOrdering::Relaxed),
            1,
            "frame guard should forward input events"
        );
    }

    #[test]
    fn frame_guard_requests_repaint_when_keep_alive_is_enabled() {
        let inner = Arc::new(Inner::new());
        let hooks: Arc<dyn RuntimeHooks> = Arc::new(CountingRuntimeHooks::default());
        let devmcp = DevMcp::new().activate_runtime(inner, hooks);
        let ctx = Context::default();
        let repaint_delays = Arc::new(Mutex::new(Vec::new()));
        let repaint_delays_for_callback = Arc::clone(&repaint_delays);
        ctx.set_request_repaint_callback(move |info| {
            repaint_delays_for_callback
                .lock()
                .expect("repaint delay lock")
                .push(info.delay);
        });

        {
            let _guard = FrameGuard::new(&devmcp, &ctx);
        }

        let repaint_delays = repaint_delays.lock().expect("repaint delay lock");
        assert_eq!(repaint_delays.len(), 1);
        assert!(repaint_delays[0] > Duration::from_millis(200));
        assert!(repaint_delays[0] <= KEEP_ALIVE_INTERVAL);
    }

    #[test]
    fn frame_guard_does_not_request_repaint_when_keep_alive_is_disabled() {
        let inner = Arc::new(Inner::new());
        let hooks = Arc::new(CountingRuntimeHooks::default());
        let runtime_hooks: Arc<dyn RuntimeHooks> = hooks.clone();
        let devmcp = DevMcp::new()
            .keep_alive(false)
            .activate_runtime(inner, runtime_hooks);
        let ctx = Context::default();
        let repaint_count = Arc::new(AtomicUsize::new(0));
        let repaint_count_for_callback = Arc::clone(&repaint_count);
        ctx.set_request_repaint_callback(move |_| {
            repaint_count_for_callback.fetch_add(1, AtomicOrdering::Relaxed);
        });

        {
            let _guard = FrameGuard::new(&devmcp, &ctx);
        }

        assert_eq!(hooks.frame_end_calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(repaint_count.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn inactive_frame_guard_does_not_capture_context() {
        let devmcp = DevMcp::new();
        let ctx = Context::default();
        instrument::reset_test_counters();

        let _output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            let _guard = FrameGuard::new(&devmcp, &ctx);
            ui.dev_button("inactive.button", "Inactive");
        });

        assert!(devmcp.context_for(egui::ViewportId::ROOT).is_none());
        assert_eq!(instrument::test_layout_capture_count(), 0);
    }
}
