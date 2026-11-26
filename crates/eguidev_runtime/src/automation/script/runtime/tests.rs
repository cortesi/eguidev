//! Tests of the script runtime.

use std::{
    future::poll_fn,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};

use serde_json::json;
use tokio::task::yield_now;

use super::{
    MAX_SAMPLE_GRID_COUNT, ScriptRuntime,
    captures::{changed_widget_fields, parse_sample_grid_count},
};
use crate::{
    EguiDiagnosticBatch, EguiDiagnosticKind,
    automation::script::types::ScriptPosition,
    dump::{DumpOptions, build_tree_dump},
    registry::{Inner, viewport_id_to_string},
    runtime::{Runtime, attach_for_tests},
    types::{Pos2, Rect, WidgetRegistryEntry, WidgetRole, WidgetState, WidgetValue},
};

fn assert_send_sync<T: Send + Sync>() {}

fn state(value: Option<WidgetValue>) -> WidgetState {
    let rect = Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 { x: 10.0, y: 10.0 },
    };
    WidgetState::from(&WidgetRegistryEntry {
        id: "slider".to_string(),
        explicit_id: true,
        native_id: 1,
        viewport_id: "root".to_string(),
        layer_id: egui::LayerId::background(),
        layer_order: 0,
        rect,
        interact_rect: rect,
        role: WidgetRole::Slider,
        label: None,
        value,
        data: None,
        layout: None,
        role_state: None,
        parent_id: None,
        enabled: true,
        visible: true,
        focused: false,
        covered: false,
    })
}

fn id_clash_output() -> egui::FullOutput {
    let ctx = egui::Context::default();
    ctx.options_mut(|options| options.warn_on_id_clash = true);
    // egui only carries rectangle-identity warnings in debug builds, so
    // this isolation is both unavailable and unnecessary in release.
    #[cfg(debug_assertions)]
    ctx.all_styles_mut(|style| style.debug.warn_if_rect_changes_id = false);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let id = egui::Id::unique("duplicate");
        ui.ctx().check_for_id_clash(
            id,
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(20.0, 20.0)),
            "test widget",
        );
        ui.ctx().check_for_id_clash(
            id,
            egui::Rect::from_min_size(egui::pos2(60.0, 10.0), egui::vec2(20.0, 20.0)),
            "test widget",
        );
    });
    output.textures_delta.clear();
    output
}

fn script_runtime(timeout_ms: u64) -> (Arc<Runtime>, Arc<ScriptRuntime>) {
    let inner = Arc::new(Inner::new());
    inner.remember_context(egui::ViewportId::ROOT, &egui::Context::default());
    let runtime = Runtime::ensure_for_inner_with_diagnostic_barriers(&inner);
    let script = Arc::new(ScriptRuntime::new(
        inner,
        Arc::clone(&runtime),
        "diagnostics.luau".to_string(),
        timeout_ms,
    ));
    (runtime, script)
}

#[test]
fn screenshot_dimensions_require_whole_pixels() {
    let (_runtime, script) = script_runtime(1_000);
    let pos = ScriptPosition::default();
    for value in [0.4, 1.5, 1600.5] {
        let error = script
            .screenshot_options(pos, Some(&json!({ "max_dimension": value })))
            .expect_err("fractional dimensions must be rejected");
        assert!(error.message.contains("non-negative integer"));
    }
    for value in [0_u32, 1, 1600, u32::MAX] {
        let options = script
            .screenshot_options(pos, Some(&json!({ "max_dimension": value })))
            .expect("whole pixel dimension");
        assert_eq!(options.max_dimension, value);
    }
}

#[test]
fn closed_viewport_is_removed_from_dumps_and_capture_diffs() {
    let (_runtime, script) = script_runtime(1_000);
    let inner = &script.server.inner;
    let ctx = egui::Context::default();
    let secondary = egui::ViewportId::from_hash_of("capture.closed.secondary");
    let secondary_id = viewport_id_to_string(secondary);
    let mut open = egui::RawInput::default();
    open.viewports.insert(secondary, Default::default());
    ctx.run_ui(open.clone(), |_| {})
        .drop_without_applying_deltas();
    inner.viewports.update_viewports(&ctx);
    let pos = ScriptPosition::default();
    let before = script.capture();

    ctx.run_ui(egui::RawInput::default(), |_| {})
        .drop_without_applying_deltas();
    inner.viewports.update_viewports(&ctx);
    let dump = build_tree_dump(inner, &DumpOptions::default()).expect("dump after close");
    assert_eq!(dump.viewports.len(), 1);
    assert_eq!(dump.viewports[0].id, "root");
    let diff = script.capture_diff(pos, &before, None).expect("close diff");
    assert_eq!(diff["viewports_removed"], json!([secondary_id]));

    let closed = script.capture();
    ctx.run_ui(open, |_| {}).drop_without_applying_deltas();
    inner.viewports.update_viewports(&ctx);
    let diff = script
        .capture_diff(pos, &closed, None)
        .expect("reopen diff");
    assert_eq!(diff["viewports_added"], json!([secondary_id]));
    assert_eq!(
        build_tree_dump(inner, &DumpOptions::default())
            .expect("reopened dump")
            .viewports
            .len(),
        2
    );
}

#[test]
fn parse_sample_grid_count_rejects_axis_over_cap() {
    assert!(parse_sample_grid_count(&json!(0), "nx").is_err());
    assert!(parse_sample_grid_count(&json!(MAX_SAMPLE_GRID_COUNT + 1), "nx").is_err());
    assert_eq!(
        parse_sample_grid_count(&json!(MAX_SAMPLE_GRID_COUNT), "nx").expect("max"),
        MAX_SAMPLE_GRID_COUNT as usize
    );
}

#[test]
fn script_runtime_is_send_sync() {
    assert_send_sync::<ScriptRuntime>();
}

/// Build a UI provider whose invocation is observable without running an
/// app.
fn queued_diagnostic_script(timeout_ms: u64) -> (ScriptRuntime, Arc<AtomicBool>) {
    let called = Arc::new(AtomicBool::new(false));
    let recorded = Arc::clone(&called);
    let devmcp = attach_for_tests(
        eguidev::DevMcp::new()
            .diagnostic_ui("queued", move |_ctx| {
                recorded.store(true, Ordering::SeqCst);
                Ok(json!(true))
            })
            .expect("provider"),
    );
    let inner = devmcp.inner_arc().expect("attached runtime");
    let runtime = Runtime::ensure_for_inner(&inner);
    let script = ScriptRuntime::new(inner, runtime, "queued.luau".to_string(), timeout_ms);
    (script, called)
}

#[tokio::test]
async fn cancelled_ui_diagnostic_future_does_not_run_the_provider_later() {
    let (script, called) = queued_diagnostic_script(5_000);
    let mut diagnostic = Box::pin(script.run_diagnostic(ScriptPosition::default(), "queued"));
    poll_fn(|cx| {
        assert!(diagnostic.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(diagnostic);

    script
        .server
        .inner
        .diagnostics
        .drain_ui(&egui::Context::default());

    assert!(
        !called.load(Ordering::SeqCst),
        "cancelled diagnostic must not call the provider"
    );
}

#[tokio::test]
async fn timed_out_ui_diagnostic_future_preserves_error_and_cancels_request() {
    let (script, called) = queued_diagnostic_script(1_000);
    let error = script
        .run_diagnostic(ScriptPosition::default(), "queued")
        .await
        .expect_err("UI did not drain the request");
    assert_eq!(error.code, "timeout");
    assert_eq!(error.message, "diagnostic provider \"queued\" timed out");

    script
        .server
        .inner
        .diagnostics
        .drain_ui(&egui::Context::default());
    assert!(!called.load(Ordering::SeqCst));
}

#[test]
fn evaluations_start_at_independent_journal_tails() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let first = ScriptRuntime::new(
        Arc::clone(&inner),
        Arc::clone(&runtime),
        "first.luau".to_string(),
        1_000,
    );
    runtime
        .egui_diagnostics()
        .record_output("root".to_string(), 1, &id_clash_output());
    let second = ScriptRuntime::new(
        inner,
        Arc::clone(&runtime),
        "second.luau".to_string(),
        1_000,
    );
    runtime
        .egui_diagnostics()
        .record_output("root".to_string(), 2, &id_clash_output());

    assert!(
        first.diagnostic_selection(None).batch.entries.len()
            > second.diagnostic_selection(None).batch.entries.len()
    );
    assert!(
        second
            .diagnostic_selection(None)
            .batch
            .entries
            .iter()
            .all(|entry| entry.frame == 2)
    );
}

#[tokio::test]
async fn viewport_read_waits_for_output_and_dismisses_returned_entries() {
    let (runtime, script) = script_runtime(1_000);
    runtime
        .egui_diagnostics()
        .record_output("root".to_string(), 1, &id_clash_output());

    let read_script = Arc::clone(&script);
    let read = tokio::spawn(async move {
        read_script
            .egui_diagnostics(ScriptPosition::default(), "root".to_string())
            .await
    });
    yield_now().await;
    runtime
        .egui_diagnostics()
        .record_output("root".to_string(), 2, &egui::FullOutput::default());

    let value = read.await.expect("read task").expect("diagnostic read");
    let batch: EguiDiagnosticBatch = serde_json::from_value(value).expect("diagnostic batch");
    assert!(
        batch
            .entries
            .iter()
            .all(|entry| entry.kind == EguiDiagnosticKind::IdClash)
    );
    assert!(script.diagnostic_selection(None).batch.is_empty());
}

#[tokio::test]
async fn viewport_clear_dismisses_without_returning_entries() {
    let (runtime, script) = script_runtime(1_000);
    runtime
        .egui_diagnostics()
        .record_output("root".to_string(), 1, &id_clash_output());

    let clear_script = Arc::clone(&script);
    let clear = tokio::spawn(async move {
        clear_script
            .clear_egui_diagnostics(ScriptPosition::default(), "root".to_string())
            .await
    });
    yield_now().await;
    runtime
        .egui_diagnostics()
        .record_output("root".to_string(), 2, &egui::FullOutput::default());

    clear.await.expect("clear task").expect("diagnostic clear");
    assert!(script.diagnostic_selection(None).batch.is_empty());
}

#[test]
fn diff_ignores_the_int_float_round_trip_a_capture_takes_through_luau() {
    // Luau has one number type, so a captured Float(42.0) returns as
    // Int(42).
    let before = state(Some(WidgetValue::Int(42)));
    let after = state(Some(WidgetValue::Float(42.0)));
    assert!(changed_widget_fields(&before, &after, 0.5).is_empty());
}

#[test]
fn diff_still_reports_a_real_value_change() {
    let before = state(Some(WidgetValue::Float(42.0)));
    let after = state(Some(WidgetValue::Float(43.0)));
    assert_eq!(
        changed_widget_fields(&before, &after, 0.5),
        vec!["value", "value_text"]
    );

    let cleared = state(None);
    assert_eq!(
        changed_widget_fields(&before, &cleared, 0.5),
        vec!["value", "value_text"]
    );
}
