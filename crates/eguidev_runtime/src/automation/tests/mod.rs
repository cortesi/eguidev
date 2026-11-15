//! Tests of the automation operations.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::Duration,
};

use eguidev::{
    DevMcp, FixtureCall, FixtureError, FixtureResponse, FixtureResult, FixtureSpec, FrameGuard,
};
use serde_json::{Value, json};
use tmcp::schema::ContentBlock;
use tokio::{task::yield_now, time::sleep};

use super::*;
use crate::{
    actions::InputAction,
    automation::types::LayoutIssueKind,
    fixtures::FixtureHandler,
    mcp::AppMcpServer,
    overlay::{OverlayDebugConfig, OverlayDebugMode},
    registry::{Inner, viewport_id_to_string},
    runtime::{Runtime, attach_for_tests},
    types::{
        Modifiers, Pos2, Rect, RoleState, Vec2, WidgetLayout, WidgetRange, WidgetRef,
        WidgetRegistryEntry, WidgetRole, WidgetRoleMeta, WidgetValue,
    },
    viewports::InputSnapshot,
    widget_registry::{WidgetMeta, record_widget},
};

mod action;
mod capture;
mod fixture;
mod layout;
mod query;
mod script;
mod wait;

fn discard_output(output: egui::FullOutput) {
    output.drop_without_applying_deltas();
}

fn set_runtime_fixture_handler<F>(inner: &Inner, handler: F)
where
    F: Fn(&FixtureCall) -> FixtureResult + Send + Sync + 'static,
{
    inner
        .fixtures
        .set_handler(FixtureHandler::Runtime(Arc::new(handler)))
        .expect("fixture handler");
}

fn fixture_ok() -> FixtureResult {
    Ok(FixtureResponse::new())
}

/// Drain queued actions into one synthetic frame's raw input.
///
/// A staged action promotes only when the frame counter moved since the
/// last drain, which a real application advances for each completed
/// pass. A test that drives `Context::run_ui` directly must do the
/// same, or an `AfterOneFrame` action never reaches the application.
fn apply_actions(inner: &Inner, raw_input: &mut egui::RawInput) {
    inner.advance_frame();
    let viewport_id = raw_input.viewport_id;
    let actions = inner
        .actions
        .drain_actions(viewport_id, inner.frame_count());
    let base_modifiers = raw_input
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            egui::Event::ModifiersChanged(modifiers) => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_default();
    let mut current_modifiers = base_modifiers;
    let mut modifiers_changed = false;
    let mut force_focus = false;
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
    if modifiers_changed {
        raw_input
            .events
            .push(egui::Event::ModifiersChanged(current_modifiers));
    }
}

fn widget_ref_id(id: &str) -> WidgetRef {
    WidgetRef {
        id: id.to_string(),
        viewport_id: None,
    }
}

fn listed_widgets(
    inner: &Inner,
    include_invisible: Option<bool>,
    role: Option<WidgetRole>,
    id_prefix: Option<&str>,
    label: Option<&str>,
    label_contains: Option<&str>,
) -> Vec<WidgetRegistryEntry> {
    collect_widget_list(
        inner,
        None,
        include_invisible,
        role,
        id_prefix,
        label,
        label_contains,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("widget list")
}

fn make_entry(id: &str, native_id: u64, role: WidgetRole) -> WidgetRegistryEntry {
    make_entry_with_rect(
        id,
        native_id,
        role,
        Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        },
        None,
    )
}

fn make_generated_entry(native_id: u64, role: WidgetRole) -> WidgetRegistryEntry {
    let mut entry = make_entry(&format!("{native_id:x}"), native_id, role);
    entry.explicit_id = false;
    entry
}

fn make_entry_with_rect(
    id: &str,
    native_id: u64,
    role: WidgetRole,
    rect: Rect,
    parent_id: Option<&str>,
) -> WidgetRegistryEntry {
    WidgetRegistryEntry {
        id: id.to_string(),
        explicit_id: true,
        native_id,
        viewport_id: "root".to_string(),
        layer_id: egui::LayerId::background(),
        layer_order: 0,
        rect,
        interact_rect: rect,
        role,
        label: None,
        value: None,
        data: None,
        layout: None,
        role_state: None,
        parent_id: parent_id.map(str::to_string),
        enabled: true,
        visible: true,
        focused: false,
        covered: false,
    }
}

fn make_scroll_entry(
    id: &str,
    native_id: u64,
    offset: Vec2,
    max_offset: Vec2,
) -> WidgetRegistryEntry {
    let mut entry = make_entry(id, native_id, WidgetRole::ScrollArea);
    entry.role_state = Some(RoleState::ScrollArea {
        offset,
        viewport_size: Vec2 { x: 100.0, y: 100.0 },
        content_size: Vec2 {
            x: 100.0 + max_offset.x,
            y: 100.0 + max_offset.y,
        },
    });
    entry
}

fn capture_test_frame(inner: &Arc<Inner>, ctx: &egui::Context) {
    inner.capture_context(ctx.viewport_id(), ctx);
    inner
        .viewports
        .capture_input_snapshot(ctx, inner.fixture_epoch(), inner.frame_count() + 1);
    inner.advance_frame();
    Runtime::ensure_for_inner(inner)
        .frame_notify()
        .notify_waiters();
}

fn drain_test_actions(inner: &Arc<Inner>, viewport_id: egui::ViewportId) {
    let actions = inner
        .actions
        .drain_actions(viewport_id, inner.frame_count());
    if !actions.is_empty() {
        inner
            .last_action_frame
            .store(inner.frame_count(), AtomicOrdering::Relaxed);
    }
}

fn record_test_snapshot(inner: &Arc<Inner>, viewport_id: egui::ViewportId) {
    record_test_pointer_snapshot(inner, viewport_id, None);
}

fn record_test_pointer_snapshot(
    inner: &Arc<Inner>,
    viewport_id: egui::ViewportId,
    pointer_pos: Option<Pos2>,
) {
    inner.viewports.record_input_snapshot(
        viewport_id,
        InputSnapshot {
            pixels_per_point: 1.0,
            pointer_pos,
        },
        inner.fixture_epoch(),
        inner.frame_count() + 1,
    );
    inner.advance_frame();
    Runtime::ensure_for_inner(inner)
        .frame_notify()
        .notify_waiters();
}

fn run_instrumented_test_frame(
    devmcp: &DevMcp,
    ctx: &egui::Context,
    viewport_id: egui::ViewportId,
    live_viewports: &[egui::ViewportId],
) {
    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    for live_viewport in live_viewports {
        raw_input
            .viewports
            .insert(*live_viewport, Default::default());
    }
    discard_output(ctx.run_ui(raw_input, |ctx| {
        let _guard = FrameGuard::new(devmcp, ctx);
    }));
}

fn viewport_snapshot_with_inner_size(inner_size: Vec2) -> ViewportSnapshot {
    ViewportSnapshot {
        viewport_id: viewport_id_to_string(egui::ViewportId::ROOT),
        name: None,
        inner_size,
        outer_pos: None,
        outer_size: Some(inner_size),
        pixels_per_point: 1.0,
        focused: true,
        title: None,
        parent_viewport_id: None,
        minimized: Some(false),
        occluded: Some(false),
        os_minimized: None,
        os_occluded: None,
        os_title_visible: None,
        maximized: Some(false),
        fullscreen: Some(false),
    }
}

fn parse_script_eval_json(result: &CallToolResult) -> Value {
    let content = result.content.first().expect("content");
    match content {
        ContentBlock::Text(text) => serde_json::from_str(&text.text).expect("script eval json"),
        other => panic!("expected text content, got {other:?}"),
    }
}
