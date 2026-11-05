//! Tests of script evaluation through the app server.

use super::*;

#[tokio::test]
async fn tools_list_contains_only_the_app_boundary() {
    use tmcp::{ServerHandler, schema::Cursor, testutils::TestServerContext};

    let inner = Arc::new(Inner::new());
    let runtime = Runtime::ensure_for_inner(&inner);
    let server = AppMcpServer::new(inner, runtime);
    let ctx = TestServerContext::new();
    let tools = server
        .list_tools(ctx.ctx(), None::<Cursor>)
        .await
        .expect("list tools")
        .tools;
    let mut names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["app_close", "script_api", "script_eval"]);
}

#[tokio::test]
async fn script_api_returns_shared_discovery() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let overview = server
        .script_api(ScriptApiQuery::default())
        .await
        .expect("overview");
    assert_eq!(overview.structured_content.unwrap()["mode"], "overview");
    let detail = server
        .script_api(ScriptApiQuery {
            list: false,
            filter: Some("Widget.click".to_owned()),
        })
        .await
        .expect("detail");
    assert_eq!(detail.structured_content.unwrap()["mode"], "detail");
}

#[tokio::test]
async fn script_eval_returns_value_and_logs() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "eguidev.log(\"hello\")\nreturn 1 + 1".to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"].as_i64(), Some(2));
    assert_eq!(json["logs"][0], "hello");
}

#[tokio::test]
async fn script_eval_exposes_one_frozen_namespace_and_stable_references() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            r#"local first = eguidev.widget("missing")
local second = eguidev.widget("missing")
local widgets = eguidev.widgets()
local namespace = eguidev :: any
return {
namespace_frozen = table.isfrozen(eguidev),
args_frozen = table.isfrozen(eguidev.args),
reference_frozen = table.isfrozen(first),
collection_frozen = table.isfrozen(widgets),
stable_identity = first == second,
missing_state = first:state() == nil,
hidden_capabilities_absent = namespace.query == nil
    and namespace.action == nil
    and namespace.wait_kernel == nil
    and namespace.capture_kernel == nil
    and namespace.fixture_kernel == nil
    and namespace.diagnostic_kernel == nil
    and namespace.record == nil,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(
        json["value"],
        json!({
            "namespace_frozen": true,
            "args_frozen": true,
            "reference_frozen": true,
            "collection_frozen": true,
            "stable_identity": true,
            "missing_state": true,
            "hidden_capabilities_absent": true,
        })
    );
}

#[tokio::test]
async fn script_eval_rejects_removed_api_globals_before_execution() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval("return widget(\"missing\")".to_string(), None, None)
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "typecheck");
}

#[tokio::test]
async fn script_eval_omitted_args_default_to_empty_table() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval("return next(eguidev.args) == nil".to_string(), None, None)
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(json["value"], true);
}

#[tokio::test]
async fn script_eval_exposes_scalar_args() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "return { name = eguidev.args.name, count = eguidev.args.count, ratio = eguidev.args.ratio, enabled = eguidev.args.enabled }"
                .to_string(),
            None,
            Some(ScriptEvalOptions {
                source_name: Some("args.luau".to_string()),
                args: ScriptArgs::from([
                    ("name".to_string(), ScriptArgValue::String("Sky".to_string())),
                    ("count".to_string(), ScriptArgValue::Int(4)),
                    ("ratio".to_string(), ScriptArgValue::Float(1.5)),
                    ("enabled".to_string(), ScriptArgValue::Bool(true)),
                ]),
                ..ScriptEvalOptions::default()
            }),
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(json["value"]["name"], "Sky");
    assert_eq!(json["value"]["count"], 4);
    assert_eq!(json["value"]["ratio"], 1.5);
    assert_eq!(json["value"]["enabled"], true);
}

#[tokio::test]
async fn script_eval_checks_and_runs_named_module_sources() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "local helper = require('helpers/math')\nreturn helper.double(21)".to_string(),
            None,
            Some(ScriptEvalOptions {
                source_name: Some("probe.luau".to_string()),
                modules: ScriptModules::from([(
                    "helpers/math.luau".to_string(),
                    "local helper = {}\nfunction helper.double(value: number): number\n    return value * 2\nend\nreturn helper".to_string(),
                )]),
                ..ScriptEvalOptions::default()
            }),
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);

    assert_eq!(json["success"], true);
    assert_eq!(json["value"], 42);
}

#[tokio::test]
async fn script_eval_reports_module_type_errors_at_the_module_source() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "return require('helpers/broken')".to_string(),
            None,
            Some(ScriptEvalOptions {
                source_name: Some("probe.luau".to_string()),
                modules: ScriptModules::from([(
                    "helpers/broken.luau".to_string(),
                    "local value: string = 42\nreturn value".to_string(),
                )]),
                ..ScriptEvalOptions::default()
            }),
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);

    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "typecheck");
    assert!(
        json["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("helpers/broken.luau:1:"),
        "{json:#}"
    );
}

#[tokio::test]
async fn script_eval_keeps_module_names_in_runtime_backtraces() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "local helper = require('helpers/runtime')\nreturn helper.fail()".to_string(),
            None,
            Some(ScriptEvalOptions {
                source_name: Some("probe.luau".to_string()),
                modules: ScriptModules::from([(
                    "helpers/runtime.luau".to_string(),
                    "local helper = {}\nfunction helper.fail()\n    error('helper boom')\nend\nreturn helper".to_string(),
                )]),
                ..ScriptEvalOptions::default()
            }),
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);

    assert_eq!(json["success"], false);
    assert!(
        json["error"]["backtrace"]
            .as_array()
            .expect("backtrace")
            .iter()
            .any(|line| line
                .as_str()
                .is_some_and(|line| line.contains("helpers/runtime"))),
        "{json:#}"
    );
}

#[tokio::test]
async fn script_eval_preserves_empty_tool_result_arrays() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"return eguidev.root:widgets({ id_prefix = "missing" })"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], json!([]));
}

#[tokio::test]
async fn script_eval_root_widget_list_returns_widget_handles() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut status = make_entry("status", 1, WidgetRole::Label);
    status.label = Some("Ready state".to_string());
    inner.widgets.record_widget(viewport_id, status);
    let mut other = make_entry("other", 2, WidgetRole::Button);
    other.label = Some("Other state".to_string());
    inner.widgets.record_widget(viewport_id, other);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"local widgets = eguidev.root:widgets({ id_prefix = "status" })
local exact = eguidev.root:widgets({ label = "Ready state" })
local contains = eguidev.root:widgets({ label_contains = "state" })
local state = widgets[1]:state()
assert(state ~= nil)
return {
count = #widgets,
id = widgets[1].id,
viewport = state.viewport_id,
exact = #exact,
contains = #contains,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["value"],
        json!({ "count": 1, "id": "status", "viewport": "root", "exact": 1, "contains": 2 })
    );
}

#[tokio::test]
async fn script_eval_widget_global_finds_across_viewports() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let secondary = egui::ViewportId::from_hash_of("script.global.widget.secondary");
    let secondary_id = viewport_id_to_string(secondary);

    inner.viewports.remember_viewport_id(secondary);
    inner.widgets.clear_registry(secondary);
    let mut panel = make_entry("panel", 1, WidgetRole::Label);
    panel.viewport_id = secondary_id.clone();
    inner.widgets.record_widget(secondary, panel);
    inner.widgets.finalize_registry(secondary);

    let result = server
        .script_eval(
            r#"local found = eguidev.widget("panel")
local found_state = found:state()
local maybe_state = eguidev.widget("panel"):state()
local missing = eguidev.widget("missing"):state()
assert(found_state ~= nil and maybe_state ~= nil)
return {
id = found.id,
viewport = found_state.viewport_id,
maybe_viewport = maybe_state.viewport_id,
missing = missing == nil,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["value"],
        json!({ "id": "panel", "viewport": secondary_id, "maybe_viewport": secondary_id, "missing": true })
    );
}

#[tokio::test]
async fn script_eval_scoped_widget_handle_keeps_its_viewport() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let secondary = egui::ViewportId::from_hash_of("script.scoped.widget.secondary");
    let secondary_id = viewport_id_to_string(secondary);

    inner.viewports.remember_viewport_id(secondary);
    inner.widgets.clear_registry(secondary);
    let mut panel = make_entry("panel", 1, WidgetRole::Label);
    panel.viewport_id = secondary_id.clone();
    inner.widgets.record_widget(secondary, panel);
    inner.widgets.finalize_registry(secondary);

    let result = server
        .script_eval(
            format!(
                r#"local viewport = eguidev.viewport("{secondary_id}")
local found = viewport:widget("panel")
local condition: WidgetCondition = {{ visible = true }}
local state = found:wait(condition)
assert(state ~= nil)
return {{ id = found.id, viewport = state.viewport_id }}"#
            ),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:#}");
    assert_eq!(
        json["value"],
        json!({ "id": "panel", "viewport": secondary_id })
    );
}

#[tokio::test]
async fn script_eval_widget_global_reports_ambiguous_matches() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let secondary = egui::ViewportId::from_hash_of("script.global.widget.ambiguous");
    let secondary_id = viewport_id_to_string(secondary);

    inner.viewports.remember_viewport_id(secondary);
    inner.widgets.clear_registry(egui::ViewportId::ROOT);
    inner.widgets.record_widget(
        egui::ViewportId::ROOT,
        make_generated_entry(0x2a, WidgetRole::Label),
    );
    inner.widgets.finalize_registry(egui::ViewportId::ROOT);
    inner.widgets.clear_registry(secondary);
    let mut panel = make_generated_entry(0x2a, WidgetRole::Label);
    panel.viewport_id = secondary_id;
    inner.widgets.record_widget(secondary, panel);
    inner.widgets.finalize_registry(secondary);

    let result = server
        .script_eval(
            r#"return eguidev.widget("2a"):state()"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "tool");
    assert_eq!(json["error"]["code"], "not_found");
    let candidates = json["error"]["details"]["candidates"]
        .as_array()
        .expect("candidates");
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn script_eval_widget_global_wait_is_not_root_scoped() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));

    inner.widgets.clear_registry(egui::ViewportId::ROOT);
    inner.widgets.record_widget(
        egui::ViewportId::ROOT,
        make_entry("basic.submit", 1, WidgetRole::Button),
    );
    inner.widgets.finalize_registry(egui::ViewportId::ROOT);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 5, poll_interval_ms = 1 })
eguidev.widget("basic.submt"):wait({ present = true })"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "timeout", "{json:?}");
    assert!(json["error"]["details"]["observation"]["target_viewport_id"].is_null());
    let suggestions = json["error"]["details"]["search"]["suggestions"]
        .as_array()
        .expect("suggestions");
    assert!(
        suggestions
            .iter()
            .any(|suggestion| suggestion.as_str() == Some("basic.submit"))
    );
}

#[tokio::test]
async fn script_eval_built_in_expect_helpers_return_widget_state() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut status = make_entry("status", 1, WidgetRole::Label);
    status.label = Some("Ready".to_string());
    inner.widgets.record_widget(viewport_id, status);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"local state = eguidev.widget("status"):expect({ label = "Ready", visible = true })
eguidev.widget("missing"):expect({ present = false })
assert(state ~= nil)
return state.label"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], "Ready");
}

#[tokio::test]
async fn script_eval_capture_diff_reports_changed_widgets() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    record_test_snapshot(&inner, viewport_id);
    inner.widgets.clear_registry(viewport_id);
    let mut before = make_entry("status", 1, WidgetRole::Label);
    before.label = Some("Before".to_string());
    inner.widgets.record_widget(viewport_id, before);
    inner.widgets.finalize_registry(viewport_id);

    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("mutate", "Mutate widget.").ready("status"),
    ]);
    let inner_for_fixture = Arc::clone(&inner);
    set_runtime_fixture_handler(&inner, move |_call| {
        inner_for_fixture.widgets.clear_registry(viewport_id);
        let mut after = make_entry_with_rect(
            "status",
            1,
            WidgetRole::Label,
            Rect {
                min: Pos2 { x: 2.0, y: 0.0 },
                max: Pos2 { x: 12.0, y: 10.0 },
            },
            None,
        );
        after.label = Some("After".to_string());
        inner_for_fixture.widgets.record_widget(viewport_id, after);
        inner_for_fixture.widgets.finalize_registry(viewport_id);
        fixture_ok()
    });

    let result = server
        .script_eval(
            r#"local before = eguidev.capture()
eguidev.fixture("mutate", nil, { wait = false })
local diff = before:diff({ id_prefix = "status", move_epsilon = 0.1 })
return diff"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["changes"][0]["id"], "status");
    assert_eq!(json["value"]["changes"][0]["change"], "changed");
    let fields = json["value"]["changes"][0]["fields"]
        .as_array()
        .expect("fields");
    assert!(fields.iter().any(|field| field == "rect"));
    assert!(fields.iter().any(|field| field == "label"));
    assert_eq!(json["value"]["viewports_added"], json!([]));
    assert_eq!(json["value"]["viewports_removed"], json!([]));
}

#[tokio::test]
async fn script_eval_capture_diff_filters_invisible_widgets() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    record_test_snapshot(&inner, viewport_id);
    inner.widgets.clear_registry(viewport_id);
    let mut before = make_entry("hidden.status", 1, WidgetRole::Label);
    before.label = Some("Before".to_string());
    before.visible = false;
    inner.widgets.record_widget(viewport_id, before);
    inner.widgets.finalize_registry(viewport_id);

    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("mutate-hidden", "Mutate hidden widget.").ready("hidden.status"),
    ]);
    let inner_for_fixture = Arc::clone(&inner);
    set_runtime_fixture_handler(&inner, move |_call| {
        inner_for_fixture.widgets.clear_registry(viewport_id);
        let mut after = make_entry("hidden.status", 1, WidgetRole::Label);
        after.label = Some("After".to_string());
        after.visible = false;
        inner_for_fixture.widgets.record_widget(viewport_id, after);
        inner_for_fixture.widgets.finalize_registry(viewport_id);
        fixture_ok()
    });

    let result = server
        .script_eval(
            r#"local before = eguidev.capture()
eguidev.fixture("mutate-hidden", nil, { wait = false })
return {
visible_only = before:diff({ id_prefix = "hidden.status" }),
include_hidden = before:diff({ id_prefix = "hidden.status", include_invisible = true }),
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["visible_only"]["changes"], json!([]));
    assert_eq!(
        json["value"]["include_hidden"]["changes"][0]["id"],
        "hidden.status"
    );
    assert_eq!(
        json["value"]["include_hidden"]["changes"][0]["change"],
        "changed"
    );
    let fields = json["value"]["include_hidden"]["changes"][0]["fields"]
        .as_array()
        .expect("fields");
    assert!(fields.iter().any(|field| field == "label"));
}

#[tokio::test]
async fn script_eval_root_viewport_state_returns_current_snapshot() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    inner.viewports.update_viewports(&ctx);
    record_test_snapshot(&inner, egui::ViewportId::ROOT);

    let result = server
        .script_eval(
            r#"local state = eguidev.root:state()
assert(state ~= nil)
return { frame = state.frame_count, pixels_per_point = state.pixels_per_point }"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], json!({ "frame": 1, "pixels_per_point": 1 }));
}

#[tokio::test]
async fn script_eval_viewport_finds_viewport_by_title() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let secondary = egui::ViewportId::from_hash_of("secondary.lookup");

    for _ in 0..2 {
        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                title: Some("Root Lookup".to_string()),
                ..Default::default()
            },
        );
        raw_input.viewports.insert(
            secondary,
            egui::ViewportInfo {
                title: Some("Secondary Lookup".to_string()),
                ..Default::default()
            },
        );
        discard_output(ctx.run_ui(raw_input, |_| {}));
    }
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner.viewports.update_viewports(&ctx);
    record_test_snapshot(&inner, egui::ViewportId::ROOT);

    let result = server
        .script_eval(
            r#"local exact = eguidev.viewports({ title = "Secondary Lookup" })[1]
local exact_wins = eguidev.viewports({ title = "Secondary Lookup" })[1]
local contains = eguidev.viewports({ title_contains = "Secondary" })[1]
local missing = eguidev.viewports({ title = "Missing" })
return {
exact = exact ~= nil and exact.id or nil,
exact_wins = exact_wins ~= nil and exact_wins.id or nil,
contains = contains ~= nil and contains.id or nil,
missing = #missing == 0,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    let secondary_id = viewport_id_to_string(secondary);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["exact"], secondary_id);
    assert_eq!(json["value"]["exact_wins"], secondary_id);
    assert_eq!(json["value"]["contains"], secondary_id);
    assert_eq!(json["value"]["missing"], true);
}

#[tokio::test]
async fn script_eval_viewport_finds_viewport_by_name_and_focus() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let secondary = egui::ViewportId::from_hash_of("secondary.named.lookup");

    let mut raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    raw_input.viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo {
            title: Some("Root Lookup".to_string()),
            focused: Some(false),
            ..Default::default()
        },
    );
    raw_input.viewports.insert(
        secondary,
        egui::ViewportInfo {
            title: Some("Secondary Lookup".to_string()),
            focused: Some(true),
            ..Default::default()
        },
    );
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner
        .viewports
        .name_viewport(secondary, "secondary".to_string());
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner.viewports.update_viewports(&ctx);
    record_test_snapshot(&inner, egui::ViewportId::ROOT);

    let result = server
        .script_eval(
            r#"local named = eguidev.wait_viewport({ name = "secondary" })
local focused = eguidev.viewports({ focused = true })[1]
local state = named ~= nil and named:state() or nil
return {
named = named ~= nil and named.id or nil,
focused = focused ~= nil and focused.id or nil,
name = state ~= nil and state.name or nil,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    let secondary_id = viewport_id_to_string(secondary);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["named"], secondary_id);
    assert_eq!(json["value"]["focused"], secondary_id);
    assert_eq!(json["value"]["name"], "secondary");
}

#[tokio::test]
async fn script_eval_viewport_errors_on_ambiguous_title() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let first = egui::ViewportId::from_hash_of("duplicate.lookup.first");
    let second = egui::ViewportId::from_hash_of("duplicate.lookup.second");

    let mut raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    raw_input
        .viewports
        .insert(egui::ViewportId::ROOT, Default::default());
    for viewport_id in [first, second] {
        raw_input.viewports.insert(
            viewport_id,
            egui::ViewportInfo {
                title: Some("Duplicate Lookup".to_string()),
                ..Default::default()
            },
        );
    }
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner.viewports.update_viewports(&ctx);
    record_test_snapshot(&inner, egui::ViewportId::ROOT);

    let result = server
        .script_eval(
            r#"return #eguidev.viewports({ title = "Duplicate Lookup" })"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);

    assert_eq!(json["success"], true);
    assert_eq!(json["value"], 2);
}

#[tokio::test]
async fn script_eval_widget_get_state_reads_current_snapshot() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Button);
    entry.label = Some("Ready".to_string());
    entry.data = Some(json!({"kind": "status", "ready": true}));
    entry.focused = true;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"local state = eguidev.widget("status"):state()
assert(state ~= nil)
return {
role = state.role,
label = state.label,
kind = state.data.kind,
ready = state.data.ready,
focused = state.focused,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["value"],
        json!({
            "role": "button",
            "label": "Ready",
            "kind": "status",
            "ready": true,
            "focused": true
        })
    );
}

#[tokio::test]
async fn script_eval_widget_state_matches_luau_type_contract() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut labeled = make_entry("labeled", 1, WidgetRole::Button);
    labeled.label = Some("Ready".to_string());
    labeled.value = Some(WidgetValue::Bool(true));
    inner.widgets.record_widget(viewport_id, labeled);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("container", 2, WidgetRole::Unknown));
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"
local seen = {}
for _, widget in ipairs(eguidev.root:widgets()) do
local state = widget:state()
assert(state ~= nil)
assert(type(state.rect) == "table", "rect must be table")
assert(type(state.interact_rect) == "table", "interact_rect must be table")
assert(type(state.role) == "string", "role must be string")
assert(type(state.value_text) == "string", "value_text must be string")
assert(type(state.enabled) == "boolean", "enabled must be boolean")
assert(type(state.visible) == "boolean", "visible must be boolean")
assert(type(state.focused) == "boolean", "focused must be boolean")
assert(state.label == nil or type(state.label) == "string", "label must be nil|string")
assert(state.value == nil or type(state.value) ~= "userdata", "value must not be userdata")
assert(state.data == nil or type(state.data) == "table", "data must be nil|table")
assert(state.layout == nil or type(state.layout) == "table", "layout must be nil|table")
assert(state.scroll_state == nil or type(state.scroll_state) == "table", "scroll_state")
assert(state.range == nil or type(state.range) == "table", "range")
assert(state.options == nil or type(state.options) == "table", "options")
assert(state.selected == nil or type(state.selected) == "boolean", "selected")
assert(state.indeterminate == nil or type(state.indeterminate) == "boolean", "indeterminate")
assert(state.multiline == nil or type(state.multiline) == "boolean", "multiline")
assert(state.password == nil or type(state.password) == "boolean", "password")
seen[widget.id] = { label = type(state.label), value_text = state.value_text }
end
return seen
"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["labeled"]["label"], "string");
    assert_eq!(json["value"]["labeled"]["value_text"], "true");
    assert_eq!(json["value"]["container"]["label"], "nil");
    assert_eq!(json["value"]["container"]["value_text"], "");
}

#[tokio::test]
async fn script_eval_widget_state_preserves_nested_empty_arrays() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("choice", 1, WidgetRole::ComboBox);
    entry.role_state = Some(RoleState::ComboBox {
        options: Vec::new(),
    });
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"return eguidev.widget("choice"):state()"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);

    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["options"], json!([]));
}

#[tokio::test]
async fn script_eval_preserves_empty_tool_result_arrays_in_multi_returns() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"local viewport = eguidev.root
return viewport:widgets({ id_prefix = "missing" }),
viewport:widgets({ id_prefix = "also_missing" })"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], json!([[], []]));
}

#[tokio::test]
async fn script_eval_characterizes_integral_float_collapse() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "return { arg = eguidev.args.unit, literal = 1.0 }".to_string(),
            None,
            Some(ScriptEvalOptions {
                source_name: Some("integral-float.luau".to_string()),
                args: ScriptArgs::from([("unit".to_string(), ScriptArgValue::Float(1.0))]),
                ..ScriptEvalOptions::default()
            }),
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["arg"].as_i64(), Some(1));
    assert_eq!(json["value"]["literal"].as_i64(), Some(1));
}

#[tokio::test]
async fn script_eval_characterizes_nil_options_as_absent() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"return eguidev.root:widgets(nil)"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], json!([]));
}

#[tokio::test]
async fn script_eval_characterizes_nil_inside_returned_arrays() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval("return { 1, nil, 3 }".to_string(), None, None)
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], json!([1, null, 3]));
}

#[tokio::test]
async fn script_eval_reports_parse_errors() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval("local x =".to_string(), None, None)
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "parse");
}

#[tokio::test]
async fn script_eval_pcall_catches_host_tool_errors() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"
                local ok, err = pcall(function()
                    return eguidev.widget("missing"):expect({ present = true })
                end)
                local caught = err :: any
                return {
                    ok = ok,
                    err_type = type(caught),
                    err = caught.message,
                    code = caught.code,
                    rendered = tostring(caught),
                    frozen = table.isfrozen(caught),
                }
            "#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(json["value"]["ok"], false);
    assert_eq!(json["value"]["err_type"], "table");
    assert_eq!(json["value"]["code"], "expectation_failed", "{json:?}");
    assert_eq!(json["value"]["frozen"], true);
    assert!(
        json["value"]["rendered"]
            .as_str()
            .is_some_and(|message| message.starts_with("expectation_failed: ")),
        "{json:?}"
    );
    assert!(
        json["value"]["err"]
            .as_str()
            .is_some_and(|message| message.contains("missing")),
        "{json:?}"
    );
}

#[tokio::test]
async fn script_eval_widget_wait_for_visible_waits_until_visible() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.label = Some("Ready".to_string());
    entry.visible = false;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let inner_for_update = Arc::clone(&inner);
    tokio::spawn(async move {
        sleep(Duration::from_millis(5)).await;
        inner_for_update.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.label = Some("Ready".to_string());
        entry.visible = true;
        inner_for_update.widgets.record_widget(viewport_id, entry);
        inner_for_update.widgets.finalize_registry(viewport_id);
    });

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 100, poll_interval_ms = 1 })
return eguidev.widget("status"):wait({ visible = true })"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["visible"], true);
}

#[tokio::test]
async fn script_eval_pcall_does_not_catch_script_timeout() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;
    let entry = make_entry("status", 1, WidgetRole::Label);

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"
                local ok = pcall(function()
                    while true do end
                end)
                return ok
            "#
            .to_string(),
            Some(50),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false, "{json:?}");
    assert_eq!(json["error"]["type"], "timeout");
    assert!(
        json["timing"]["total_ms"]
            .as_u64()
            .is_some_and(|elapsed_ms| elapsed_ms < 500),
        "{json:?}"
    );
}

#[tokio::test]
async fn script_eval_reports_assertion_failures() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "eguidev.widget(\"missing\"):expect({ present = true }, { timeout_ms = 10, poll_interval_ms = 1 })".to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "eguidev");
    assert_eq!(json["error"]["code"], "expectation_failed");
    assert_eq!(json["assertions"][0]["passed"], false);
}

#[tokio::test]
async fn script_eval_reports_assert_widget_exists() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
    inner.widgets.finalize_registry(viewport_id);

    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "eguidev.widget(\"status\"):expect({ present = true })".to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["assertions"][0]["passed"], true);
    assert!(
        json["assertions"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("expectation"))
    );
}

#[tokio::test]
async fn script_widget_parent_and_children_follow_hierarchy() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "root",
            1,
            WidgetRole::Window,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 100.0, y: 100.0 },
            },
            None,
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "child",
            2,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            Some("root"),
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "sibling",
            3,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 12.0, y: 0.0 },
                max: Pos2 { x: 22.0, y: 10.0 },
            },
            Some("root"),
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "grand",
            4,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 12.0 },
                max: Pos2 { x: 10.0, y: 22.0 },
            },
            Some("child"),
        ),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"
                local root = eguidev.widget("root")
                local child = eguidev.widget("child")
                local child_ids = {}
                for _, widget in ipairs(root:children()) do
                    table.insert(child_ids, widget.id)
                end
                local grand_ids = {}
                for _, widget in ipairs(child:children()) do
                    table.insert(grand_ids, widget.id)
                end
                local grand_parent = eguidev.widget("grand"):parent()
                return {
                    child_count = #child_ids,
                    child_ids = table.concat(child_ids, ","),
                    grand_count = #grand_ids,
                    grand_ids = table.concat(grand_ids, ","),
                    grand_parent_child_count = grand_parent and #grand_parent:children() or 0,
                }
            "#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    let value = &json["value"];
    assert_eq!(value.get("child_count").and_then(Value::as_u64), Some(2));
    assert_eq!(
        value.get("child_ids").and_then(Value::as_str),
        Some("child,sibling")
    );
    assert_eq!(value.get("grand_count").and_then(Value::as_u64), Some(1));
    assert_eq!(
        value.get("grand_ids").and_then(Value::as_str),
        Some("grand")
    );
    assert_eq!(
        value
            .get("grand_parent_child_count")
            .and_then(Value::as_u64),
        Some(1)
    );
}

#[tokio::test]
async fn script_widget_state_and_wait_for_use_widget_state() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut button = make_entry("status", 1, WidgetRole::Button);
    button.label = Some("Ready".to_string());
    button.value = Some(WidgetValue::Text("Ready".to_string()));
    button.focused = true;
    inner.widgets.record_widget(viewport_id, button);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"
                local widget = eguidev.widget("status")
                local state = widget:state()
                local waited = widget:wait(function(current)
                    return current ~= nil
                        and current.focused
                        and current.value ~= nil
                        and current.value == "Ready"
                end, { timeout_ms = 20, poll_interval_ms = 1 })
                assert(state ~= nil)
                return {
                    role = state.role,
                    focused = state.focused,
                    value_text = state.value,
                    waited = waited ~= nil,
                    waited_role = waited ~= nil and waited.role or nil,
                }
            "#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    let value = &json["value"];
    assert_eq!(value.get("role").and_then(Value::as_str), Some("button"));
    assert_eq!(
        value.get("value_text").and_then(Value::as_str),
        Some("Ready")
    );
    assert_eq!(value.get("focused").and_then(Value::as_bool), Some(true));
    assert_eq!(value.get("waited").and_then(Value::as_bool), Some(true));
    assert_eq!(
        value.get("waited_role").and_then(Value::as_str),
        Some("button")
    );
}
