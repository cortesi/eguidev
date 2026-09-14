//! Internal state and registry capture.
#![allow(missing_docs)]

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use egui::{Context, Vec2 as EguiVec2};
use serde_json::json;

use crate::{
    actions::{ActionQueue, ActionTiming, InputAction},
    devmcp::{AppShutdownHandler, AutomationOptions, RuntimeHooks},
    diagnostics::DiagnosticRegistry,
    error::{ErrorCode, ToolError},
    fixtures::{FixtureExecution, FixtureManager},
    idle::IdleRegistry,
    overlay::{OverlayDebugConfig, OverlayEntry, OverlayManager},
    types::{FixtureCall, Modifiers, WidgetValue},
    viewports::{FrameHealth, ViewportState},
    widget_registry::WidgetRegistry,
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
    pub actions: ActionQueue,
    pub viewports: ViewportState,
    pub widgets: WidgetRegistry,
    pub overlays: OverlayManager,
    contexts: Mutex<HashMap<egui::ViewportId, Context>>,
    animation_baselines: Mutex<HashMap<egui::ViewportId, f32>>,
    widget_value_updates: Mutex<HashMap<WidgetValueKey, WidgetValueUpdate>>,
    widget_value_consumers: Mutex<HashSet<WidgetValueKey>>,
    scroll_overrides: Mutex<HashMap<ScrollAreaKey, EguiVec2>>,
    frame_fixture_epochs: Mutex<HashMap<egui::ViewportId, u64>>,
    frame_depth: Mutex<HashMap<egui::ViewportId, u32>>,
    next_request_id: AtomicU64,
    frame_count: AtomicU64,
    fixture_epoch: AtomicU64,
    pub last_action_frame: AtomicU64,
    verbose_logging: AtomicBool,
    pub fixtures: FixtureManager,
    pub diagnostics: DiagnosticRegistry,
    pub idle: IdleRegistry,
    runtime_hooks: Mutex<Option<Arc<dyn RuntimeHooks>>>,
    shutdown_handler: Mutex<Option<AppShutdownHandler>>,
    automation_options: Mutex<AutomationOptions>,
}

impl Default for Inner {
    fn default() -> Self {
        Self::new()
    }
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

#[derive(Debug, Clone)]
struct WidgetValueUpdate {
    value: WidgetValue,
    queued_frame: u64,
    consumer_seen: bool,
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
    pub const UNCONSUMED_OVERRIDE_FRAME_GRACE: u64 = 2;

    pub fn new() -> Self {
        Self {
            actions: ActionQueue::new(),
            viewports: ViewportState::new(),
            widgets: WidgetRegistry::new(),
            overlays: OverlayManager::new(),
            contexts: Mutex::new(HashMap::new()),
            animation_baselines: Mutex::new(HashMap::new()),
            widget_value_updates: Mutex::new(HashMap::new()),
            widget_value_consumers: Mutex::new(HashSet::new()),
            scroll_overrides: Mutex::new(HashMap::new()),
            frame_fixture_epochs: Mutex::new(HashMap::new()),
            frame_depth: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            frame_count: AtomicU64::new(0),
            fixture_epoch: AtomicU64::new(0),
            last_action_frame: AtomicU64::new(0),
            verbose_logging: AtomicBool::new(false),
            fixtures: FixtureManager::new(),
            diagnostics: DiagnosticRegistry::new(),
            idle: IdleRegistry::new(),
            runtime_hooks: Mutex::new(None),
            shutdown_handler: Mutex::new(None),
            automation_options: Mutex::new(AutomationOptions::default()),
        }
    }

    pub fn set_runtime_hooks(&self, hooks: Arc<dyn RuntimeHooks>) {
        *lock(&self.runtime_hooks, "runtime hooks lock") = Some(hooks);
    }

    pub fn runtime_hooks(&self) -> Option<Arc<dyn RuntimeHooks>> {
        lock(&self.runtime_hooks, "runtime hooks lock").clone()
    }

    pub fn set_shutdown_handler(&self, handler: Option<AppShutdownHandler>) {
        *lock(&self.shutdown_handler, "shutdown handler lock") = handler;
    }

    pub fn shutdown_handler(&self) -> Option<AppShutdownHandler> {
        lock(&self.shutdown_handler, "shutdown handler lock").clone()
    }

    pub fn set_automation_options(&self, options: AutomationOptions) {
        *lock(&self.automation_options, "automation options lock") = options;
        self.apply_automation_options_to_stored_contexts(options);
    }

    pub fn automation_options(&self) -> AutomationOptions {
        *lock(&self.automation_options, "automation options lock")
    }

    /// Start applying a validated fixture by calling the registered handler.
    pub fn start_fixture(&self, call: FixtureCall) -> FixtureExecution {
        self.fixtures.start_fixture(call)
    }

    pub fn dismiss_transient_ui(&self, viewport_id: Option<egui::ViewportId>) {
        if let Some(viewport_id) = viewport_id {
            self.actions.clear_viewport(viewport_id);
            lock(&self.widget_value_updates, "widget value update lock")
                .retain(|key, _| key.viewport_id != viewport_id);
            lock(&self.widget_value_consumers, "widget value consumers lock")
                .retain(|key| key.viewport_id != viewport_id);
            lock(&self.scroll_overrides, "scroll overrides lock")
                .retain(|key, _| key.viewport_id != viewport_id);
            self.overlays.clear_viewport_overlays(viewport_id);
            self.overlays.clear_overlay_debug_config(viewport_id);
            // Context clones share memory: direct popup/focus mutations cannot
            // select a viewport. Escape runs in the target's next input pass.
            for pressed in [true, false] {
                self.queue_action(
                    viewport_id,
                    InputAction::Key {
                        key: egui::Key::Escape,
                        pressed,
                        modifiers: Modifiers::default(),
                    },
                );
            }
            return;
        }
        self.actions.clear_all();
        lock(&self.widget_value_updates, "widget value update lock").clear();
        lock(&self.widget_value_consumers, "widget value consumers lock").clear();
        lock(&self.scroll_overrides, "scroll overrides lock").clear();
        self.overlays.clear_transient_state();
        let contexts = {
            let contexts = lock(&self.contexts, "contexts lock");
            contexts.values().cloned().collect::<Vec<_>>()
        };
        for ctx in &contexts {
            egui::Popup::close_all(ctx);
            ctx.memory_mut(|memory| memory.stop_text_input());
        }
        self.request_repaint_all();
    }

    pub fn set_verbose_logging(&self, verbose_logging: bool) {
        self.verbose_logging
            .store(verbose_logging, Ordering::Relaxed);
    }

    pub fn verbose_logging(&self) -> bool {
        self.verbose_logging.load(Ordering::Relaxed)
    }

    pub fn capture_context(&self, viewport_id: egui::ViewportId, ctx: &Context) {
        self.apply_automation_options_to_context(viewport_id, ctx, self.automation_options());
        self.remember_context(viewport_id, ctx);
    }

    pub fn remember_context(&self, viewport_id: egui::ViewportId, ctx: &Context) {
        let mut stored = lock(&self.contexts, "contexts lock");
        stored.insert(viewport_id, ctx.clone());
        self.viewports.remember_viewport_id(viewport_id);
    }

    fn apply_automation_options_to_stored_contexts(&self, options: AutomationOptions) {
        let contexts = {
            let contexts = lock(&self.contexts, "contexts lock");
            contexts
                .iter()
                .map(|(viewport_id, ctx)| (*viewport_id, ctx.clone()))
                .collect::<Vec<_>>()
        };
        for (viewport_id, ctx) in contexts {
            self.apply_automation_options_to_context(viewport_id, &ctx, options);
        }
    }

    fn apply_automation_options_to_context(
        &self,
        viewport_id: egui::ViewportId,
        ctx: &Context,
        options: AutomationOptions,
    ) {
        let target_animation_time = if options.animations {
            lock(&self.animation_baselines, "animation baselines lock")
                .get(&viewport_id)
                .copied()
        } else {
            let current_animation_time = ctx.global_style().animation_time;
            lock(&self.animation_baselines, "animation baselines lock")
                .entry(viewport_id)
                .or_insert(current_animation_time);
            Some(0.0)
        };
        if let Some(animation_time) = target_animation_time {
            ctx.global_style_mut(|style| {
                style.animation_time = animation_time;
            });
        }
    }

    pub fn context_for(&self, viewport_id: egui::ViewportId) -> Option<Context> {
        let contexts = lock(&self.contexts, "contexts lock");
        contexts.get(&viewport_id).cloned()
    }

    pub fn has_context(&self) -> bool {
        !lock(&self.contexts, "contexts lock").is_empty()
    }

    pub fn request_repaint(&self) {
        self.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub fn request_repaint_all(&self) {
        let contexts = {
            let contexts = lock(&self.contexts, "contexts lock");
            contexts
                .iter()
                .map(|(viewport_id, ctx)| (*viewport_id, ctx.clone()))
                .collect::<Vec<_>>()
        };
        for (viewport_id, ctx) in contexts {
            ctx.request_repaint_of(viewport_id);
        }
    }

    pub fn request_repaint_of(&self, viewport_id: egui::ViewportId) {
        let ctx = {
            let contexts = lock(&self.contexts, "contexts lock");
            contexts.get(&viewport_id).cloned()
        };
        if let Some(ctx) = ctx {
            ctx.request_repaint_of(viewport_id);
        }
    }

    pub fn queue_widget_value_update(
        &self,
        viewport_id: egui::ViewportId,
        id: String,
        value: WidgetValue,
    ) {
        let queued_frame = self
            .viewports
            .capture_snapshot(viewport_id)
            .map(|snapshot| snapshot.frame_count)
            .unwrap_or_else(|| self.frame_count());
        let key = WidgetValueKey::new(viewport_id, id);
        let consumer_seen =
            lock(&self.widget_value_consumers, "widget value consumers lock").contains(&key);
        let mut updates = lock(&self.widget_value_updates, "widget value update lock");
        updates.insert(
            key,
            WidgetValueUpdate {
                value,
                queued_frame,
                consumer_seen,
            },
        );
        self.request_repaint_of(viewport_id);
    }

    pub fn mark_widget_value_consumer(&self, viewport_id: egui::ViewportId, id: &str) {
        lock(&self.widget_value_consumers, "widget value consumers lock")
            .insert(WidgetValueKey::new(viewport_id, id));
    }

    pub fn take_widget_value_update(
        &self,
        viewport_id: egui::ViewportId,
        id: &str,
    ) -> Option<WidgetValue> {
        let mut updates = lock(&self.widget_value_updates, "widget value update lock");
        updates
            .remove(&WidgetValueKey::new(viewport_id, id))
            .map(|update| update.value)
    }

    pub fn clear_widget_value_update_if_matches(
        &self,
        viewport_id: egui::ViewportId,
        id: &str,
        value: &WidgetValue,
    ) {
        let key = WidgetValueKey::new(viewport_id, id);
        let mut updates = lock(&self.widget_value_updates, "widget value update lock");
        if updates
            .get(&key)
            .is_some_and(|update| &update.value == value)
        {
            updates.remove(&key);
        }
    }

    pub fn expired_widget_value_update_error(
        &self,
        viewport_id: egui::ViewportId,
        widget_id: Option<&str>,
    ) -> Option<ToolError> {
        let current_frame = self
            .viewports
            .capture_snapshot(viewport_id)
            .map(|snapshot| snapshot.frame_count)
            .unwrap_or_else(|| self.frame_count());
        let expired = {
            let updates = lock(&self.widget_value_updates, "widget value update lock");
            updates
                .iter()
                .filter(|(key, _)| key.viewport_id == viewport_id)
                .filter(|(key, _)| widget_id.is_none_or(|widget_id| key.id == widget_id))
                .find_map(|(key, update)| {
                    let age = current_frame.saturating_sub(update.queued_frame);
                    (age >= Self::UNCONSUMED_OVERRIDE_FRAME_GRACE && !update.consumer_seen)
                        .then(|| (key.id.clone(), update.queued_frame, age))
                })
        }?;
        let (id, queued_frame, age) = expired;
        let viewport = self
            .viewports
            .viewports_snapshot()
            .into_iter()
            .find(|snapshot| snapshot.viewport_id == viewport_id_to_string(viewport_id));
        let viewport_label = viewport
            .as_ref()
            .and_then(|snapshot| snapshot.name.as_deref())
            .unwrap_or_else(|| {
                if viewport_id == egui::ViewportId::ROOT {
                    "root"
                } else {
                    "unnamed viewport"
                }
            });
        Some(
            ToolError::new(
                ErrorCode::InstrumentationFault,
                format!(
                    "Widget value override for {id:?} in {viewport_label} was not consumed; \
                     call take_widget_value_override before rendering the custom widget"
                ),
            )
            .with_details(json!({
                "reason": "override_not_consumed",
                "viewport": viewport.map(|snapshot| {
                    json!({
                        "id": snapshot.viewport_id,
                        "name": snapshot.name,
                        "title": snapshot.title,
                    })
                }).unwrap_or_else(|| {
                    json!({
                        "id": viewport_id_to_string(viewport_id),
                        "name": null,
                        "title": null,
                    })
                }),
                "widget_id": id,
                "queued_frame": queued_frame,
                "current_frame": current_frame,
                "captures_since_queue": age,
                "grace_captures": Self::UNCONSUMED_OVERRIDE_FRAME_GRACE,
                "consumer": "take_widget_value_override",
            })),
        )
    }

    pub fn set_overlay_debug_config(
        &self,
        viewport_id: egui::ViewportId,
        config: OverlayDebugConfig,
    ) {
        self.overlays.set_overlay_debug_config(viewport_id, config);
        self.request_repaint_of(viewport_id);
    }

    pub fn clear_overlay_debug_config(&self, viewport_id: egui::ViewportId) {
        self.overlays.clear_overlay_debug_config(viewport_id);
        self.request_repaint_of(viewport_id);
    }

    pub fn set_overlay(&self, viewport_id: egui::ViewportId, key: String, overlay: OverlayEntry) {
        self.overlays.set_overlay(viewport_id, key, overlay);
        self.request_repaint_of(viewport_id);
    }

    pub fn remove_overlay(&self, viewport_id: egui::ViewportId, key: &str) {
        self.overlays.remove_overlay(viewport_id, key);
        self.request_repaint_of(viewport_id);
    }

    pub fn clear_viewport_overlays(&self, viewport_id: egui::ViewportId) {
        self.overlays.clear_viewport_overlays(viewport_id);
        self.request_repaint_of(viewport_id);
    }

    pub fn clear_overlays(&self) {
        self.overlays.clear_overlays();
        self.request_repaint_all();
    }

    pub fn paint_overlays(&self, ctx: &Context) {
        self.overlays
            .paint_overlays(ctx, &self.widgets, &self.viewports);
    }

    pub fn set_scroll_override(
        &self,
        viewport_id: egui::ViewportId,
        widget_id: u64,
        offset: egui::Vec2,
    ) {
        let mut overrides = lock(&self.scroll_overrides, "scroll overrides lock");
        overrides.insert(ScrollAreaKey::new(viewport_id, widget_id), offset);
        self.request_repaint_of(viewport_id);
    }

    pub fn take_scroll_override(
        &self,
        viewport_id: egui::ViewportId,
        widget_id: u64,
    ) -> Option<egui::Vec2> {
        let mut overrides = lock(&self.scroll_overrides, "scroll overrides lock");
        overrides.remove(&ScrollAreaKey::new(viewport_id, widget_id))
    }

    pub fn queue_action(&self, viewport_id: egui::ViewportId, action: InputAction) {
        self.queue_action_with_timing(viewport_id, ActionTiming::Immediate, action);
    }

    pub fn queue_action_with_timing(
        &self,
        viewport_id: egui::ViewportId,
        timing: ActionTiming,
        action: InputAction,
    ) {
        self.actions
            .record_pointer_queued(viewport_id, self.frame_count(), &action);
        self.actions
            .queue_action_with_timing(viewport_id, timing, action);
        self.request_repaint_of(viewport_id);
    }

    pub fn queue_command(&self, viewport_id: egui::ViewportId, command: egui::ViewportCommand) {
        self.actions.queue_command(viewport_id, command);
        self.request_repaint_of(viewport_id);
    }

    pub fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn clear_all(&self) {
        lock(&self.widget_value_updates, "widget values lock").clear();
        lock(&self.widget_value_consumers, "widget value consumers lock").clear();
        lock(&self.scroll_overrides, "scroll overrides lock").clear();
        self.actions.clear_all();
    }

    pub fn begin_frame(&self, viewport_id: egui::ViewportId) {
        let epoch = self.fixture_epoch();
        lock(&self.frame_fixture_epochs, "frame fixture epochs lock").insert(viewport_id, epoch);
    }

    /// Enter a viewport frame. Returns true when this is the outermost begin.
    pub fn enter_viewport_frame(&self, viewport_id: egui::ViewportId) -> bool {
        let mut depths = lock(&self.frame_depth, "frame depth lock");
        let depth = depths.entry(viewport_id).or_insert(0);
        *depth = depth.saturating_add(1);
        *depth == 1
    }

    /// Exit a viewport frame. Returns true when this is the matching outermost
    /// end.
    pub fn exit_viewport_frame(&self, viewport_id: egui::ViewportId) -> bool {
        let mut depths = lock(&self.frame_depth, "frame depth lock");
        let Some(depth) = depths.get_mut(&viewport_id) else {
            return true;
        };
        *depth = depth.saturating_sub(1);
        if *depth == 0 {
            depths.remove(&viewport_id);
            true
        } else {
            false
        }
    }

    pub fn finish_frame_fixture_epoch(&self, viewport_id: egui::ViewportId) -> Option<u64> {
        lock(&self.frame_fixture_epochs, "frame fixture epochs lock").remove(&viewport_id)
    }

    /// Count one settled automation frame. Discarded layout passes do not
    /// publish automation state or increment this count.
    pub fn advance_frame(&self) {
        self.frame_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    /// Drop heavy per-viewport maps for non-root viewports that left the live
    /// set.
    pub fn forget_dead_viewports(&self) {
        let Some(live) = self.viewports.live_viewport_ids() else {
            return;
        };
        let is_live = |viewport_id: &egui::ViewportId| {
            *viewport_id == egui::ViewportId::ROOT || live.contains(viewport_id)
        };
        lock(&self.contexts, "contexts lock").retain(|viewport_id, _| is_live(viewport_id));
        lock(&self.animation_baselines, "animation baselines lock")
            .retain(|viewport_id, _| is_live(viewport_id));
    }

    pub fn frame_health(&self, viewport_id: egui::ViewportId) -> Option<FrameHealth> {
        self.viewports.frame_health(viewport_id)
    }

    pub fn begin_fixture_epoch(&self) -> u64 {
        self.fixture_epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn fixture_epoch(&self) -> u64 {
        self.fixture_epoch.load(Ordering::Relaxed)
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
    use crate::types::Pos2;

    #[test]
    fn request_repaint_targets_viewport_without_holding_contexts_lock() {
        let inner = Arc::new(new_test_inner());
        let ctx = Context::default();
        let viewport_id = egui::ViewportId::from_hash_of("secondary");
        inner.capture_context(viewport_id, &ctx);

        let inner_for_callback = Arc::clone(&inner);
        let (sender, receiver) = mpsc::channel();
        ctx.set_request_repaint_callback(move |info| {
            assert!(inner_for_callback.context_for(viewport_id).is_some());
            sender
                .send(info.viewport_id)
                .expect("notify repaint callback");
        });

        inner.request_repaint_of(viewport_id);
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("repaint callback"),
            viewport_id
        );
    }

    #[test]
    fn overlay_changes_repaint_their_viewport() {
        let viewport_id = egui::ViewportId::from_hash_of("secondary");
        let operations: [fn(&Inner, egui::ViewportId); 5] = [
            |inner, viewport_id| {
                inner.set_overlay(
                    viewport_id,
                    "test".into(),
                    OverlayEntry {
                        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
                        color: egui::Color32::RED,
                        stroke_width: 1.0,
                    },
                )
            },
            |inner, viewport_id| inner.remove_overlay(viewport_id, "test"),
            Inner::clear_viewport_overlays,
            |inner, viewport_id| {
                inner.set_overlay_debug_config(
                    viewport_id,
                    OverlayDebugConfig {
                        enabled: true,
                        ..Default::default()
                    },
                )
            },
            Inner::clear_overlay_debug_config,
        ];
        for operation in operations {
            let inner = new_test_inner();
            let ctx = Context::default();
            inner.capture_context(viewport_id, &ctx);
            let (sender, receiver) = mpsc::channel();
            ctx.set_request_repaint_callback(move |info| {
                sender
                    .send(info.viewport_id)
                    .expect("notify repaint callback");
            });
            operation(&inner, viewport_id);
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("overlay repaint"),
                viewport_id
            );
        }
    }

    #[test]
    fn global_overlay_resets_repaint_all_viewports() {
        let secondary = egui::ViewportId::from_hash_of("secondary");
        let operations: [fn(&Inner); 2] = [Inner::clear_overlays, |inner| {
            inner.dismiss_transient_ui(None)
        }];
        for operation in operations {
            let inner = new_test_inner();
            let (sender, receiver) = mpsc::channel();
            for viewport_id in [egui::ViewportId::ROOT, secondary] {
                let ctx = Context::default();
                inner.capture_context(viewport_id, &ctx);
                let sender = sender.clone();
                ctx.set_request_repaint_callback(move |info| {
                    sender
                        .send(info.viewport_id)
                        .expect("notify repaint callback");
                });
            }
            operation(&inner);
            let first = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("first repaint");
            let second = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("second repaint");
            assert_ne!(first, second);
            assert!([first, second].contains(&egui::ViewportId::ROOT));
            assert!([first, second].contains(&secondary));
        }
    }

    #[test]
    fn automation_options_apply_animation_policy_to_contexts() {
        let inner = Arc::new(new_test_inner());
        let ctx = Context::default();
        ctx.global_style_mut(|style| {
            style.animation_time = 0.25;
        });

        inner.capture_context(egui::ViewportId::ROOT, &ctx);
        assert_eq!(ctx.global_style().animation_time, 0.0);

        inner.set_automation_options(AutomationOptions {
            keep_alive: true,
            animations: true,
        });
        assert_eq!(ctx.global_style().animation_time, 0.25);
    }

    #[test]
    fn clear_all_resets_widget_value_consumer_cache() {
        let inner = new_test_inner();
        let viewport_id = egui::ViewportId::ROOT;

        inner.mark_widget_value_consumer(viewport_id, "custom.value");
        inner.clear_all();
        inner.queue_widget_value_update(
            viewport_id,
            "custom.value".to_string(),
            WidgetValue::Int(7),
        );
        inner.advance_frame();
        inner.advance_frame();

        let error = inner
            .expired_widget_value_update_error(viewport_id, Some("custom.value"))
            .expect("unwired widget should fault after reset");
        assert_eq!(error.code(), ErrorCode::InstrumentationFault);
    }

    #[test]
    fn dismiss_transient_ui_preserves_other_viewport_state() {
        let inner = new_test_inner();
        let root = egui::ViewportId::ROOT;
        let secondary = egui::ViewportId::from_hash_of("secondary");
        let pos = Pos2 { x: 5.0, y: 6.0 };
        for viewport_id in [root, secondary] {
            inner.queue_action(viewport_id, InputAction::PointerMove { pos });
            assert_eq!(inner.actions.drain_actions(viewport_id, 1).len(), 1);
            inner.queue_action_with_timing(
                viewport_id,
                ActionTiming::AfterTwoFrames,
                InputAction::Text {
                    text: "pending".to_string(),
                },
            );
            inner.queue_command(viewport_id, egui::ViewportCommand::Title("pending".into()));
            inner.mark_widget_value_consumer(viewport_id, "field");
            inner.queue_widget_value_update(viewport_id, "field".into(), WidgetValue::Int(7));
            inner.set_scroll_override(viewport_id, 1, egui::vec2(3.0, 4.0));
            inner.set_overlay_debug_config(
                viewport_id,
                OverlayDebugConfig {
                    enabled: true,
                    ..Default::default()
                },
            );
        }

        inner.dismiss_transient_ui(Some(root));

        assert_eq!(inner.actions.pending_action_count(root), 2);
        assert!(!inner.actions.has_pending_commands(root));
        assert_eq!(inner.actions.stats(root).queued_actions, 2);
        assert_eq!(inner.actions.stats(root).last_drain_frame, None);
        assert!(inner.actions.pointer_pos(root).is_none());
        assert_eq!(inner.actions.pending_action_count(secondary), 1);
        assert_eq!(inner.actions.pending_command_count(secondary), 1);
        assert_eq!(inner.actions.stats(secondary).queued_actions, 2);
        assert_eq!(inner.actions.stats(secondary).last_drain_frame, Some(1));
        assert_eq!(inner.actions.pointer_pos(secondary), Some(pos));
        // Reports only enter the trace when recent consumed pointer state
        // remains. The cleared viewport must not leave either kind of trace.
        for viewport_id in [root, secondary] {
            inner
                .actions
                .record_pointer_report(viewport_id, 2, Some(pos));
        }
        let trace = inner.actions.pointer_trace();
        let events = trace["events"].as_array().expect("pointer events");
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| event["viewport_id"] == viewport_id_to_string(secondary))
        );

        assert!(inner.take_widget_value_update(root, "field").is_none());
        assert!(matches!(
            inner.take_widget_value_update(secondary, "field"),
            Some(WidgetValue::Int(7))
        ));
        assert!(inner.take_scroll_override(root, 1).is_none());
        assert_eq!(
            inner.take_scroll_override(secondary, 1),
            Some(egui::vec2(3.0, 4.0))
        );
        assert!(!inner.overlays.overlay_debug_config(root).enabled);
        assert!(inner.overlays.overlay_debug_config(secondary).enabled);
    }

    #[test]
    fn dismiss_transient_ui_preserves_other_viewport_consumers() {
        let inner = new_test_inner();
        let root = egui::ViewportId::ROOT;
        let secondary = egui::ViewportId::from_hash_of("secondary");
        for viewport_id in [root, secondary] {
            inner.mark_widget_value_consumer(viewport_id, "field");
        }
        inner.dismiss_transient_ui(Some(root));
        // Only the cleared viewport must rediscover the custom widget's
        // value-override consumer before a later update can be admitted.
        for viewport_id in [root, secondary] {
            inner.queue_widget_value_update(viewport_id, "field".into(), WidgetValue::Int(9));
        }
        for _ in 0..Inner::UNCONSUMED_OVERRIDE_FRAME_GRACE {
            inner.advance_frame();
        }
        assert!(
            inner
                .expired_widget_value_update_error(root, Some("field"))
                .is_some()
        );
        assert!(
            inner
                .expired_widget_value_update_error(secondary, Some("field"))
                .is_none()
        );
    }

    #[test]
    fn dismiss_transient_ui_resets_widget_value_consumer_cache() {
        let inner = new_test_inner();
        let viewport_id = egui::ViewportId::ROOT;

        inner.mark_widget_value_consumer(viewport_id, "custom.value");
        inner.dismiss_transient_ui(Some(viewport_id));
        inner.queue_widget_value_update(
            viewport_id,
            "custom.value".to_string(),
            WidgetValue::Int(7),
        );
        inner.advance_frame();
        inner.advance_frame();

        let error = inner
            .expired_widget_value_update_error(viewport_id, Some("custom.value"))
            .expect("unwired widget should fault after popup dismissal");
        assert_eq!(error.code(), ErrorCode::InstrumentationFault);
    }

    fn new_test_inner() -> Inner {
        Inner::new()
    }
}
