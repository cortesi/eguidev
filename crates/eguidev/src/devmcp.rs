//! DevMCP instrumentation helpers and public API.

use std::sync::{Arc, atomic::Ordering};
#[cfg(feature = "devtools")]
use std::time::Duration;

use egui::Context;
#[cfg(feature = "devtools")]
use tokio::runtime::Handle;

#[cfg(feature = "devtools")]
use crate::tools::{
    DEFAULT_POLL_INTERVAL_MS, DEFAULT_SCRIPT_EVAL_TIMEOUT_MS, ScriptErrorInfo, ScriptEvalOptions,
    ScriptEvalOutcome, script::run_script_eval,
};
use crate::{
    actions::InputAction,
    fixtures::FixtureRequest,
    instrument::{ACTIVE, swallow_panic},
    registry::Inner,
    types::FixtureSpec,
};

#[derive(Clone, Debug, Default)]
enum DevMcpState {
    #[default]
    Inactive,
    Active(Arc<Inner>),
}

/// DevMCP handle stored in app state.
#[derive(Clone, Debug, Default)]
pub struct DevMcp {
    state: DevMcpState,
    fixtures: Vec<FixtureSpec>,
    verbose_logging: bool,
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

    /// Register fixture metadata for discovery and validation.
    pub fn fixtures(mut self, fixtures: impl IntoIterator<Item = FixtureSpec>) -> Self {
        self.fixtures = fixtures.into_iter().collect();
        if let Some(inner) = self.inner() {
            inner.fixtures.set_fixtures(self.fixtures.clone());
        }
        self
    }

    /// Collect pending fixture requests.
    pub fn collect_fixture_requests(&self) -> Vec<FixtureRequest> {
        self.inner()
            .map_or_else(Vec::new, |inner| inner.fixtures.collect_fixture_requests())
    }

    /// Returns true if DevMCP automation is attached.
    pub fn is_enabled(&self) -> bool {
        matches!(self.state, DevMcpState::Active(_))
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

    pub(crate) fn activate(mut self, inner: Arc<Inner>) -> Self {
        inner.set_verbose_logging(self.verbose_logging);
        if !self.fixtures.is_empty() {
            inner.fixtures.set_fixtures(self.fixtures.clone());
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
        swallow_panic("begin_frame", || {
            let viewport_id = ctx.viewport_id();
            inner.capture_context(viewport_id, ctx);
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
        inner.widgets.finalize_registry(ctx.viewport_id());
        inner.viewports.capture_input_snapshot(ctx);
        self.finish_frame_runtime(inner, ctx);
    }

    #[cfg(feature = "devtools")]
    fn finish_frame_runtime(&self, inner: &Arc<Inner>, ctx: &Context) {
        ctx.input(|i| inner.capture_screenshot_events(&i.raw.events));
        let mut sent_viewport_command = false;
        for (viewport_id, commands) in inner.actions.drain_all_commands() {
            for command in commands {
                sent_viewport_command = true;
                if let egui::ViewportCommand::Screenshot(user_data) = &command {
                    let request_id = user_data
                        .data
                        .as_ref()
                        .and_then(|data| data.downcast_ref::<u64>())
                        .copied();
                    inner.record_screenshot_command_sent(viewport_id, request_id);
                }
                ctx.send_viewport_cmd_to(viewport_id, command);
            }
        }
        if sent_viewport_command {
            ctx.request_repaint();
        }
        inner.viewports.update_viewports(ctx);
        inner.paint_overlays(ctx);
        inner.notify_frame_end();
        // Keep the runtime alive so queued automation work can advance.
        ctx.request_repaint_after(Duration::from_millis(DEFAULT_POLL_INTERVAL_MS));
    }

    #[cfg(not(feature = "devtools"))]
    fn finish_frame_runtime(&self, inner: &Arc<Inner>, ctx: &Context) {
        let _ = inner;
        let _ = ctx;
    }

    /// Inject queued raw input during the raw input hook.
    pub(crate) fn raw_input_hook(&self, ctx: &Context, raw_input: &mut egui::RawInput) {
        let Some(inner) = self.inner() else {
            return;
        };
        swallow_panic("raw_input_hook", || {
            let viewport_id = raw_input.viewport_id;
            inner.capture_context(viewport_id, ctx);
            #[cfg(feature = "devtools")]
            inner.capture_screenshot_events(&raw_input.events);
            let actions = inner.actions.drain_actions(viewport_id);
            if !actions.is_empty() {
                inner
                    .last_action_frame
                    .store(inner.frame_count(), Ordering::Relaxed);
                if self.verbose_logging_enabled() {
                    eprintln!(
                        "eguidev: raw_input_hook viewport={:?} actions={}",
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
        });
    }

    /// Evaluate a Luau script directly against this attached `DevMcp` instance.
    #[cfg(feature = "devtools")]
    pub fn eval_script(
        &self,
        handle: Handle,
        script_source: &str,
        timeout_ms: Option<u64>,
        options: ScriptEvalOptions,
    ) -> ScriptEvalOutcome {
        let Some(inner) = self.inner() else {
            return ScriptEvalOutcome::error_only(ScriptErrorInfo {
                error_type: "runtime".to_string(),
                message: "DevMCP runtime is not attached".to_string(),
                location: None,
                backtrace: None,
                code: None,
                details: None,
            });
        };
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_SCRIPT_EVAL_TIMEOUT_MS);
        let source_name = options
            .source_name
            .unwrap_or_else(|| "script.luau".to_string());
        run_script_eval(
            Arc::clone(inner),
            handle,
            script_source,
            timeout_ms,
            source_name,
            options.args,
        )
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

/// Forward the raw input hook into the DevMcp handler.
pub fn raw_input_hook(devmcp: &DevMcp, ctx: &Context, raw_input: &mut egui::RawInput) {
    devmcp.raw_input_hook(ctx, raw_input);
}

#[cfg(test)]
#[allow(deprecated)]
#[allow(clippy::tests_outside_test_module)]
mod inactive_tests {
    use egui::Context;

    use super::*;
    use crate::{instrument, ui_ext::DevUiExt};

    #[test]
    fn inactive_raw_input_hook_is_a_noop() {
        let devmcp = DevMcp::new();
        let ctx = Context::default();
        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            focused: false,
            ..Default::default()
        };

        devmcp.raw_input_hook(&ctx, &mut raw_input);

        assert!(!raw_input.focused);
        assert!(raw_input.events.is_empty());
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

#[cfg(all(test, feature = "devtools"))]
#[allow(deprecated)]
#[allow(clippy::tests_outside_test_module)]
pub mod tests {
    use std::{
        cell::Cell,
        sync::{Arc, Mutex},
    };

    use egui::{Area, Rect, pos2, vec2};
    use serde_json::json;
    use tokio::runtime::Builder;

    use super::*;
    use crate::{
        actions::InputAction,
        runtime,
        types::{Modifiers, Pos2},
        ui_ext::DevUiExt,
    };

    pub fn devmcp_enabled() -> DevMcp {
        runtime::attach_for_tests(DevMcp::new())
    }

    fn inner(devmcp: &DevMcp) -> &Arc<Inner> {
        devmcp.inner().expect("attached inner")
    }

    #[test]
    fn raw_input_hook_forces_focus_for_key_actions() {
        let devmcp = devmcp_enabled();
        let ctx = Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::Key {
                key: egui::Key::A,
                pressed: true,
                modifiers: Modifiers::default(),
            },
        );

        let mut raw_input = egui::RawInput {
            viewport_id,
            focused: false,
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);

        assert!(raw_input.focused);
        assert!(raw_input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::A,
                    pressed: true,
                    ..
                }
            )
        }));
    }

    #[test]
    fn raw_input_hook_injects_key_text_without_window_focus() {
        let devmcp = devmcp_enabled();
        let ctx = Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::Key {
                key: egui::Key::A,
                pressed: true,
                modifiers: Modifiers::default(),
            },
        );
        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::Text {
                text: "a".to_string(),
            },
        );

        let mut raw_input = egui::RawInput {
            viewport_id,
            focused: false,
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);

        let saw_key = Cell::new(false);
        let saw_text = Cell::new(false);
        let _output = ctx.run_ui(raw_input, |ui| {
            ui.ctx().input(|input| {
                for event in &input.events {
                    match event {
                        egui::Event::Key {
                            key: egui::Key::A,
                            pressed: true,
                            ..
                        } => saw_key.set(true),
                        egui::Event::Text(text) if text == "a" => saw_text.set(true),
                        _ => {}
                    }
                }
            });
        });

        assert!(saw_key.get());
        assert!(saw_text.get());
    }

    fn render_click_target(ui: &egui::Ui, clicked: Option<&Cell<bool>>) {
        Area::new("click_target_area".into())
            .fixed_pos(pos2(40.0, 30.0))
            .show(ui.ctx(), |ui| {
                let response = ui.dev_button("click.target", "Click");
                if response.clicked()
                    && let Some(clicked) = clicked
                {
                    clicked.set(true);
                }
            });
    }

    #[test]
    fn injected_click_triggers_button_response() {
        let devmcp = devmcp_enabled();
        let ctx = Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let screen_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(240.0, 160.0));

        let raw_input = egui::RawInput {
            viewport_id,
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _output = ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();
            devmcp.begin_frame(&ctx);
            render_click_target(ui, None);
            devmcp.end_frame(&ctx);
        });

        let raw_input = egui::RawInput {
            viewport_id,
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _output = ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();
            devmcp.begin_frame(&ctx);
            render_click_target(ui, None);
            devmcp.end_frame(&ctx);
        });

        let widgets = inner(&devmcp).widgets.widget_list(viewport_id);
        let widget = widgets
            .iter()
            .find(|entry| entry.id == "click.target")
            .unwrap_or_else(|| panic!("missing widget click.target"));
        assert!(widget.visible);
        let click_pos = widget.interact_rect.center();
        inner(&devmcp).queue_action(viewport_id, InputAction::PointerMove { pos: click_pos });
        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            },
        );
        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            },
        );

        let mut raw_input = egui::RawInput {
            viewport_id,
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);

        let clicked = Cell::new(false);
        let _output = ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();
            devmcp.begin_frame(&ctx);
            render_click_target(ui, Some(&clicked));
            devmcp.end_frame(&ctx);
        });

        assert!(clicked.get());
    }

    #[test]
    fn end_frame_drains_pending_commands() {
        let devmcp = devmcp_enabled();
        let ctx = Context::default();
        let raw_input = egui::RawInput::default();
        let _output = ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();
            devmcp.begin_frame(&ctx);
            inner(&devmcp).queue_command(ctx.viewport_id(), egui::ViewportCommand::Focus);
            devmcp.end_frame(&ctx);
        });

        let pending = inner(&devmcp).actions.drain_all_commands();
        assert!(pending.is_empty());
    }

    #[test]
    fn end_frame_records_screenshot_command() {
        let devmcp = devmcp_enabled();
        let ctx = Context::default();
        let raw_input = egui::RawInput::default();
        let request_id = 55_u64;
        let _output = ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();
            devmcp.begin_frame(&ctx);
            inner(&devmcp).queue_command(
                ctx.viewport_id(),
                egui::ViewportCommand::Screenshot(egui::UserData::new(request_id)),
            );
            devmcp.end_frame(&ctx);
        });

        let snapshot = inner(&devmcp).screenshot_debug_snapshot();
        assert_eq!(snapshot.debug.commands_sent, 1);
        let last_command = snapshot.debug.last_command.expect("last command");
        assert_eq!(last_command.request_id, Some(request_id));
    }

    #[test]
    fn raw_input_hook_does_not_drain_commands() {
        let devmcp = devmcp_enabled();
        let viewport_id = egui::ViewportId::ROOT;
        inner(&devmcp).queue_command(viewport_id, egui::ViewportCommand::Focus);
        let ctx = Context::default();
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);

        let pending = inner(&devmcp).actions.drain_all_commands();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, viewport_id);
        assert_eq!(pending[0].1.len(), 1);
    }

    #[test]
    fn action_queue_applies_to_raw_input() {
        let devmcp = devmcp_enabled();
        let viewport_id = egui::ViewportId::ROOT;
        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::PointerMove {
                pos: Pos2 { x: 10.0, y: 20.0 },
            },
        );
        let ctx = Context::default();
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);
        assert!(
            raw_input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::PointerMoved(_)))
        );
    }

    #[test]
    fn queued_actions_request_repaint_for_the_target_viewport() {
        let devmcp = devmcp_enabled();
        let repaint_requests = Arc::new(Mutex::new(Vec::new()));

        let root_ctx = Context::default();
        let requests_for_root = Arc::clone(&repaint_requests);
        root_ctx.set_request_repaint_callback(move |_| {
            requests_for_root
                .lock()
                .expect("repaint requests lock")
                .push("root");
        });
        inner(&devmcp).capture_context(egui::ViewportId::ROOT, &root_ctx);

        let secondary_ctx = Context::default();
        let viewport_id = egui::ViewportId::from_hash_of("secondary");
        let requests_for_secondary = Arc::clone(&repaint_requests);
        secondary_ctx.set_request_repaint_callback(move |_| {
            requests_for_secondary
                .lock()
                .expect("repaint requests lock")
                .push("secondary");
        });
        inner(&devmcp).capture_context(viewport_id, &secondary_ctx);

        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::PointerMove {
                pos: Pos2 { x: 10.0, y: 20.0 },
            },
        );

        let repaint_requests = repaint_requests.lock().expect("repaint requests lock");
        assert_eq!(*repaint_requests, vec!["secondary"]);
    }

    #[test]
    fn action_modifiers_update_raw_input_modifiers() {
        let devmcp = devmcp_enabled();
        let viewport_id = egui::ViewportId::ROOT;
        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::Key {
                key: egui::Key::H,
                pressed: true,
                modifiers: Modifiers {
                    command: true,
                    ..Default::default()
                },
            },
        );
        let ctx = Context::default();
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);
        assert!(raw_input.modifiers.command);
        assert!(raw_input.modifiers.mac_cmd);
    }

    #[test]
    fn key_release_restores_base_modifiers() {
        let devmcp = devmcp_enabled();
        let ctx = Context::default();
        let viewport_id = egui::ViewportId::ROOT;

        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::Key {
                key: egui::Key::A,
                pressed: true,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            },
        );
        inner(&devmcp).queue_action(
            viewport_id,
            InputAction::Key {
                key: egui::Key::A,
                pressed: false,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            },
        );

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        devmcp.raw_input_hook(&ctx, &mut raw_input);

        assert!(!raw_input.modifiers.ctrl);
        assert!(raw_input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::A,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.ctrl
            )
        }));
        assert!(raw_input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::A,
                    pressed: false,
                    modifiers,
                    ..
                } if modifiers.ctrl
            )
        }));
    }

    #[test]
    fn eval_script_returns_value_and_logs() {
        let devmcp = devmcp_enabled();
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = devmcp.eval_script(
            runtime.handle().clone(),
            "log(\"hello\")\nreturn 1 + 1",
            None,
            crate::ScriptEvalOptions::default(),
        );

        assert!(result.success);
        assert_eq!(result.value, Some(json!(2)));
        assert_eq!(result.logs, vec!["hello".to_string()]);
        assert!(result.error.is_none());
    }

    #[test]
    fn eval_script_reports_parse_errors() {
        let devmcp = devmcp_enabled();
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = devmcp.eval_script(
            runtime.handle().clone(),
            "local x =",
            None,
            crate::ScriptEvalOptions::default(),
        );

        assert!(!result.success);
        assert_eq!(
            result.error.as_ref().map(|error| error.error_type.as_str()),
            Some("parse")
        );
    }

    #[test]
    fn eval_script_reports_assertion_failures() {
        let devmcp = devmcp_enabled();
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = devmcp.eval_script(
            runtime.handle().clone(),
            "assert(false, \"nope\")",
            None,
            crate::ScriptEvalOptions::default(),
        );

        assert!(!result.success);
        assert_eq!(
            result.error.as_ref().map(|error| error.error_type.as_str()),
            Some("assertion")
        );
        assert_eq!(result.assertions.len(), 1);
        assert!(!result.assertions[0].passed);
    }
}
