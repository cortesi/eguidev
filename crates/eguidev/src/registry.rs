//! Internal state and registry capture.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use egui::{Context, Vec2 as EguiVec2};
#[cfg(feature = "devtools")]
use tokio::sync::{Notify, oneshot};

use crate::{
    actions::{ActionQueue, ActionTiming, InputAction},
    fixtures::FixtureManager,
    overlay::{OverlayDebugConfig, OverlayEntry, OverlayManager},
    types::WidgetValue,
    viewports::ViewportState,
    widget_registry::WidgetRegistry,
};
#[cfg(feature = "devtools")]
use crate::{
    fixtures::FixtureRuntime,
    screenshots::{ScreenshotDebugSnapshot, ScreenshotKind, ScreenshotManager},
};

pub fn lock<'a, T>(mutex: &'a Mutex<T>, label: &'static str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("eguidev: recovering poisoned lock: {label}");
            poisoned.into_inner()
        }
    }
}

pub struct Inner {
    pub(crate) actions: ActionQueue,
    pub(crate) viewports: ViewportState,
    pub(crate) widgets: WidgetRegistry,
    pub(crate) overlays: OverlayManager,
    contexts: Mutex<HashMap<egui::ViewportId, Context>>,
    widget_value_updates: Mutex<HashMap<WidgetValueKey, WidgetValue>>,
    scroll_overrides: Mutex<HashMap<ScrollAreaKey, EguiVec2>>,
    next_request_id: AtomicU64,
    frame_count: AtomicU64,
    pub(crate) last_action_frame: AtomicU64,
    verbose_logging: AtomicBool,
    pub(crate) fixtures: FixtureManager,
    #[cfg(feature = "devtools")]
    frame_notify: Notify,
    #[cfg(feature = "devtools")]
    fixture_runtime: FixtureRuntime,
    #[cfg(feature = "devtools")]
    pub(crate) screenshots: ScreenshotManager,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("fixtures", &self.fixtures.fixtures().len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WidgetValueKey {
    viewport_id: egui::ViewportId,
    id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ScrollAreaKey {
    viewport_id: egui::ViewportId,
    widget_id: u64,
}

impl WidgetValueKey {
    fn new(viewport_id: egui::ViewportId, id: impl Into<String>) -> Self {
        Self {
            viewport_id,
            id: id.into(),
        }
    }
}

impl ScrollAreaKey {
    fn new(viewport_id: egui::ViewportId, widget_id: u64) -> Self {
        Self {
            viewport_id,
            widget_id,
        }
    }
}

impl Inner {
    #[cfg(feature = "devtools")]
    pub(crate) fn new() -> Self {
        Self {
            actions: ActionQueue::new(),
            viewports: ViewportState::new(),
            screenshots: ScreenshotManager::new(),
            widgets: WidgetRegistry::new(),
            overlays: OverlayManager::new(),
            contexts: Mutex::new(HashMap::new()),
            widget_value_updates: Mutex::new(HashMap::new()),
            scroll_overrides: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            frame_count: AtomicU64::new(0),
            last_action_frame: AtomicU64::new(0),
            verbose_logging: AtomicBool::new(false),
            fixtures: FixtureManager::new(),
            frame_notify: Notify::new(),
            fixture_runtime: FixtureRuntime::new(),
        }
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn enqueue_fixture_request(
        &self,
        name: String,
    ) -> oneshot::Receiver<Result<(), String>> {
        let (sender, receiver) = oneshot::channel();
        self.fixtures.enqueue_fixture_request(name, move |result| {
            drop(sender.send(result));
        });
        self.fixture_runtime.notify_request();
        self.request_repaint_of(egui::ViewportId::ROOT);
        receiver
    }

    pub(crate) fn reset_fixture_transient_state(&self) {
        self.actions.clear_all();
        lock(&self.widget_value_updates, "widget value update lock").clear();
        lock(&self.scroll_overrides, "scroll overrides lock").clear();
        self.overlays.clear_transient_state();
        self.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub(crate) fn set_verbose_logging(&self, verbose_logging: bool) {
        self.verbose_logging
            .store(verbose_logging, Ordering::Relaxed);
    }

    pub(crate) fn verbose_logging(&self) -> bool {
        self.verbose_logging.load(Ordering::Relaxed)
    }

    pub(crate) fn capture_context(&self, viewport_id: egui::ViewportId, ctx: &Context) {
        let mut stored = lock(&self.contexts, "contexts lock");
        stored.insert(viewport_id, ctx.clone());
    }

    pub(crate) fn context_for(&self, viewport_id: egui::ViewportId) -> Option<Context> {
        let contexts = lock(&self.contexts, "contexts lock");
        contexts.get(&viewport_id).cloned()
    }

    pub(crate) fn has_context(&self) -> bool {
        !lock(&self.contexts, "contexts lock").is_empty()
    }

    pub(crate) fn request_repaint(&self) {
        self.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub(crate) fn request_repaint_of(&self, viewport_id: egui::ViewportId) {
        let ctx = {
            let contexts = lock(&self.contexts, "contexts lock");
            contexts.get(&viewport_id).cloned()
        };
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }

    pub(crate) fn queue_widget_value_update(
        &self,
        viewport_id: egui::ViewportId,
        id: String,
        value: WidgetValue,
    ) {
        let mut updates = lock(&self.widget_value_updates, "widget value update lock");
        updates.insert(WidgetValueKey::new(viewport_id, id), value);
        self.request_repaint_of(viewport_id);
    }

    pub(crate) fn take_widget_value_update(
        &self,
        viewport_id: egui::ViewportId,
        id: &str,
    ) -> Option<WidgetValue> {
        let mut updates = lock(&self.widget_value_updates, "widget value update lock");
        updates.remove(&WidgetValueKey::new(viewport_id, id))
    }

    pub(crate) fn set_overlay_debug_config(&self, config: OverlayDebugConfig) {
        self.overlays.set_overlay_debug_config(config);
        self.request_repaint();
    }

    pub(crate) fn set_overlay(&self, key: String, overlay: OverlayEntry) {
        self.overlays.set_overlay(key, overlay);
        self.request_repaint();
    }

    pub(crate) fn remove_overlay(&self, key: &str) {
        self.overlays.remove_overlay(key);
        self.request_repaint();
    }

    pub(crate) fn clear_overlays(&self) {
        self.overlays.clear_overlays();
        self.request_repaint();
    }

    pub(crate) fn paint_overlays(&self, ctx: &Context) {
        self.overlays
            .paint_overlays(ctx, &self.widgets, &self.viewports);
    }

    pub(crate) fn set_scroll_override(
        &self,
        viewport_id: egui::ViewportId,
        widget_id: u64,
        offset: egui::Vec2,
    ) {
        let mut overrides = lock(&self.scroll_overrides, "scroll overrides lock");
        overrides.insert(ScrollAreaKey::new(viewport_id, widget_id), offset);
        self.request_repaint_of(viewport_id);
    }

    pub(crate) fn take_scroll_override(
        &self,
        viewport_id: egui::ViewportId,
        widget_id: u64,
    ) -> Option<egui::Vec2> {
        let mut overrides = lock(&self.scroll_overrides, "scroll overrides lock");
        overrides.remove(&ScrollAreaKey::new(viewport_id, widget_id))
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn capture_screenshot_events(&self, events: &[egui::Event]) {
        self.screenshots.capture_screenshot_events(
            events,
            self.verbose_logging(),
            self.frame_count(),
        );
    }

    pub(crate) fn queue_action(&self, viewport_id: egui::ViewportId, action: InputAction) {
        self.queue_action_with_timing(viewport_id, ActionTiming::Current, action);
    }

    pub(crate) fn queue_action_with_timing(
        &self,
        viewport_id: egui::ViewportId,
        timing: ActionTiming,
        action: InputAction,
    ) {
        self.actions
            .queue_action_with_timing(viewport_id, timing, action);
        self.request_repaint_of(viewport_id);
    }

    pub(crate) fn queue_command(
        &self,
        viewport_id: egui::ViewportId,
        command: egui::ViewportCommand,
    ) {
        self.actions.queue_command(viewport_id, command);
        self.request_repaint_of(viewport_id);
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn record_screenshot_request(
        &self,
        request_id: u64,
        viewport_id: egui::ViewportId,
        kind: &ScreenshotKind,
    ) {
        self.screenshots.record_screenshot_request(
            request_id,
            viewport_id,
            kind,
            self.verbose_logging(),
            self.frame_count(),
        );
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn record_screenshot_command_sent(
        &self,
        viewport_id: egui::ViewportId,
        request_id: Option<u64>,
    ) {
        self.screenshots.record_screenshot_command_sent(
            viewport_id,
            request_id,
            self.verbose_logging(),
            self.frame_count(),
        );
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn screenshot_debug_snapshot(&self) -> ScreenshotDebugSnapshot {
        self.screenshots
            .screenshot_debug_snapshot(true, self.frame_count())
    }

    pub(crate) fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn clear_all(&self) {
        lock(&self.widget_value_updates, "widget values lock").clear();
        lock(&self.scroll_overrides, "scroll overrides lock").clear();
        lock(&self.contexts, "contexts lock").clear();
        self.actions.clear_all();
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn notify_frame_end(&self) {
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        self.frame_notify.notify_waiters();
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn frame_notify(&self) -> &Notify {
        &self.frame_notify
    }

    #[cfg(feature = "devtools")]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn wait_for_fixture_request(&self) {
        self.fixture_runtime.wait_for_request(&self.fixtures).await;
    }

    pub(crate) fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }
}

pub fn viewport_id_to_string(viewport_id: egui::ViewportId) -> String {
    if viewport_id == egui::ViewportId::ROOT {
        "root".to_string()
    } else {
        format!("vp:{:x}", viewport_id.0.value())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    use super::*;

    #[test]
    fn request_repaint_does_not_hold_contexts_lock_across_callback() {
        let inner = Arc::new(new_test_inner());
        let ctx = Context::default();
        inner.capture_context(egui::ViewportId::ROOT, &ctx);

        let inner_for_callback = Arc::clone(&inner);
        let (sender, receiver) = mpsc::channel();
        ctx.set_request_repaint_callback(move |_| {
            assert!(
                inner_for_callback
                    .context_for(egui::ViewportId::ROOT)
                    .is_some()
            );
            sender.send(()).expect("notify repaint callback");
        });

        inner.request_repaint_of(egui::ViewportId::ROOT);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("repaint callback");
    }

    fn new_test_inner() -> Inner {
        #[cfg(feature = "devtools")]
        {
            Inner::new()
        }
        #[cfg(not(feature = "devtools"))]
        {
            Inner {
                actions: ActionQueue::new(),
                viewports: ViewportState::new(),
                widgets: WidgetRegistry::new(),
                overlays: OverlayManager::new(),
                contexts: Mutex::new(HashMap::new()),
                widget_value_updates: Mutex::new(HashMap::new()),
                scroll_overrides: Mutex::new(HashMap::new()),
                next_request_id: AtomicU64::new(1),
                frame_count: AtomicU64::new(0),
                last_action_frame: AtomicU64::new(0),
                verbose_logging: AtomicBool::new(false),
                fixtures: FixtureManager::new(),
            }
        }
    }
}
