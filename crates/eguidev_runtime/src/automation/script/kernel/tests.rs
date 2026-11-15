//! Tests of the script kernel.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use egui::{Color32, Context, RawInput, ViewportId, epaint::Shape};
use eguidev::AutomationOptions;
use ruau::vm::{Ambient, Function, Limits, RuntimeCapabilities, Vm, serde::json_to_scoped_value};
use serde_json::json;
use tokio::runtime::Builder as TokioRuntimeBuilder;

#[test]
fn script_instruction_limit_uses_explicit_override() {
    assert_eq!(base_limits(Some(25_000_000)).gas, Some(25_000_000));
    assert_eq!(
        base_limits(None).gas,
        Some(super::super::DEFAULT_SCRIPT_MAX_INSTRUCTIONS)
    );
}

#[test]
fn script_instruction_limit_stops_runaway_work() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking_with_modules(
        inner,
        runtime,
        "local total = 0\nfor index = 1, 100000 do total += index end\nreturn total".to_string(),
        1_000,
        ScriptEvalOptions {
            source_name: Some("instruction-limit.luau".to_string()),
            max_instructions: Some(1_000),
            ..ScriptEvalOptions::default()
        },
    );

    assert!(!outcome.success, "{outcome:?}");
    assert!(
        outcome
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("instruction budget exhausted")),
        "{outcome:?}"
    );
}

use super::{
    EguidevModule, base_limits, run_script_eval_blocking, run_script_eval_blocking_with_modules,
};
use crate::{
    DevMcp,
    automation::script::types::{ScriptArgValue, ScriptArgs, ScriptEvalOptions},
    fixtures::FixtureHandler,
    registry::{Inner, viewport_id_to_string},
    runtime::{self, Runtime},
    types::{
        FixtureParam, FixtureResponse, FixtureSpec, Pos2, Rect, RoleState, Vec2,
        WidgetRegistryEntry, WidgetRole, WidgetRoleMeta, WidgetValue,
    },
};

#[test]
fn native_kernel_surface_matches_the_seven_private_capabilities_exactly() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let script_runtime = Arc::new(super::ScriptRuntime::new(
        inner,
        runtime,
        "kernel-surface.luau".to_string(),
        1_000,
    ));
    let (_, surface) = EguidevModule {
        args: serde_json::Value::Object(serde_json::Map::new()),
        runtime: script_runtime,
        declaration: super::library::DECLARATION.to_string(),
    }
    .build();

    let expected = BTreeMap::from([
        (
            "eguidev.action".to_string(),
            BTreeSet::from(
                [
                    "viewport_clear_debug_overlay",
                    "viewport_clear_highlights",
                    "viewport_dismiss_popups",
                    "viewport_focus",
                    "viewport_input",
                    "viewport_key",
                    "viewport_paste",
                    "viewport_resize",
                    "viewport_show_debug_overlay",
                    "viewport_show_highlight",
                    "widget_clear_debug_overlay",
                    "widget_clear_highlight",
                    "widget_click",
                    "widget_drag_position",
                    "widget_drag_relative",
                    "widget_drag_to",
                    "widget_focus",
                    "widget_hover",
                    "widget_scroll",
                    "widget_scroll_into_view",
                    "widget_scroll_to",
                    "widget_set_value",
                    "widget_show_debug_overlay",
                    "widget_show_highlight",
                    "widget_type_text",
                ]
                .map(str::to_string),
            ),
        ),
        (
            "eguidev.capture".to_string(),
            BTreeSet::from(
                [
                    "diff",
                    "viewport_layout_issues",
                    "viewport_sample_pixels",
                    "viewport_screenshot",
                    "viewport_native_screenshot",
                    "widget_layout_issues",
                    "widget_sample_grid",
                    "widget_sample_pixels",
                    "widget_screenshot",
                    "widget_text_measure",
                ]
                .map(str::to_string),
            ),
        ),
        (
            "eguidev.diagnostic".to_string(),
            BTreeSet::from(["all", "clear_egui", "egui", "get"].map(str::to_string)),
        ),
        (
            "eguidev.fixture".to_string(),
            BTreeSet::from(["apply", "list"].map(str::to_string)),
        ),
        (
            "eguidev.query".to_string(),
            BTreeSet::from(
                [
                    "capture",
                    "dump",
                    "dump_text",
                    "viewport_list",
                    "viewport_state",
                    "viewport_widget_at_point",
                    "viewport_widget_list",
                    "widget_children",
                    "widget_parent",
                    "widget_state",
                    "widget_viewport",
                ]
                .map(str::to_string),
            ),
        ),
        (
            "eguidev.record".to_string(),
            BTreeSet::from(["args", "array", "assertion", "configure", "log"].map(str::to_string)),
        ),
        (
            "eguidev.wait".to_string(),
            BTreeSet::from(
                [
                    "capture",
                    "frames",
                    "viewport_settle",
                    "viewport_wait",
                    "widget_wait",
                    "widget_wait_absent",
                ]
                .map(str::to_string),
            ),
        ),
    ]);

    assert!(
        surface.globals.is_empty(),
        "unexpected globals: {surface:?}"
    );
    assert_eq!(surface.methods, expected);
}

#[test]
fn initial_ruau_slice_runs_value_and_log_script() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.log("hello")
return 1 + 1"#
            .to_string(),
        1_000,
        "probe.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(2)));
    assert_eq!(outcome.logs, vec!["hello"]);
}

#[test]
fn typecheck_failure_prevents_tenant_execution() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.log("must not execute")
local state: WidgetState = eguidev.widget("missing"):state()
return state"#
            .to_string(),
        1_000,
        "typecheck_side_effect.luau".to_string(),
        ScriptArgs::default(),
    );

    assert!(!outcome.success, "{outcome:?}");
    assert!(outcome.logs.is_empty(), "{outcome:?}");
    assert_eq!(
        outcome
            .error
            .as_ref()
            .map(|error| error.error_type.as_str()),
        Some("typecheck")
    );
    let location = outcome
        .error
        .as_ref()
        .and_then(|error| error.location.as_ref())
        .expect("typecheck location");
    assert_eq!(location.line, 2);
    assert!(location.column.is_some());
}

#[test]
fn initial_ruau_slice_runs_diagnostics_and_wait_until() {
    let devmcp = runtime::attach_for_tests(
        DevMcp::new()
            .diagnostic("ready", || Ok(json!({ "ready": true, "count": 2 })))
            .expect("diagnostic"),
    );
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    let outcome = runtime.block_on(runtime::eval_script(
        &devmcp,
        r#"eguidev.wait(function()
return eguidev.diagnostic("ready").ready
end)
return eguidev.diagnostics()"#,
        Some(1_000),
        crate::ScriptEvalOptions {
            source_name: Some("diagnostics.luau".to_string()),
            args: ScriptArgs::default(),
            ..crate::ScriptEvalOptions::default()
        },
    ));

    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "values": {
                "eguidev.input": {
                    "positions": {},
                    "events": [],
                },
                "ready": {
                    "ready": true,
                    "count": 2,
                },
            },
            "errors": {},
        }))
    );
}

#[test]
fn initial_ruau_slice_wait_until_respects_configured_timeout() {
    let devmcp = runtime::attach_for_tests(DevMcp::new());
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    let outcome = runtime.block_on(runtime::eval_script(
        &devmcp,
        r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 })
eguidev.wait(function()
return false
end)
"#,
        Some(1_000),
        crate::ScriptEvalOptions {
            source_name: Some("wait-until-timeout.luau".to_string()),
            args: ScriptArgs::default(),
            ..crate::ScriptEvalOptions::default()
        },
    ));

    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.as_ref().expect("timeout error");
    assert_eq!(error.error_type, "timeout");
    assert_eq!(error.code.as_deref(), Some("timeout"));
    assert!(
        error
            .message
            .contains("Timed out waiting for a fresh capture"),
        "{error:?}"
    );
    assert!(
        outcome.timing.exec_ms < 500,
        "wait_until should honor the configured timeout: {outcome:?}"
    );
}

#[test]
fn initial_ruau_slice_collects_diagnostic_errors() {
    let devmcp = runtime::attach_for_tests(
        DevMcp::new()
            .diagnostic("broken", || {
                Err(eguidev::DiagnosticError::new("broken", "diagnostic failed")
                    .with_details(json!({ "reason": "test" })))
            })
            .expect("diagnostic"),
    );
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    let outcome = runtime.block_on(runtime::eval_script(
        &devmcp,
        r#"return eguidev.diagnostics()"#,
        Some(1_000),
        crate::ScriptEvalOptions {
            source_name: Some("diagnostics.luau".to_string()),
            args: ScriptArgs::default(),
            ..crate::ScriptEvalOptions::default()
        },
    ));

    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "values": {
                "eguidev.input": {
                    "positions": {},
                    "events": [],
                },
            },
            "errors": {
                "broken": {
                    "code": "broken",
                    "message": "diagnostic failed",
                    "details": {
                        "reason": "test",
                    },
                },
            },
        }))
    );
}

#[test]
fn initial_ruau_slice_records_assertion_failures() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("missing"):expect(
{ present = true },
{ timeout_ms = 10, poll_interval_ms = 1 }
)"#
        .to_string(),
        1_000,
        "probe.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.expect("assertion error");
    assert_eq!(error.error_type, "eguidev");
    assert_eq!(error.code.as_deref(), Some("expectation_failed"));
    assert!(error.message.contains("expectation failed"), "{error:?}");
    assert_eq!(outcome.assertions.len(), 1);
    assert!(!outcome.assertions[0].passed);
    assert!(outcome.assertions[0].message.contains("expectation failed"));
}

/// Build a registry that holds one covered button in the root viewport.
fn covered_button_inner() -> Arc<Inner> {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("covered.button", 1, WidgetRole::Button);
    entry.covered = true;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);
    inner
}

#[test]
fn timed_out_click_names_its_widget() {
    let inner = covered_button_inner();
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("covered.button"):click({ timeout_ms = 20, poll_interval_ms = 1 })"#
            .to_string(),
        1_000,
        "click.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.expect("click error");
    assert_eq!(error.code.as_deref(), Some("timeout"));
    assert!(
        error
            .message
            .contains("waiting for widget covered.button in viewport root to become"),
        "{error:?}"
    );
    assert_eq!(
        error.details.as_ref().map(|details| &details["target"]),
        Some(&json!({ "kind": "widget", "id": "covered.button", "viewport_id": "root" })),
        "{error:?}"
    );
}

#[test]
fn focus_waits_until_the_widget_is_actionable() {
    // A covered widget is present, visible, and enabled, but a click to
    // focus it would reach the covering layer.
    let inner = covered_button_inner();
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("covered.button"):focus({ timeout_ms = 20, poll_interval_ms = 1 })"#
            .to_string(),
        1_000,
        "focus.luau".to_string(),
        ScriptArgs::default(),
    );
    let error = outcome.error.expect("focus error");
    assert_eq!(error.code.as_deref(), Some("timeout"), "{error:?}");
    assert!(
        error
            .message
            .contains("to become interaction-ready and uncovered"),
        "{error:?}"
    );
}

#[test]
fn rust_errors_carry_target_and_details_tables() {
    let inner = covered_button_inner();
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local ok, err = pcall(function()
eguidev.widget("covered.button"):click({ timeout_ms = 20, poll_interval_ms = 1 })
end)
local failure = err :: any
return {
ok = ok,
code = failure.code,
target = failure.target,
details = type(failure.details),
elapsed = type(failure.details.elapsed_ms),
}"#
        .to_string(),
        1_000,
        "click-pcall.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "ok": false,
            "code": "timeout",
            "target": { "kind": "widget", "id": "covered.button", "viewport_id": "root" },
            "details": "table",
            "elapsed": "number",
        }))
    );
}

#[test]
fn initial_ruau_slice_runs_assert_widget_exists() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local state = eguidev.widget("status"):expect({ present = true })
return state ~= nil"#
            .to_string(),
        1_000,
        "assert-widget.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(true)));
    assert_eq!(outcome.assertions.len(), 1);
    assert!(outcome.assertions[0].passed);
    assert!(outcome.assertions[0].message.contains("expectation"));
}

#[test]
fn script_eval_widget_predicate_receives_nil_when_absent() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.configure({ timeout_ms = 50, poll_interval_ms = 1 })
local seen_nil = false
local state = eguidev.widget("missing"):wait(function(current)
if current == nil then
    seen_nil = true
    return true
end
return false
end)
return { seen_nil = seen_nil, state_nil = state == nil }"#
            .to_string(),
        1_000,
        "absent-predicate.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({ "seen_nil": true, "state_nil": true }))
    );
}

#[test]
fn script_eval_expect_text_fits_on_absent_widget_records_failure() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("missing"):expect(
{ present = false, text_fits = true },
{ timeout_ms = 20, poll_interval_ms = 1 }
)"#
        .to_string(),
        1_000,
        "expect-absent-text-fits.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.expect("expectation error");
    assert_eq!(error.error_type, "eguidev");
    assert_eq!(error.code.as_deref(), Some("expectation_failed"));
    assert_eq!(outcome.assertions.len(), 1);
    assert!(!outcome.assertions[0].passed);
    assert!(outcome.assertions[0].message.contains("expectation failed"));
}

#[test]
fn script_eval_data_condition_matches_tables_and_zero_based_array_index() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.data = Some(json!({
        "palette": { "name": "analysis", "rgba": [28, 34, 44] },
        "pass": "analysis"
    }));
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.configure({ timeout_ms = 50, poll_interval_ms = 1 })
local table_match = eguidev.widget("status"):wait({
data = { pointer = "/palette", equals = { name = "analysis", rgba = { 28, 34, 44 } } },
})
local zero = eguidev.widget("status"):wait({
data = { pointer = "/palette/rgba/0", equals = 28 },
})
local one = eguidev.widget("status"):wait({
data = { pointer = "/palette/rgba/1", equals = 34 },
})
return { table_match = table_match ~= nil, zero = zero ~= nil, one = one ~= nil }"#
            .to_string(),
        1_000,
        "data-table-pointer.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({ "table_match": true, "zero": true, "one": true }))
    );
}

#[test]
fn initial_ruau_slice_runs_configure_fixture_and_fixtures() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("zeta", "Z fixture.")
            .ready("status")
            .param(
                FixtureParam::text("mode", "Selection mode.")
                    .default("fast")
                    .choices(["fast", "slow"]),
            ),
        FixtureSpec::new("alpha", "A fixture.").ready("status"),
    ]);
    let applied = Arc::new(AtomicBool::new(false));
    let applied_c = Arc::clone(&applied);
    inner
        .fixtures
        .set_handler(FixtureHandler::Runtime(Arc::new(move |call| {
            assert_eq!(call.name, "zeta");
            assert_eq!(call.params.text("mode"), "slow");
            applied_c.store(true, Ordering::SeqCst);
            Ok(FixtureResponse::new())
        })))
        .expect("fixture handler");

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        Arc::clone(&inner),
        runtime,
        r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1, settle = false, animations = true })
	eguidev.fixture("zeta", { mode = "slow" }, { wait = false })
	local frame = eguidev.wait_frames(0)
	local catalog = eguidev.fixtures()
	eguidev.log(catalog[1].name)
	return { first = catalog[1].name, count = #catalog, frame = frame, params = catalog[2].params[1].name }"#
            .to_string(),
        1_000,
        "fixtures.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert!(applied.load(Ordering::SeqCst));
    assert_eq!(outcome.logs, vec!["alpha"]);
    assert_eq!(outcome.fixtures.len(), 1);
    assert_eq!(outcome.fixtures[0].name, "zeta");
    assert_eq!(
        outcome.fixtures[0].params.get("mode"),
        Some(&WidgetValue::Text("slow".to_string()))
    );
    assert_eq!(
        outcome.value,
        Some(json!({ "first": "alpha", "count": 2, "frame": 0, "params": "mode" }))
    );
    assert!(!inner.automation_options().animations);
}

#[test]
fn configure_animations_does_not_leak_across_evaluations() {
    let inner = Arc::new(Inner::new());
    inner.set_automation_options(AutomationOptions {
        keep_alive: true,
        animations: false,
    });
    let runtime = Runtime::ensure_for_inner(&inner);
    let first = run_script_eval_blocking(
        Arc::clone(&inner),
        Arc::clone(&runtime),
        r#"eguidev.configure({ animations = true })
return true"#
            .to_string(),
        1_000,
        "configure-animations-first.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(first.success, "{first:?}");
    assert!(!inner.automation_options().animations);

    let second = run_script_eval_blocking(
        Arc::clone(&inner),
        runtime,
        r#"eguidev.configure({ animations = true })
return true"#
            .to_string(),
        1_000,
        "configure-animations-second.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(second.success, "{second:?}");
    assert!(!inner.automation_options().animations);
}

#[test]
fn initial_ruau_slice_runs_root_widget_list() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
    inner
        .widgets
        .record_widget(viewport_id, make_entry("other", 2, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local widgets = eguidev.root:widgets({ id_prefix = "status" })
local state = widgets[1]:state()
assert(state ~= nil)
return { count = #widgets, id = widgets[1].id, viewport = state.viewport_id }"#
            .to_string(),
        1_000,
        "root-widget-list.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({ "count": 1, "id": "status", "viewport": "root" }))
    );
}

#[test]
fn initial_ruau_slice_runs_widget_handle_reads() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut root = make_entry("panel", 1, WidgetRole::Window);
    root.label = Some("Panel".to_string());
    inner.widgets.record_widget(viewport_id, root);
    let mut child = make_entry("status", 2, WidgetRole::Button);
    child.parent_id = Some("panel".to_string());
    child.label = Some("Ready".to_string());
    inner.widgets.record_widget(viewport_id, child);
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local viewport = eguidev.root
local widget = eguidev.widget("status")
local state = widget:state()
local parent = widget:parent()
local hits = viewport:widgets_at({ x = 1, y = 1 })
assert(state ~= nil and parent ~= nil)
local parent_state = parent:state()
assert(state.parent_id == "panel")
assert(#state.child_ids == 0)
assert(parent_state ~= nil and parent_state.child_ids[1] == "status")
local waited = parent:wait({ present = true })
assert(waited ~= nil and waited.child_ids[1] == "status", "a wait returns the children")
return {
role = state.role,
label = state.label,
parent_id = parent and parent.id or "",
sibling_count = #parent:children(),
hit_count = #hits,
top_hit = hits[1].id,
}"#
        .to_string(),
        1_000,
        "widget-reads.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "role": "button",
            "label": "Ready",
            "parent_id": "panel",
            "sibling_count": 1,
            "hit_count": 2,
            "top_hit": "status",
        }))
    );
}

#[test]
fn initial_ruau_slice_runs_widget_actions() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("button", 1, WidgetRole::Button));
    inner
        .widgets
        .record_widget(viewport_id, make_entry("checkbox", 2, WidgetRole::Checkbox));
    // No frame runs, so the input holds the focus that type_text confirms.
    let mut input = make_entry("input", 3, WidgetRole::TextEdit);
    input.focused = true;
    inner.widgets.record_widget(viewport_id, input);
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        Arc::clone(&inner),
        runtime,
        r#"eguidev.configure({ settle = false })
local button = eguidev.widget("button")
local checkbox = eguidev.widget("checkbox")
local input = eguidev.widget("input")
button:click({ settle = false, click_count = 2 })
button:hover({ settle = false })
button:drag_relative(
{ x = 0.8, y = 0.5 },
{ from = { x = 0.2, y = 0.5 }, settle = false }
)
checkbox:set_value(true, { settle = false })
input:type_text("hello", { settle = false })
input:focus()
local viewport = button:viewport()
assert(viewport ~= nil)
return { viewport = viewport.id }"#
            .to_string(),
        1_000,
        "widget-actions.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!({ "viewport": "root" })));
    assert!(
        !inner
            .actions
            .drain_actions(viewport_id, inner.frame_count())
            .is_empty()
    );
}

#[test]
fn initial_ruau_slice_runs_viewport_actions() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.configure({ settle = false })
local viewport = eguidev.root
viewport:dismiss_popups()
viewport:key("enter", { settle = false })
viewport:paste("hello", { settle = false })
viewport:input({ type = "pointer_move", position = { x = 1, y = 2 } })
viewport:input({
type = "pointer_button",
position = { x = 1, y = 2 },
button = "primary",
action = "press",
})
viewport:input({ type = "key", key = "enter", action = "release" })
viewport:input({ type = "text", text = "hello" })
viewport:input({ type = "scroll", delta = { x = 0, y = -10 } })
viewport:resize({ inner_size = { x = 320, y = 240 }, resizable = true })
return true"#
            .to_string(),
        1_000,
        "viewport-actions.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(true)));
}

#[test]
fn initial_ruau_slice_runs_visual_methods_without_screenshots() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r##"local viewport = eguidev.root
local widget = eguidev.widget("status")
local widget_issues = widget:layout_issues()
local viewport_issues = viewport:layout_issues()
widget:show_highlight("#ff0000")
widget:clear_highlight()
widget:show_debug_overlay({ mode = "bounds", show_labels = false })
widget:clear_debug_overlay()
viewport:show_highlight(
{ min = { x = 0, y = 0 }, max = { x = 10, y = 10 } },
"#00ff00"
)
viewport:clear_highlights()
viewport:show_debug_overlay({ mode = "bounds" })
viewport:clear_debug_overlay()
return { widget_issues = #widget_issues, viewport_issues = #viewport_issues }"##
            .to_string(),
        1_000,
        "visual-methods.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({ "widget_issues": 0, "viewport_issues": 0 }))
    );
}

fn run_overlay_script(script: &str) -> (Arc<Inner>, ViewportId) {
    let inner = Arc::new(Inner::new());
    let secondary = ViewportId::from_hash_of("highlight.secondary");
    for viewport_id in [ViewportId::ROOT, secondary] {
        inner.viewports.remember_viewport_id(viewport_id);
        let mut entry = make_entry("shared", 1, WidgetRole::Button);
        // Generated IDs can repeat across viewports; explicit IDs cannot.
        entry.explicit_id = false;
        entry.viewport_id = viewport_id_to_string(viewport_id);
        inner.widgets.clear_registry(viewport_id);
        inner.widgets.record_widget(viewport_id, entry);
        inner.widgets.finalize_registry(viewport_id);
    }
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        Arc::clone(&inner),
        runtime,
        format!(
            "local secondary_id = \"{}\"\n{script}",
            viewport_id_to_string(secondary)
        ),
        1_000,
        "highlight-viewports.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    (inner, secondary)
}

fn painted_overlay_colors(inner: &Inner, viewport_id: ViewportId) -> Vec<Color32> {
    let ctx = Context::default();
    let mut input = RawInput {
        viewport_id,
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(100.0, 100.0),
        )),
        ..Default::default()
    };
    input.viewports.entry(viewport_id).or_default();
    ctx.begin_pass(input);
    inner.paint_overlays(&ctx);
    let output = ctx.end_pass();
    let colors = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            Shape::Rect(rect) => Some(rect.stroke.color),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    colors
}

#[test]
fn rectangle_highlights_stay_in_their_viewport() {
    let (inner, secondary) = run_overlay_script(
        r##"
local rect = { min = { x = 10, y = 10 }, max = { x = 50, y = 50 } }
eguidev.root:show_highlight(rect, "#ff0000")
eguidev.viewport(secondary_id):show_highlight(rect, "#0000ff")
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert_eq!(
        painted_overlay_colors(&inner, secondary),
        vec![Color32::BLUE]
    );
}

#[test]
fn widget_highlights_stay_in_their_viewport() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:widget("shared"):show_highlight("#ff0000")
eguidev.viewport(secondary_id):widget("shared"):show_highlight("#0000ff")
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert_eq!(
        painted_overlay_colors(&inner, secondary),
        vec![Color32::BLUE]
    );
}

#[test]
fn clearing_viewport_highlights_preserves_other_viewports() {
    let (inner, secondary) = run_overlay_script(
        r##"
local rect = { min = { x = 10, y = 10 }, max = { x = 50, y = 50 } }
eguidev.root:show_highlight(rect, "#ff0000")
local secondary = eguidev.viewport(secondary_id)
secondary:show_highlight(rect, "#0000ff")
secondary:clear_highlights()
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert!(painted_overlay_colors(&inner, secondary).is_empty());
}

#[test]
fn clearing_widget_highlight_preserves_other_viewports() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:widget("shared"):show_highlight("#ff0000")
local secondary = eguidev.viewport(secondary_id):widget("shared")
secondary:show_highlight("#0000ff")
secondary:clear_highlight()
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert!(painted_overlay_colors(&inner, secondary).is_empty());
}

#[test]
fn viewport_debug_overlays_stay_in_their_viewport() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:show_debug_overlay({ show_labels = false, bounds_color = "#ff0000" })
eguidev.viewport(secondary_id):show_debug_overlay({ show_labels = false, bounds_color = "#0000ff" })
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert_eq!(
        painted_overlay_colors(&inner, secondary),
        vec![Color32::BLUE]
    );
}

#[test]
fn widget_debug_overlays_stay_in_their_viewport() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:widget("shared"):show_debug_overlay({ show_labels = false, bounds_color = "#ff0000" })
eguidev.viewport(secondary_id):widget("shared"):show_debug_overlay({ show_labels = false, bounds_color = "#0000ff" })
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert_eq!(
        painted_overlay_colors(&inner, secondary),
        vec![Color32::BLUE]
    );
}

#[test]
fn clearing_viewport_debug_overlay_preserves_other_viewports() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:show_debug_overlay({ show_labels = false, bounds_color = "#ff0000" })
eguidev.viewport(secondary_id):show_debug_overlay({ show_labels = false, bounds_color = "#0000ff" })
eguidev.root:clear_debug_overlay()
"##,
    );
    assert!(painted_overlay_colors(&inner, ViewportId::ROOT).is_empty());
    assert_eq!(
        painted_overlay_colors(&inner, secondary),
        vec![Color32::BLUE]
    );
}

#[test]
fn clearing_widget_debug_overlay_preserves_other_viewports() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:widget("shared"):show_debug_overlay({ show_labels = false, bounds_color = "#ff0000" })
local widget = eguidev.viewport(secondary_id):widget("shared")
widget:show_debug_overlay({ show_labels = false, bounds_color = "#0000ff" })
widget:clear_debug_overlay()
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
    assert!(painted_overlay_colors(&inner, secondary).is_empty());
}

#[test]
fn missing_debug_scope_does_not_expand_to_the_whole_viewport() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.viewport(secondary_id):widget("shared"):show_debug_overlay({ show_labels = false, bounds_color = "#0000ff" })
"##,
    );
    inner.widgets.clear_registry(secondary);
    let mut other = make_entry("other", 2, WidgetRole::Button);
    other.viewport_id = viewport_id_to_string(secondary);
    inner.widgets.record_widget(secondary, other);
    inner.widgets.finalize_registry(secondary);
    assert!(painted_overlay_colors(&inner, secondary).is_empty());
}

#[test]
fn missing_scoped_widget_can_clear_its_viewport_debug_overlay() {
    let (inner, secondary) = run_overlay_script(
        r##"
eguidev.root:show_debug_overlay({ show_labels = false, bounds_color = "#ff0000" })
local viewport = eguidev.viewport(secondary_id)
viewport:show_debug_overlay({ show_labels = false, bounds_color = "#0000ff" })
viewport:widget("missing"):clear_debug_overlay()
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, egui::ViewportId::ROOT),
        [egui::Color32::RED]
    );
    assert!(painted_overlay_colors(&inner, secondary).is_empty());
}

#[test]
fn dismiss_popups_preserves_other_viewport_overlays() {
    for show_method in ["show_highlight", "show_debug_overlay"] {
        let options = if show_method == "show_highlight" {
            "\"#0000ff\""
        } else {
            "{ show_labels = false, bounds_color = \"#0000ff\" }"
        };
        let script = format!(
            r#"
eguidev.viewport(secondary_id):widget("shared"):{show_method}({options})
eguidev.root:dismiss_popups({{ settle = false }})
"#
        );
        let (inner, secondary) = run_overlay_script(&script);
        assert_eq!(
            painted_overlay_colors(&inner, secondary),
            [egui::Color32::BLUE],
            "{show_method}"
        );
    }
}

#[test]
fn missing_widget_debug_clear_preserves_other_overlays() {
    let (inner, _) = run_overlay_script(
        r##"
eguidev.root:show_debug_overlay({ show_labels = false, bounds_color = "#ff0000" })
local ok = pcall(function() eguidev.widget("missing"):clear_debug_overlay() end)
assert(not ok, "an unscoped missing widget must not clear root")
"##,
    );
    assert_eq!(
        painted_overlay_colors(&inner, ViewportId::ROOT),
        vec![Color32::RED]
    );
}

#[test]
fn initial_ruau_slice_runs_predicate_methods() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.label = Some("Ready".to_string());
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);
    inner.viewports.update_viewports(&egui::Context::default());

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 })
local viewport = eguidev.root
local widget = eguidev.widget("status")
local from_widget = widget:wait(function(current)
if current == nil then return false end
eguidev.log("widget:" .. (current.label or ""))
return current.visible and current.label == "Ready"
end, { timeout_ms = 20, poll_interval_ms = 1 })
local viewport_state = viewport:wait(function(current)
return current ~= nil and current.frame_count >= 0
end, { timeout_ms = 20, poll_interval_ms = 1 })
local visible = widget:wait({ visible = true })
eguidev.widget("missing"):wait(
{ present = false },
{ timeout_ms = 20, poll_interval_ms = 1 }
)
assert(from_widget ~= nil and viewport_state ~= nil and visible ~= nil)
assert(viewport_state.id == viewport.id)
return {
viewport_label = from_widget.label,
widget_label = from_widget.label,
visible = visible.visible,
frame_count = viewport_state.frame_count,
viewport_id = viewport_state.id,
}"#
        .to_string(),
        1_000,
        "predicate-methods.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "viewport_label": "Ready",
            "widget_label": "Ready",
            "visible": true,
            "frame_count": 0,
            "viewport_id": "root",
        }))
    );
    assert_eq!(outcome.logs, vec!["widget:Ready"]);
}

#[test]
fn initial_ruau_slice_keeps_widget_state_numbers_comparable_to_luau_numbers() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("choice", 1, WidgetRole::ComboBox);
    entry.value = Some(WidgetValue::Int(2));
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);
    inner.viewports.update_viewports(&egui::Context::default());

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 })
local widget = eguidev.widget("choice")
local current = widget:state()
assert(current ~= nil)
assert(current.value == 2)
local matched = widget:wait(function(state)
return state ~= nil and state.value == 2
end)
assert(matched ~= nil)
return matched.value"#
            .to_string(),
        1_000,
        "widget-state-numbers.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(2)));
}

#[test]
fn initial_ruau_slice_keeps_integer_args_comparable_to_luau_numbers() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"assert(eguidev.args.count == 4)
return eguidev.args.count"#
            .to_string(),
        1_000,
        "args.luau".to_string(),
        ScriptArgs::from([("count".to_string(), ScriptArgValue::Int(4))]),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(4)));
}

#[test]
fn wait_frames_names_the_fault_of_a_bad_count() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local messages = {}
for _, count in { -1, 1.5 } do
    local ok, cause = pcall(function() eguidev.wait_frames(count) end)
    assert(not ok, "a bad count must fail")
    table.insert(messages, (cause :: Error).message)
end
return messages"#
            .to_string(),
        1_000,
        "wait-frames-count.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    let message = "frame count must be a non-negative integer";
    assert_eq!(outcome.value, Some(json!([message, message])));
}

#[test]
fn rejected_configure_does_not_poison_luau_defaults() {
    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.configure({ timeout_ms = 30, poll_interval_ms = 1 })
local configured = pcall(function()
eguidev.configure({ timeout_ms = -1 })
end)
assert(configured == false)
eguidev.wait(function()
return false
end)
"#
        .to_string(),
        1_000,
        "configure-reject.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.as_ref().expect("timeout error");
    assert_eq!(error.error_type, "timeout");
    assert!(
        outcome.timing.exec_ms < 500,
        "rejected configure must keep the previous timeout: {outcome:?}"
    );
}

#[test]
fn viewport_wait_and_expect_handle_absence() {
    let inner = Arc::new(Inner::new());
    let absent = ViewportId::from_hash_of("absent.viewport.wait");
    inner.viewports.update_viewports(&Context::default());
    inner.viewports.remember_viewport_id(absent);
    assert!(!inner.viewports.is_live_viewport(absent));
    let runtime = Runtime::ensure_for_inner(&inner);
    let script = r#"
assert(viewport:state() == nil)
assert(viewport:wait({ present = false }) == nil)
assert(viewport:wait(function(current) return current == nil end) == nil)
assert(viewport:expect({ present = false }) == nil)
local ok, error = pcall(function()
viewport:wait({ present = true }, { timeout_ms = 1, poll_interval_ms = 1 })
end)
assert(not ok, "waiting for presence must time out")
assert((error :: Error).code == "timeout", "absence must preserve the timeout error")
return true
"#;
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        format!(
            "local viewport = eguidev.viewport(\"{}\")\n{script}",
            viewport_id_to_string(absent)
        ),
        1_000,
        "absent-viewport-wait.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(true)));
    assert_eq!(outcome.assertions.len(), 1);
    assert!(outcome.assertions[0].passed);
}

#[test]
fn capture_snapshots_are_immutable() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local cap = eguidev.capture()
local anyCap: any = cap
local frameOk = pcall(function()
anyCap.frame = 1
end)
local idOk = pcall(function()
anyCap.__capture_id = 0
end)
local diff = cap:diff()
return {
frame_ok = frameOk,
id_ok = idOk,
widget_table = anyCap.__widgets == nil,
change_count = #diff.changes,
}
"#
        .to_string(),
        1_000,
        "capture-freeze.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "frame_ok": false,
            "id_ok": false,
            "widget_table": true,
            "change_count": 0,
        }))
    );
}

#[test]
fn a_diff_of_a_tree_above_the_marshal_budget_resolves_in_rust() {
    // Each widget state marshals as dozens of nodes, so this tree is far
    // above the 1,048,576 nodes that Ruau marshals in one call.
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let widgets = 60_000_u64;
    for index in 0..widgets {
        inner.widgets.record_widget(
            viewport_id,
            make_entry(&format!("row.{index}"), index + 1, WidgetRole::Label),
        );
    }
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local before = eguidev.capture()
local diff = before:diff()
return #diff.changes"#
            .to_string(),
        30_000,
        "large-diff.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{:?}", outcome.error);
    assert_eq!(outcome.value, Some(json!(0)));
}

#[test]
fn wait_rejects_invalid_condition_combinations() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local function code_of(run: () -> ()): string
local ok, err = pcall(run)
assert(ok == false)
return (err :: Error).code
end
local present = code_of(function()
eguidev.widget("status"):wait({ present = false, visible = true })
end)
local label = code_of(function()
eguidev.widget("status"):wait({ label = "" })
end)
local pointer = code_of(function()
eguidev.widget("status"):wait({ data = { pointer = "not-a-pointer", equals = true } })
end)
local viewport = code_of(function()
eguidev.root:wait({ present = false, focused = true })
end)
return {
present = present,
label = label,
pointer = pointer,
viewport = viewport,
}
"#
        .to_string(),
        1_000,
        "wait-validate.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "present": "invalid_argument",
            "label": "invalid_argument",
            "pointer": "invalid_argument",
            "viewport": "invalid_argument",
        }))
    );
}

/// Render a background button under an interactable `Area` at
/// `Order::Middle`, and a second button inside the area, into `inner`'s
/// registry. An area needs one prior frame before egui treats it as
/// interactable, and this frame shows the area before the background
/// button so a single repeat already reaches steady-state coverage.
fn render_covered_scene(inner: &Arc<Inner>) {
    use crate::widget_registry::{WidgetMeta, record_widget};

    let viewport_id = egui::ViewportId::ROOT;
    let ctx = egui::Context::default();
    let raw_input = || egui::RawInput {
        viewport_id,
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 400.0),
        )),
        ..Default::default()
    };
    for _ in 0..2 {
        inner.widgets.clear_registry(viewport_id);
        let output = ctx.run_ui(raw_input(), |ui| {
            let card = egui::Area::new(egui::Id::unique("floating.card"))
                .order(egui::Order::Middle)
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    ui.set_min_size(egui::vec2(200.0, 200.0));
                    ui.button("Approve")
                });
            record_widget(
                &inner.widgets,
                "card.approve".to_string(),
                &card.inner,
                WidgetMeta {
                    role: WidgetRoleMeta::Plain(WidgetRole::Button),
                    label: Some("Approve".to_string()),
                    visible: true,
                    ..Default::default()
                },
            );

            let background = ui.button("Hidden approve");
            record_widget(
                &inner.widgets,
                "background.approve".to_string(),
                &background,
                WidgetMeta {
                    role: WidgetRoleMeta::Plain(WidgetRole::Button),
                    label: Some("Hidden approve".to_string()),
                    visible: true,
                    ..Default::default()
                },
            );
        });
        output.drop_without_applying_deltas();
        inner.widgets.finalize_registry(viewport_id);
    }
}

#[test]
fn covered_widget_state_reports_coverage_by_the_floating_card() {
    let inner = Arc::new(Inner::new());
    render_covered_scene(&inner);
    let runtime = Runtime::ensure_for_inner(&inner);

    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local background = eguidev.widget("background.approve"):state()
local card = eguidev.widget("card.approve"):state()
assert(background ~= nil and card ~= nil)
return { background_covered = background.covered, card_covered = card.covered }"#
            .to_string(),
        1_000,
        "covered-state.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({ "background_covered": true, "card_covered": false }))
    );
}

/// `Widget:click()` waits for `{ actionable = true }` before it queues the
/// low-level click, exactly as it already does for an invisible or
/// disabled widget, so a permanently covered widget times out here rather
/// than reaching the covering card. The direct `not_actionable` reason
/// `covered` is asserted at the tool layer by
/// `action_click_rejects_covered_widget` in `automation::mod`, which calls
/// the click tool without this settling wait.
#[test]
fn covered_widget_click_times_out_instead_of_reaching_the_card() {
    let inner = Arc::new(Inner::new());
    render_covered_scene(&inner);
    let runtime = Runtime::ensure_for_inner(&inner);

    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("background.approve"):click({ timeout_ms = 60, poll_interval_ms = 5 })"#
            .to_string(),
        1_000,
        "covered-click.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.as_ref().expect("covered click timeout");
    assert_eq!(error.error_type, "timeout");
}

#[test]
fn covered_widget_fails_the_actionable_wait() {
    let inner = Arc::new(Inner::new());
    render_covered_scene(&inner);
    let runtime = Runtime::ensure_for_inner(&inner);

    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("background.approve"):wait(
{ actionable = true },
{ timeout_ms = 60, poll_interval_ms = 5 }
)"#
        .to_string(),
        1_000,
        "covered-actionable-wait.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.as_ref().expect("actionable wait timeout");
    assert_eq!(error.error_type, "timeout");
}

// `scroll_into_view()`'s settle-enabled interaction-ready poll is tested
// in `automation::mod::covered_widget_scroll_into_view_still_settles`,
// which drives real frames: `settle_after_action` needs an advancing
// frame count that this blocking, single-shot harness cannot provide.

#[test]
fn uncovered_widget_inside_the_card_clicks_normally() {
    let inner = Arc::new(Inner::new());
    render_covered_scene(&inner);
    let runtime = Runtime::ensure_for_inner(&inner);

    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("card.approve"):click({ settle = false })"#.to_string(),
        1_000,
        "card-click.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
}

#[test]
fn covered_query_filter_and_expect_condition_select_the_right_widget() {
    let inner = Arc::new(Inner::new());
    render_covered_scene(&inner);
    let runtime = Runtime::ensure_for_inner(&inner);

    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local covered = eguidev.root:widgets({ covered = true })
local uncovered = eguidev.root:widgets({ covered = false })
eguidev.widget("card.approve"):expect({ covered = false })
return {
covered_count = #covered,
covered_first = covered[1].id,
uncovered_count = #uncovered,
uncovered_first = uncovered[1].id,
}"#
        .to_string(),
        1_000,
        "covered-filter.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    let value = outcome.value.expect("script value");
    assert_eq!(value["covered_count"], 1);
    assert_eq!(value["covered_first"], "background.approve");
    assert_eq!(value["uncovered_count"], 1);
    assert_eq!(value["uncovered_first"], "card.approve");
}

#[test]
fn click_settle_shares_one_timeout_budget() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("button", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"eguidev.widget("button"):click({ timeout_ms = 200, poll_interval_ms = 1 })
"#
        .to_string(),
        3_000,
        "click-timeout.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(!outcome.success, "{outcome:?}");
    let error = outcome.error.as_ref().expect("timeout error");
    assert_eq!(error.error_type, "timeout");
    assert!(
        !error.message.contains("Script timed out after"),
        "settle must use the call timeout, not the script deadline: {error:?}"
    );
    assert!(
        outcome.timing.exec_ms < 800,
        "click wait plus settle must stay near 200 ms: {outcome:?}"
    );
}

#[test]
fn widget_list_filters_focused_widgets() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut focused = make_entry("focused", 1, WidgetRole::Button);
    focused.focused = true;
    inner.widgets.record_widget(viewport_id, focused);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("other", 2, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local focused = eguidev.root:widgets({ focused = true })
local selected = eguidev.root:widgets({ selected = true })
return { focused = focused[1].id, focused_count = #focused, selected_count = #selected }
"#
        .to_string(),
        1_000,
        "widget-filter.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "focused": "focused",
            "focused_count": 1,
            "selected_count": 0,
        }))
    );
}

#[test]
fn widget_list_filters_selected_toggle() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    let mut toggle = make_entry("feature", 1, WidgetRole::Toggle);
    toggle.value = Some(WidgetValue::Bool(true));
    inner.widgets.record_widget(viewport_id, toggle);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("other", 2, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local selected = eguidev.root:widgets({ selected = true })
local state = eguidev.widget("feature"):state()
assert(state ~= nil)
return { selected = selected[1].id, selected_count = #selected, state_selected = state.selected }
"#
        .to_string(),
        1_000,
        "widget-selected-toggle.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "selected": "feature",
            "selected_count": 1,
            "state_selected": true,
        }))
    );
}

#[test]
fn key_target_keeps_scoped_widget_viewport() {
    let inner = Arc::new(Inner::new());
    let secondary = egui::ViewportId::from_hash_of("script.key.target.secondary");
    let secondary_id = viewport_id_to_string(secondary);
    inner.viewports.remember_viewport_id(secondary);
    inner.widgets.clear_registry(secondary);
    let mut field = make_entry("field", 1, WidgetRole::TextEdit);
    field.viewport_id = secondary_id.clone();
    field.focused = true;
    inner.widgets.record_widget(secondary, field);
    inner.widgets.finalize_registry(secondary);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        format!(
            r#"eguidev.configure({{ settle = false }})
local viewport = eguidev.viewport("{secondary_id}")
viewport:key("a", {{ target = viewport:widget("field"), settle = false }})
return true
"#
        ),
        1_000,
        "key-target.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.value, Some(json!(true)));
}

#[test]
fn keyboard_actions_confirm_focus_within_the_action_timeout() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("field", 1, WidgetRole::TextEdit));
    inner.widgets.finalize_registry(viewport_id);

    // No frame runs, so the field never takes the focus. Each action must
    // fail at its own timeout: the script deadline is shorter than the
    // old fixed focus wait of 5 seconds.
    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local field = eguidev.widget("field")
local typed, typed_error = pcall(function()
field:type_text("abc", { timeout_ms = 40, poll_interval_ms = 1, settle = false })
end)
local keyed, key_error = pcall(function()
eguidev.root:key("a", { target = field, timeout_ms = 40, poll_interval_ms = 1, settle = false })
end)
return {
typed = typed,
typed_code = (typed_error :: any).code,
keyed = keyed,
key_code = (key_error :: any).code,
}"#
        .to_string(),
        2_000,
        "keyboard-focus.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "typed": false,
            "typed_code": "not_actionable",
            "keyed": false,
            "key_code": "not_actionable",
        }))
    );
}

#[test]
fn fixture_catalog_keeps_viewport_scoped_targets() {
    let inner = Arc::new(Inner::new());
    let secondary = egui::ViewportId::from_hash_of("fixture.list.secondary");
    let secondary_id = viewport_id_to_string(secondary);
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("multi", "Multi viewport fixture.").ready_in("status", secondary),
    ]);

    let runtime = Runtime::ensure_for_inner(&inner);
    let outcome = run_script_eval_blocking(
        inner,
        runtime,
        r#"local catalog = eguidev.fixtures()
local ready = catalog[1].ready[1]
local widget = ready.widget :: any
return { id = ready.widget.id, viewport = widget.__viewport_id }
"#
        .to_string(),
        1_000,
        "fixture-catalog.luau".to_string(),
        ScriptArgs::default(),
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.value,
        Some(json!({
            "id": "status",
            "viewport": secondary_id,
        }))
    );
}

#[test]
fn nested_sample_arrays_are_promoted_for_luau_arithmetic() {
    let mut vm = Vm::builder()
        .ambient(Ambient::deterministic(0))
        .limits(Limits::unlimited())
        .runtime_capabilities(RuntimeCapabilities::default().enable_runtime_compilation())
        .trusted_host()
        .build()
        .expect("sample vm builds");
    vm.step(|scope| {
        let encoded = json_to_scoped_value(
            scope,
            &json!({
                "samples": [{
                    "physical": [25, 16],
                    "rgba": [47, 128, 237, 255],
                }]
            }),
        )?;
        let probe = scope.load_chunk(
            br#"
            return function(d)
                local sample = d.samples[1]
                return sample.rgba[1] + 1 == 48 and sample.physical[1] * 2 == 50
            end
            "#,
            b"=sample-arrays",
        )?;
        let probe: Function<'_> = scope.call(probe, ())?;
        let ok: bool = scope.call(probe, (encoded,))?;
        assert!(ok);
        Ok(())
    })
    .expect("sample arrays add and multiply as numbers");
}

/// Return the fields of a declared table type, and of each type that it
/// intersects.
fn declared_fields(name: &str) -> BTreeSet<String> {
    let source = crate::script_definitions();
    let marker = format!("export type {name} =");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("no declared type {name}"));
    let body = &source[start + marker.len()..];
    let open = body.find('{').expect("a table type");
    let mut fields = BTreeSet::new();
    for part in body[..open].split('&').map(str::trim) {
        if !part.is_empty() {
            fields.extend(declared_fields(part));
        }
    }
    let mut depth = 0;
    for line in body[open..].lines() {
        let trimmed = line.trim();
        if depth == 1
            && !trimmed.starts_with("--")
            && let Some((field, _)) = trimmed.split_once(':')
            && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            fields.insert(field.to_string());
        }
        depth += line.matches('{').count();
        depth -= line.matches('}').count();
        if depth == 0 {
            break;
        }
    }
    fields
}

/// Build a registry with a button, a focused text field, and a scroll area.
fn option_parity_inner() -> Arc<Inner> {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("button", 1, WidgetRole::Button));
    let mut field = make_entry("field", 2, WidgetRole::TextEdit);
    field.focused = true;
    inner.widgets.record_widget(viewport_id, field);
    let mut scroll = make_entry("scroll", 3, WidgetRole::ScrollArea);
    scroll.role_state = Some(RoleState::ScrollArea {
        offset: Vec2 { x: 0.0, y: 0.0 },
        viewport_size: Vec2 { x: 10.0, y: 10.0 },
        content_size: Vec2 { x: 10.0, y: 100.0 },
    });
    inner.widgets.record_widget(viewport_id, scroll);
    inner.widgets.finalize_registry(viewport_id);
    inner
}

#[test]
fn each_host_function_reads_the_declared_option_fields() {
    // Each case calls one public function with every declared field of
    // its option type. The keys that the private library consumes or adds
    // for a case are not compared.
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "WaitOptions",
            r#"button:wait({ actionable = true }, { timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "ActionOptions",
            r#"button:focus({ timeout_ms = 50, poll_interval_ms = 1, settle = false })"#,
            &[],
        ),
        (
            "ClickOptions",
            r#"button:click({
timeout_ms = 50, poll_interval_ms = 1, settle = false,
button = "primary", click_count = 1, modifiers = { ctrl = true },
})"#,
            &[],
        ),
        (
            "HoverOptions",
            r#"button:hover({
timeout_ms = 50, poll_interval_ms = 1, settle = false,
position = { x = 0.5, y = 0.5 }, duration_ms = 0,
})"#,
            &[],
        ),
        (
            "TypeTextOptions",
            r#"field:type_text("a", {
timeout_ms = 50, poll_interval_ms = 1, settle = false, clear = true, enter = true,
})"#,
            &[],
        ),
        (
            "DragOptions",
            r#"button:drag_relative({ x = 0.5, y = 0.5 }, {
timeout_ms = 50, poll_interval_ms = 1, settle = false,
from = { x = 0.5, y = 0.5 }, modifiers = { shift = true },
})"#,
            &[],
        ),
        (
            "ScrollOptions",
            r#"button:scroll({ x = 0, y = 1 }, {
timeout_ms = 50, poll_interval_ms = 1, settle = false, modifiers = { ctrl = true },
})"#,
            &[],
        ),
        (
            "ScrollToOptions",
            r#"scroll:scroll_to({
timeout_ms = 50, poll_interval_ms = 1, settle = false,
offset = { x = 0, y = 0 }, align = "top",
})"#,
            &[],
        ),
        (
            "KeyOptions",
            r#"eguidev.root:key("a", {
timeout_ms = 50, poll_interval_ms = 1, settle = false, repeat_count = 1, target = field,
})"#,
            &[],
        ),
        (
            "ResizeOptions",
            r#"eguidev.root:resize({
timeout_ms = 50, poll_interval_ms = 1, settle = true,
inner_size = { x = 100, y = 100 }, min_size = { x = 10, y = 10 },
max_size = { x = 200, y = 200 }, increments = { x = 1, y = 1 }, resizable = true,
})"#,
            &[],
        ),
        (
            "DebugOverlayOptions",
            r##"eguidev.root:show_debug_overlay({
mode = "bounds", show_labels = true, show_sizes = true, label_font_size = 10,
bounds_color = "#00ff00", clip_color = "#ff0000", overlap_color = "#ffff00",
})"##,
            &[],
        ),
        (
            "ScreenshotOptions",
            r#"eguidev.root:screenshot({ format = "jpeg", max_dimension = 0 })"#,
            &[],
        ),
        (
            "DumpOptions",
            r#"eguidev.dump({
viewport = eguidev.root, root = button, include_invisible = true, fields = "full",
})"#,
            &[],
        ),
        (
            "DiffOptions",
            r#"eguidev.capture():diff({ move_epsilon = 0.5, include_invisible = true, id_prefix = "b" })"#,
            &[],
        ),
        (
            "WidgetQuery",
            r#"eguidev.widgets({
role = "button", id_prefix = "b", label = "b", label_contains = "b", visible = true,
enabled = true, focused = false, selected = false, covered = false,
viewport = eguidev.root,
})"#,
            &["viewport", "include_invisible"],
        ),
        (
            "ScriptOptions",
            r#"eguidev.configure({
timeout_ms = 50, poll_interval_ms = 1, settle = true, animations = false,
})"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"button:wait({ present = false }, { timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"eguidev.root:wait(function(_) return false end, { timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"eguidev.root:settle({ timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"eguidev.root:wait_capture({ timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"eguidev.wait(function() return false end, { timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"eguidev.wait_frames(1, { timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "WaitOptions",
            r#"eguidev.wait_viewport({ name = "none" }, { timeout_ms = 50, poll_interval_ms = 1 })"#,
            &[],
        ),
        (
            "ActionOptions",
            r#"button:set_value(true, { timeout_ms = 50, poll_interval_ms = 1, settle = true })"#,
            &[],
        ),
        (
            "ActionOptions",
            r#"button:scroll_into_view({ timeout_ms = 50, poll_interval_ms = 1, settle = true })"#,
            &[],
        ),
        (
            "ActionOptions",
            r#"eguidev.root:dismiss_popups({ timeout_ms = 50, poll_interval_ms = 1, settle = true })"#,
            &[],
        ),
        (
            "ActionOptions",
            r#"eguidev.root:paste("a", { timeout_ms = 50, poll_interval_ms = 1, settle = true })"#,
            &[],
        ),
        (
            "ActionOptions",
            r#"eguidev.root:focus({ timeout_ms = 50, poll_interval_ms = 1, settle = true })"#,
            &[],
        ),
        (
            "DragOptions",
            r#"button:drag_to({ x = 1, y = 1 }, {
timeout_ms = 50, poll_interval_ms = 1, settle = false,
from = { x = 0.5, y = 0.5 }, modifiers = { shift = true },
})"#,
            &[],
        ),
        (
            "DragOptions",
            r#"button:drag_to(field, {
timeout_ms = 50, poll_interval_ms = 1, settle = false,
from = { x = 0.5, y = 0.5 }, modifiers = { shift = true },
})"#,
            &[],
        ),
        (
            "ScreenshotOptions",
            r#"button:screenshot({ format = "jpeg", max_dimension = 0 })"#,
            &[],
        ),
        (
            "ScreenshotOptions",
            r#"eguidev.root:native_screenshot({ format = "jpeg", max_dimension = 0 })"#,
            &[],
        ),
        (
            "DebugOverlayOptions",
            r##"button:show_debug_overlay({
mode = "bounds", show_labels = true, show_sizes = true, label_font_size = 10,
bounds_color = "#00ff00", clip_color = "#ff0000", overlap_color = "#ffff00",
})"##,
            &[],
        ),
        (
            "WidgetFilter",
            r#"eguidev.root:widgets({
role = "button", id_prefix = "b", label = "b", label_contains = "b", visible = true,
enabled = true, focused = false, selected = false, covered = false,
})"#,
            &["include_invisible"],
        ),
        (
            "DumpOptions",
            r#"eguidev.dump_text({
viewport = eguidev.root, root = button, include_invisible = true, fields = "full",
})"#,
            &[],
        ),
    ];
    // The private library adds the viewport of a receiver to its options.
    let internal = ["viewport_id"];
    for (type_name, call, library_keys) in cases {
        let inner = option_parity_inner();
        let runtime = Runtime::ensure_for_inner(&inner);
        super::super::parse::take_read_keys();
        let outcome = run_script_eval_blocking(
            inner,
            runtime,
            format!(
                "local button = eguidev.widget(\"button\")\n\
                 local field = eguidev.widget(\"field\")\n\
                 local scroll = eguidev.widget(\"scroll\")\n\
                 local _ = {{ button, field, scroll }}\n\
                 pcall(function()\n{call}\nend)\n"
            ),
            2_000,
            format!("{type_name}.luau"),
            ScriptArgs::default(),
        );
        assert!(outcome.success, "{type_name}: {outcome:?}");
        let compared = |key: &String| {
            !internal.contains(&key.as_str()) && !library_keys.contains(&key.as_str())
        };
        let read = super::super::parse::take_read_keys()
            .into_iter()
            .filter(compared)
            .collect::<BTreeSet<_>>();
        let declared = declared_fields(type_name)
            .into_iter()
            .filter(compared)
            .collect::<BTreeSet<_>>();
        assert_eq!(read, declared, "{type_name}");
    }
}

fn make_entry(id: &str, native_id: u64, role: WidgetRole) -> WidgetRegistryEntry {
    let rect = Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 { x: 10.0, y: 10.0 },
    };
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
        parent_id: None,
        enabled: true,
        visible: true,
        focused: false,
        covered: false,
    }
}
