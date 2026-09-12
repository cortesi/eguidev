//! DevMCP instrumentation helpers and public API.
#![allow(missing_docs)]

use std::{
    any::Any,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use egui::{Context, FullOutput};

use crate::{
    actions::InputAction,
    diagnostics::{DevMcpConfigError, DiagnosticRegistry, DiagnosticResult},
    fixtures::{FixtureHandler, RuntimeFixtureHandler, UiFixtureHandler},
    idle::IdleRegistry,
    instrument::{ACTIVE, container, swallow_panic},
    registry::{Inner, lock},
    types::{FixtureCall, FixtureResult, FixtureSpec},
};

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(250);

/// Nonblocking application shutdown request supplied by the embedding app.
pub type AppShutdownHandler = Arc<dyn Fn() + Send + Sync>;

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

    fn on_egui_output(&self, _inner: &Inner, _viewport_id: egui::ViewportId, _output: &FullOutput) {
    }
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
/// Registered automatically on the first instrumented frame of each egui
/// `Context` (see [`DevMcp::begin_frame`]), so apps do not need to wire
/// anything up themselves. Because `egui::Plugin::input_hook` runs inside the
/// public `Context::begin_pass` for root, deferred, and immediate viewports
/// alike, this makes injected input reach immediate viewports, which the old
/// app-side `raw_input_hook` override could never do (by the time an immediate
/// viewport's render callback ran, `begin_pass` had already consumed that
/// pass's `RawInput`).
///
/// Holding a `DevMcp` here (which transitively remembers `Context`s through
/// `Inner::remember_context`) creates a reference cycle with the `Context`
/// that owns this plugin. That cycle is benign: both live for the process
/// lifetime and are torn down together at process exit.
struct AutomationPlugin {
    /// The DevMCP handle whose queued actions should be drained into raw
    /// input for every pass.
    devmcp: DevMcp,
    /// Viewport reported by the input hook for the output that follows.
    output_viewport_id: Option<egui::ViewportId>,
}

impl egui::Plugin for AutomationPlugin {
    fn debug_name(&self) -> &'static str {
        "eguidev_automation"
    }

    fn input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        self.output_viewport_id = Some(raw_input.viewport_id);
        let Some(inner) = self.devmcp.inner() else {
            return;
        };
        swallow_panic("input_injection_plugin", || {
            inner.remember_context(raw_input.viewport_id, ctx);
            let base_modifiers = ctx.input(|input| input.modifiers);
            self.devmcp.drain_actions_into_raw_input(
                inner,
                raw_input.viewport_id,
                base_modifiers,
                raw_input,
            );
        });
    }

    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        let Some(inner) = self.devmcp.inner() else {
            return;
        };
        swallow_panic("automation_output_plugin", || {
            if let Some(hooks) = inner.runtime_hooks() {
                hooks.on_egui_output(
                    inner,
                    self.output_viewport_id
                        .take()
                        .unwrap_or_else(|| ctx.viewport_id()),
                    output,
                );
            }
        });
    }
}

#[derive(Default)]
struct DevMcpShared {
    fixtures: Mutex<Vec<FixtureSpec>>,
    fixture_handler: Mutex<Option<FixtureHandler>>,
    shutdown_handler: Mutex<Option<AppShutdownHandler>>,
    verbose_logging: AtomicBool,
    automation_options: Mutex<AutomationOptions>,
}

/// DevMCP handle stored in app state.
///
/// `Clone` is a cheap shared handle: configuration, the shutdown handler,
/// fixtures, diagnostics, and idle providers are observed by every clone.
#[derive(Clone, Default)]
pub struct DevMcp {
    state: DevMcpState,
    shared: Arc<DevMcpShared>,
    diagnostics: DiagnosticRegistry,
    idle: IdleRegistry,
}

impl fmt::Debug for DevMcp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DevMcp")
            .field("state", &self.state)
            .field(
                "fixtures",
                &lock(&self.shared.fixtures, "devmcp fixtures lock").len(),
            )
            .field(
                "shutdown_handler",
                &lock(
                    &self.shared.shutdown_handler,
                    "devmcp shutdown handler lock",
                )
                .is_some(),
            )
            .field("diagnostics", &self.diagnostics)
            .field("idle", &self.idle)
            .field(
                "verbose_logging",
                &self.shared.verbose_logging.load(Ordering::Relaxed),
            )
            .field(
                "automation_options",
                &*lock(
                    &self.shared.automation_options,
                    "devmcp automation options lock",
                ),
            )
            .finish()
    }
}

impl DevMcp {
    /// Create a new inert DevMCP handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable verbose internal logging for DevMCP operations.
    pub fn verbose_logging(self, verbose_logging: bool) -> Self {
        self.shared
            .verbose_logging
            .store(verbose_logging, Ordering::Relaxed);
        if let Some(inner) = self.inner() {
            inner.set_verbose_logging(verbose_logging);
        }
        self
    }

    /// Configure runtime-owned automation behavior.
    pub fn automation_options(self, options: AutomationOptions) -> Self {
        *lock(
            &self.shared.automation_options,
            "devmcp automation options lock",
        ) = options;
        if let Some(inner) = self.inner() {
            inner.set_automation_options(options);
        }
        self
    }

    /// Enable or disable runtime repaint keep-alive while automation is
    /// attached.
    pub fn keep_alive(self, keep_alive: bool) -> Self {
        let options = {
            let mut options = lock(
                &self.shared.automation_options,
                "devmcp automation options lock",
            );
            options.keep_alive = keep_alive;
            *options
        };
        if let Some(inner) = self.inner() {
            inner.set_automation_options(options);
        }
        self
    }

    /// Enable or disable egui animations while automation is attached.
    pub fn animations(self, animations: bool) -> Self {
        let options = {
            let mut options = lock(
                &self.shared.automation_options,
                "devmcp automation options lock",
            );
            options.animations = animations;
            *options
        };
        if let Some(inner) = self.inner() {
            inner.set_automation_options(options);
        }
        self
    }

    /// Register a nonblocking application shutdown request.
    ///
    /// Edev calls this handler on the app MCP runtime thread during managed
    /// shutdown. The handler must publish its request to the application's
    /// lifecycle owner and return immediately. If no handler is registered,
    /// Eguidev closes the root viewport.
    pub fn on_shutdown<F>(self, handler: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let handler: AppShutdownHandler = Arc::new(handler);
        if let Some(inner) = self.inner() {
            inner.set_shutdown_handler(Some(Arc::clone(&handler)));
        }
        *lock(
            &self.shared.shutdown_handler,
            "devmcp shutdown handler lock",
        ) = Some(handler);
        self
    }

    /// Register fixture metadata for discovery and validation.
    pub fn fixtures(self, fixtures: impl IntoIterator<Item = FixtureSpec>) -> Self {
        let fixtures: Vec<_> = fixtures.into_iter().collect();
        *lock(&self.shared.fixtures, "devmcp fixtures lock") = fixtures.clone();
        if let Some(inner) = self.inner() {
            inner.fixtures.set_fixtures(fixtures);
        }
        self
    }

    /// Register the fixture handler, to run on the automation thread.
    ///
    /// An app registers exactly one fixture handler. Calling this after either
    /// handler is already registered returns a `duplicate_fixture_handler`
    /// configuration error. The handler reaches app state by closing over it.
    pub fn on_fixture_runtime<F>(self, handler: F) -> Result<Self, DevMcpConfigError>
    where
        F: Fn(&FixtureCall) -> FixtureResult + Send + Sync + 'static,
    {
        self.ensure_no_fixture_handler()?;
        let handler: RuntimeFixtureHandler = Arc::new(handler);
        let handler = FixtureHandler::Runtime(handler);
        if let Some(inner) = self.inner() {
            inner.fixtures.set_handler(handler.clone())?;
        }
        *lock(&self.shared.fixture_handler, "devmcp fixture handler lock") = Some(handler);
        Ok(self)
    }

    /// Register the fixture handler, to run on the egui UI thread.
    ///
    /// Use this when setup must touch the `egui::Context`. An app registers
    /// exactly one fixture handler. Calling this after either handler is
    /// already registered returns a `duplicate_fixture_handler` configuration
    /// error. The handler reaches app state by closing over it.
    pub fn on_fixture_ui<F>(self, handler: F) -> Result<Self, DevMcpConfigError>
    where
        F: FnMut(&Context, &FixtureCall) -> FixtureResult + Send + 'static,
    {
        self.ensure_no_fixture_handler()?;
        let handler: UiFixtureHandler = Arc::new(Mutex::new(Box::new(handler)));
        let handler = FixtureHandler::Ui(handler);
        if let Some(inner) = self.inner() {
            inner.fixtures.set_handler(handler.clone())?;
        }
        *lock(&self.shared.fixture_handler, "devmcp fixture handler lock") = Some(handler);
        Ok(self)
    }

    /// Register a named diagnostic provider that runs on the automation runtime
    /// thread.
    pub fn diagnostic<F>(
        self,
        name: impl Into<String>,
        provider: F,
    ) -> Result<Self, DevMcpConfigError>
    where
        F: Fn() -> DiagnosticResult + Send + Sync + 'static,
    {
        self.diagnostics.insert_runtime(name.into(), provider)?;
        if let Some(inner) = self.inner() {
            inner.diagnostics.set_providers_from(&self.diagnostics);
        }
        Ok(self)
    }

    /// Register a named diagnostic provider that runs on the UI thread.
    pub fn diagnostic_ui<F>(
        self,
        name: impl Into<String>,
        provider: F,
    ) -> Result<Self, DevMcpConfigError>
    where
        F: FnMut(&Context) -> DiagnosticResult + Send + 'static,
    {
        self.diagnostics.insert_ui(name.into(), provider)?;
        if let Some(inner) = self.inner() {
            inner.diagnostics.set_providers_from(&self.diagnostics);
        }
        Ok(self)
    }

    /// Register an app-level idle check that runs on the automation runtime
    /// thread.
    pub fn on_idle<F>(self, is_idle: F) -> Result<Self, DevMcpConfigError>
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        self.idle.insert_runtime(is_idle)?;
        if let Some(inner) = self.inner() {
            inner.idle.set_from(&self.idle);
        }
        Ok(self)
    }

    /// Register an app-level idle check that runs on the UI thread at root
    /// frame end.
    pub fn on_idle_ui<F>(self, is_idle: F) -> Result<Self, DevMcpConfigError>
    where
        F: FnMut(&Context) -> bool + Send + 'static,
    {
        self.idle.insert_ui(is_idle)?;
        if let Some(inner) = self.inner() {
            inner.idle.set_from(&self.idle);
        }
        Ok(self)
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
        self.inner().map_or_else(
            || self.shared.verbose_logging.load(Ordering::Relaxed),
            |inner| inner.verbose_logging(),
        )
    }

    fn ensure_no_fixture_handler(&self) -> Result<(), DevMcpConfigError> {
        if lock(&self.shared.fixture_handler, "devmcp fixture handler lock").is_some() {
            return Err(DevMcpConfigError::new(
                "duplicate_fixture_handler",
                "fixture handler is already registered",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn context_for(&self, viewport_id: egui::ViewportId) -> Option<Context> {
        self.inner()
            .and_then(|inner| inner.context_for(viewport_id))
    }

    #[doc(hidden)]
    pub fn activate_runtime(mut self, inner: Arc<Inner>, hooks: Arc<dyn RuntimeHooks>) -> Self {
        inner.set_runtime_hooks(hooks);
        inner.set_verbose_logging(self.shared.verbose_logging.load(Ordering::Relaxed));
        inner.set_automation_options(*lock(
            &self.shared.automation_options,
            "devmcp automation options lock",
        ));
        let fixtures = lock(&self.shared.fixtures, "devmcp fixtures lock").clone();
        if !fixtures.is_empty() {
            inner.fixtures.set_fixtures(fixtures);
        }
        if let Some(handler) =
            lock(&self.shared.fixture_handler, "devmcp fixture handler lock").clone()
        {
            inner
                .fixtures
                .set_handler(handler)
                .expect("fixture handler was validated before runtime activation");
        }
        inner.set_shutdown_handler(
            lock(
                &self.shared.shutdown_handler,
                "devmcp shutdown handler lock",
            )
            .clone(),
        );
        inner.diagnostics.set_providers_from(&self.diagnostics);
        inner.idle.set_from(&self.idle);
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
    pub(crate) fn begin_frame(&self, ctx: &Context) -> bool {
        let Some(inner) = self.inner() else {
            return false;
        };
        if ctx.with_plugin::<AutomationPlugin, _>(|_| ()).is_none() {
            ctx.add_plugin(AutomationPlugin {
                devmcp: self.clone(),
                output_viewport_id: Some(ctx.viewport_id()),
            });
        }
        let viewport_id = ctx.viewport_id();
        let outermost = inner.enter_viewport_frame(viewport_id);
        let pushed = AtomicBool::new(false);
        swallow_panic("begin_frame", || {
            inner.begin_frame(viewport_id);
            inner.capture_context(viewport_id, ctx);
            if outermost && viewport_id == egui::ViewportId::ROOT {
                inner.fixtures.drain_ui(ctx);
                inner.diagnostics.drain_ui(ctx);
            }
            if let Some(hooks) = inner.runtime_hooks() {
                let events = ctx.input(|input| input.events.clone());
                hooks.on_raw_input(inner, &events);
            }
            if outermost {
                inner.widgets.clear_registry(viewport_id);
                ACTIVE.with(|active| {
                    if let Ok(mut active) = active.try_borrow_mut() {
                        active.push(Arc::clone(inner));
                        pushed.store(true, Ordering::Relaxed);
                    } else {
                        eprintln!("eguidev: begin_frame skipped; active already borrowed");
                    }
                });
            }
        });
        pushed.load(Ordering::Relaxed)
    }

    /// End a frame, finalizing widget registry and handling automation state.
    ///
    /// Prefer [`FrameGuard`] over calling this directly.
    pub(crate) fn end_frame(&self, ctx: &Context, pushed: bool) {
        let Some(inner) = self.inner() else {
            return;
        };
        let viewport_id = ctx.viewport_id();
        let outermost = inner.exit_viewport_frame(viewport_id);
        if outermost {
            swallow_panic("end_frame", || {
                self.finish_frame(inner, ctx);
            });
        }
        if pushed {
            ACTIVE.with(|active| {
                if let Ok(mut active) = active.try_borrow_mut() {
                    if active.last().is_some_and(|top| Arc::ptr_eq(top, inner)) {
                        active.pop();
                    }
                } else {
                    eprintln!("eguidev: end_frame skipped; active already borrowed");
                }
            });
        }
    }

    fn finish_frame(&self, inner: &Arc<Inner>, ctx: &Context) {
        let viewport_id = ctx.viewport_id();
        if ctx.will_discard() {
            return;
        }
        inner.widgets.finalize_registry(viewport_id);
        let next_frame = inner.frame_count() + 1;
        let fixture_epoch = inner
            .finish_frame_fixture_epoch(viewport_id)
            .unwrap_or_else(|| inner.fixture_epoch());
        inner
            .viewports
            .capture_input_snapshot(ctx, fixture_epoch, next_frame);
        let pointer_pos = inner
            .viewports
            .input_snapshot(viewport_id)
            .and_then(|snapshot| snapshot.pointer_pos);
        inner
            .actions
            .record_pointer_report(viewport_id, next_frame, pointer_pos);
        if viewport_id == egui::ViewportId::ROOT {
            inner.idle.update_ui(ctx, next_frame);
        }
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
        base_modifiers: egui::Modifiers,
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
        let mut current_modifiers = base_modifiers;
        let mut modifiers_changed = false;
        let mut force_focus = false;
        let pointer_moved = actions
            .iter()
            .any(|action| matches!(action, InputAction::PointerMove { .. }));
        for action in &actions {
            if let InputAction::Key {
                pressed, modifiers, ..
            } = action
            {
                modifiers_changed = true;
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
        if force_focus {
            raw_input.focused = true;
        }
        for action in actions {
            action.apply(raw_input);
        }
        if !pointer_moved && let Some(pos) = inner.actions.pointer_pos(viewport_id) {
            raw_input.events.push(egui::Event::PointerMoved(pos.into()));
        }
        if modifiers_changed {
            raw_input
                .events
                .push(egui::Event::ModifiersChanged(current_modifiers));
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
    /// Whether this guard pushed the recording stack.
    pushed: bool,
}

impl<'a> FrameGuard<'a> {
    /// Create a new frame guard for the provided DevMcp.
    pub fn new(devmcp: &'a DevMcp, ctx: &'a Context) -> Self {
        let pushed = devmcp.begin_frame(ctx);
        Self {
            devmcp,
            ctx,
            pushed,
        }
    }
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        self.devmcp.end_frame(self.ctx, self.pushed);
    }
}

/// Wrap one viewport frame and register `container_id` as its root container.
///
/// Call this once for each rendered viewport pass, and render all instrumented
/// widgets for that pass inside `add_contents`. If the viewport has a semantic
/// name, call `name_viewport` from inside `add_contents` so the active frame is
/// already installed.
pub fn frame_scope<R>(
    devmcp: &DevMcp,
    ui: &mut egui::Ui,
    container_id: impl Into<String>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let ctx = ui.ctx().clone();
    let _guard = FrameGuard::new(devmcp, &ctx);
    container(ui, container_id, add_contents)
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
    use crate::{actions::InputAction, instrument, registry::Inner, types::Pos2, ui_ext::DevUiExt};

    #[derive(Default)]
    struct CountingRuntimeHooks {
        raw_input_calls: AtomicUsize,
        raw_input_events: AtomicUsize,
        frame_end_calls: AtomicUsize,
        output_calls: AtomicUsize,
        output_viewports: Mutex<Vec<egui::ViewportId>>,
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

        fn on_egui_output(
            &self,
            _inner: &Inner,
            viewport_id: egui::ViewportId,
            _output: &FullOutput,
        ) {
            self.output_calls.fetch_add(1, AtomicOrdering::Relaxed);
            self.output_viewports
                .lock()
                .expect("output viewports lock")
                .push(viewport_id);
        }
    }

    #[test]
    fn inactive_input_hook_plugin_is_a_noop() {
        let devmcp = DevMcp::new();
        let ctx = Context::default();
        let mut plugin = AutomationPlugin {
            devmcp,
            output_viewport_id: None,
        };
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
        let mut plugin = AutomationPlugin {
            devmcp,
            output_viewport_id: None,
        };
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
    fn popup_dismissal_delivers_escape_only_to_its_viewport() {
        let inner = Arc::new(Inner::new());
        let secondary = egui::ViewportId::from_hash_of("secondary");
        let ctx = Context::default();
        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        inner.capture_context(secondary, &ctx);
        let devmcp = DevMcp::new().activate_runtime(
            Arc::clone(&inner),
            Arc::new(CountingRuntimeHooks::default()),
        );
        let mut plugin = AutomationPlugin {
            devmcp,
            output_viewport_id: None,
        };

        inner.dismiss_transient_ui(Some(secondary));

        let mut root_input = egui::RawInput::default();
        plugin.input_hook(&ctx, &mut root_input);
        assert!(root_input.events.is_empty());

        let mut secondary_input = egui::RawInput {
            viewport_id: secondary,
            ..Default::default()
        };
        plugin.input_hook(&ctx, &mut secondary_input);
        let mut expected = [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect::<Vec<_>>();
        expected.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        assert_eq!(secondary_input.events, expected);
        assert!(!inner.actions.has_pending_actions(secondary));
    }

    #[test]
    fn popup_dismissal_preserves_other_viewport_popup_and_focus() {
        let inner = Arc::new(Inner::new());
        let devmcp = DevMcp::new().activate_runtime(
            Arc::clone(&inner),
            Arc::new(CountingRuntimeHooks::default()),
        );
        let ctx = Context::default();
        let root = egui::ViewportId::ROOT;
        let secondary = egui::ViewportId::from_hash_of("secondary");
        let input = |viewport_id| {
            let mut raw = egui::RawInput {
                viewport_id,
                ..Default::default()
            };
            raw.viewports.insert(secondary, Default::default());
            raw
        };
        // Install the plugin before opening either viewport's transient UI.
        ctx.run_ui(input(root), |ui| {
            let _guard = FrameGuard::new(&devmcp, ui.ctx());
        })
        .drop_without_applying_deltas();
        let render = |viewport_id, open| {
            let mut state = (false, false);
            ctx.run_ui(input(viewport_id), |ui| {
                let pass_context = ui.ctx().clone();
                let _guard = FrameGuard::new(&devmcp, &pass_context);
                let edit_id = egui::Id::new((viewport_id, "edit"));
                let popup_id = egui::Id::new((viewport_id, "popup"));
                let mut text = String::new();
                let edit = ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
                let anchor = ui.button("Open");
                if open {
                    egui::Popup::open_id(ui.ctx(), popup_id);
                    edit.request_focus();
                }
                egui::Popup::new(popup_id, ui.ctx().clone(), &anchor, anchor.layer_id)
                    .open_memory(None)
                    .show(|ui| {
                        ui.label("Popup");
                    });
                state = (
                    egui::Popup::is_id_open(ui.ctx(), popup_id),
                    ui.ctx().memory(|memory| memory.has_focus(edit_id)),
                );
            })
            .drop_without_applying_deltas();
            state
        };
        assert_eq!(render(root, true), (true, true));
        assert_eq!(render(secondary, true), (true, true));

        inner.dismiss_transient_ui(Some(root));

        assert_eq!(render(root, false), (false, false));
        assert_eq!(render(secondary, false), (true, true));
    }

    #[test]
    fn input_hook_plugin_retains_the_synthetic_pointer_position() {
        let inner = Arc::new(Inner::new());
        let viewport_id = egui::ViewportId::ROOT;
        let pos = Pos2 { x: 12.0, y: 34.0 };
        inner.queue_action(viewport_id, InputAction::PointerMove { pos });
        let devmcp =
            DevMcp::new().activate_runtime(inner, Arc::new(CountingRuntimeHooks::default()));
        let mut plugin = AutomationPlugin {
            devmcp,
            output_viewport_id: None,
        };
        let ctx = Context::default();
        let mut first = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        plugin.input_hook(&ctx, &mut first);
        assert_eq!(
            first.events,
            vec![egui::Event::PointerMoved(egui::pos2(12.0, 34.0))]
        );

        let mut next = egui::RawInput {
            viewport_id,
            events: vec![egui::Event::PointerGone],
            ..Default::default()
        };
        plugin.input_hook(&ctx, &mut next);
        assert_eq!(
            next.events,
            vec![
                egui::Event::PointerGone,
                egui::Event::PointerMoved(egui::pos2(12.0, 34.0)),
            ]
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

        ctx.run_ui(raw_input, |ui| {
            let _guard = FrameGuard::new(&devmcp, ui.ctx());
        })
        .drop_without_applying_deltas();

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
        assert_eq!(
            hooks.output_calls.load(AtomicOrdering::Relaxed),
            1,
            "automation plugin should forward completed output"
        );
    }

    #[test]
    fn frame_guard_publishes_only_the_settled_multipass_registry() {
        let inner = Arc::new(Inner::new());
        let hooks = Arc::new(CountingRuntimeHooks::default());
        let runtime_hooks: Arc<dyn RuntimeHooks> = hooks.clone();
        let devmcp = DevMcp::new().activate_runtime(Arc::clone(&inner), runtime_hooks);
        let ctx = Context::default();
        let pass = AtomicUsize::new(0);

        ctx.run_ui(egui::RawInput::default(), |ui| {
            let pass_context = ui.ctx().clone();
            let _guard = FrameGuard::new(&devmcp, &pass_context);
            if pass.fetch_add(1, AtomicOrdering::Relaxed) == 0 {
                let _response = ui.dev_button("discarded", "Discarded");
                ui.ctx().request_discard("test sizing pass");
            } else {
                let _response = ui.dev_button("settled", "Settled");
            }
        })
        .drop_without_applying_deltas();

        let widgets = inner.widgets.widget_list(egui::ViewportId::ROOT);
        assert_eq!(
            widgets
                .iter()
                .map(|widget| widget.id.as_str())
                .collect::<Vec<_>>(),
            ["settled"]
        );
        assert_eq!(inner.frame_count(), 1);
        assert_eq!(hooks.frame_end_calls.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn frame_guard_installs_automation_plugin_on_each_context() {
        let inner = Arc::new(Inner::new());
        let hooks = Arc::new(CountingRuntimeHooks::default());
        let runtime_hooks: Arc<dyn RuntimeHooks> = hooks.clone();
        let devmcp = DevMcp::new().activate_runtime(inner, runtime_hooks);
        let root = Context::default();
        let secondary = Context::default();

        for (ctx, viewport_id) in [
            (&root, egui::ViewportId::ROOT),
            (&secondary, egui::ViewportId::from_hash_of("secondary")),
        ] {
            let mut raw_input = egui::RawInput {
                viewport_id,
                ..Default::default()
            };
            raw_input.viewports.insert(viewport_id, Default::default());
            ctx.run_ui(raw_input, |ui| {
                let _guard = FrameGuard::new(&devmcp, ui.ctx());
            })
            .drop_without_applying_deltas();
        }

        assert_eq!(
            hooks.output_calls.load(AtomicOrdering::Relaxed),
            2,
            "each egui context should forward its completed output"
        );
        assert_eq!(
            *hooks
                .output_viewports
                .lock()
                .expect("output viewports lock"),
            [
                egui::ViewportId::ROOT,
                egui::ViewportId::from_hash_of("secondary"),
            ]
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

        ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            let _guard = FrameGuard::new(&devmcp, &ctx);
            let _response = ui.dev_button("inactive.button", "Inactive");
        })
        .drop_without_applying_deltas();

        assert!(devmcp.context_for(egui::ViewportId::ROOT).is_none());
        assert_eq!(instrument::test_layout_capture_count(), 0);
    }

    #[test]
    fn nested_frame_guard_keeps_parent_recording() {
        let inner = Arc::new(Inner::new());
        let hooks: Arc<dyn RuntimeHooks> = Arc::new(CountingRuntimeHooks::default());
        let devmcp = DevMcp::new().activate_runtime(inner, hooks);
        let ctx = Context::default();

        ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            let _outer = FrameGuard::new(&devmcp, &ctx);
            let _response = ui.dev_button("outer.first", "First");
            {
                let _inner = FrameGuard::new(&devmcp, &ctx);
                let _response = ui.dev_button("inner", "Inner");
            }
            assert!(
                instrument::active_inner().is_some(),
                "parent frame should stay active after the nested guard drops"
            );
            let _response = ui.dev_button("outer.second", "Second");
        })
        .drop_without_applying_deltas();

        let widgets = devmcp
            .inner()
            .expect("attached inner")
            .widgets
            .widget_list(egui::ViewportId::ROOT);
        let ids = widgets
            .iter()
            .map(|widget| widget.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"outer.first"), "{ids:?}");
        assert!(ids.contains(&"inner"), "{ids:?}");
        assert!(ids.contains(&"outer.second"), "{ids:?}");
        assert!(
            ctx.plugin_opt::<AutomationPlugin>().is_some(),
            "second frame on the same context should reuse the installed plugin"
        );
    }

    #[test]
    fn finish_frame_panic_does_not_leave_recording_armed() {
        struct PanicOnEnd;

        impl RuntimeHooks for PanicOnEnd {
            fn as_any(&self) -> &(dyn Any + Send + Sync) {
                self
            }

            fn on_frame_end(&self, _inner: &Inner, _ctx: &Context) {
                panic!("finish_frame test panic");
            }
        }

        let inner = Arc::new(Inner::new());
        let hooks: Arc<dyn RuntimeHooks> = Arc::new(PanicOnEnd);
        let devmcp = DevMcp::new().activate_runtime(inner, hooks);
        let ctx = Context::default();

        ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            let _guard = FrameGuard::new(&devmcp, &ctx);
            let _response = ui.dev_button("inside", "Inside");
        })
        .drop_without_applying_deltas();

        assert!(instrument::active_inner().is_none());

        ctx.run_ui(egui::RawInput::default(), |ui| {
            let _response = ui.dev_button("ungarded", "Ungarded");
        })
        .drop_without_applying_deltas();

        let widgets = devmcp
            .inner()
            .expect("attached inner")
            .widgets
            .widget_list(egui::ViewportId::ROOT);
        assert!(
            widgets.iter().all(|widget| widget.id != "ungarded"),
            "unguarded widgets must not record after a swallowed finish_frame panic"
        );
    }

    #[test]
    fn clone_shares_configuration() {
        let a = DevMcp::new();
        let b = a.clone();
        let spec = FixtureSpec::new("shared.fixture", "Shared");
        let b = b
            .verbose_logging(true)
            .keep_alive(false)
            .on_shutdown(|| {})
            .fixtures([spec]);
        let a = a.animations(true);

        assert!(a.verbose_logging_enabled());
        assert!(b.verbose_logging_enabled());
        assert_eq!(
            lock(&a.shared.fixtures, "devmcp fixtures lock")
                .iter()
                .map(|fixture| fixture.name.as_str())
                .collect::<Vec<_>>(),
            ["shared.fixture"]
        );
        let a_options = *lock(
            &a.shared.automation_options,
            "devmcp automation options lock",
        );
        let b_options = *lock(
            &b.shared.automation_options,
            "devmcp automation options lock",
        );
        assert!(!a_options.keep_alive);
        assert!(a_options.animations);
        assert_eq!(a_options, b_options);
        assert!(lock(&a.shared.fixture_handler, "devmcp fixture handler lock").is_none());
        assert!(lock(&a.shared.shutdown_handler, "devmcp shutdown handler lock").is_some());
    }
}
