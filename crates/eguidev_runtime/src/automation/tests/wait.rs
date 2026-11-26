//! Tests of waits and of the settle report.

use super::*;

#[tokio::test]
async fn script_eval_wait_viewport_reports_ambiguous_and_absent_filters() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let first = egui::ViewportId::from_hash_of("duplicate.name.first");
    let second = egui::ViewportId::from_hash_of("duplicate.name.second");

    let mut raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    raw_input
        .viewports
        .insert(egui::ViewportId::ROOT, Default::default());
    raw_input.viewports.insert(
        first,
        egui::ViewportInfo {
            title: Some("Duplicate First".to_string()),
            ..Default::default()
        },
    );
    raw_input.viewports.insert(
        second,
        egui::ViewportInfo {
            title: Some("Duplicate Second".to_string()),
            ..Default::default()
        },
    );
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner.viewports.update_viewports(&ctx);
    record_test_snapshot(&inner, egui::ViewportId::ROOT);

    let result = server
        .script_eval(
            r#"local ambiguousOk, ambiguousError = pcall(function()
return eguidev.wait_viewport(
    { title_contains = "Duplicate" },
    { timeout_ms = 20, poll_interval_ms = 1 }
)
end)
local absentOk, absentError = pcall(function()
return eguidev.wait_viewport(
    { name = "missing" },
    { timeout_ms = 20, poll_interval_ms = 1 }
)
end)
local ambiguous: any = ambiguousError
local absent: any = absentError
return {
ambiguous_ok = ambiguousOk,
ambiguous_code = ambiguous.code,
ambiguous_details = ambiguous.details,
absent_ok = absentOk,
absent_code = absent.code,
absent_details = absent.details,
}"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);

    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(json["value"]["ambiguous_ok"], false, "{json:?}");
    assert_eq!(json["value"]["ambiguous_code"], "not_found", "{json:?}");
    assert_eq!(
        json["value"]["ambiguous_details"]["reason"], "ambiguous",
        "{json:?}"
    );
    assert_eq!(
        json["value"]["ambiguous_details"]["candidates"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "{json:?}"
    );
    assert_eq!(json["value"]["absent_ok"], false, "{json:?}");
    assert_eq!(json["value"]["absent_code"], "not_found", "{json:?}");
    assert_eq!(
        json["value"]["absent_details"]["reason"], "absent",
        "{json:?}"
    );
}

#[tokio::test]
async fn script_eval_wait_for_widget_closure_matches() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.label = Some("Ready".to_string());
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 50 }) return eguidev.widget("status"):wait(function(w) return w ~= nil and w.label == "Ready" end)"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["label"], "Ready");
}

#[tokio::test]
async fn script_eval_wait_for_widget_closure_compares_integer_values() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("choice", 1, WidgetRole::ComboBox);
    entry.value = Some(WidgetValue::Int(2));
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 50 }) return eguidev.widget("choice"):wait(function(widget) return widget ~= nil and widget.value == 2 end)"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(json["value"]["value"], 2);
}

#[tokio::test]
async fn script_eval_wait_for_widget_absent() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 30 }) eguidev.widget("missing"):wait({ present = false }) return true"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert_eq!(json["value"], true);
}

#[tokio::test]
async fn script_eval_wait_for_widget_visible_matches_existing_widget() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.label = Some("Ready".to_string());
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 50 }) return eguidev.widget("status"):wait({ visible = true })"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["visible"], true);
    assert_eq!(json["value"]["label"], "Ready");
}

#[tokio::test]
async fn script_eval_wait_reports_its_whole_timeout() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    discard_output(ctx.run_ui(egui::RawInput::default(), |_| {}));
    capture_test_frame(&inner, &ctx);
    // Frames keep arriving, so the wait loop runs many slices, each with a
    // shorter remaining time, before its deadline passes.
    let inner_for_frames = Arc::clone(&inner);
    let frames = tokio::spawn(async move {
        loop {
            yield_now().await;
            capture_test_frame(&inner_for_frames, &ctx);
        }
    });

    let result = server
        .script_eval(
            "eguidev.wait(function() return false end, { timeout_ms = 60, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    frames.abort();
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false, "{json:?}");
    assert_eq!(json["error"]["code"], "timeout", "{json:?}");
    assert_eq!(json["error"]["details"]["operation"], "wait", "{json:?}");
    assert_eq!(
        json["error"]["details"]["details"]["timeout_ms"], 60,
        "the error names the timeout of the wait, not its last slice: {json:?}"
    );
}

#[tokio::test]
async fn script_eval_wait_for_widget_visible_waits_for_widget_to_appear() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let inner_for_update = Arc::clone(&inner);
    tokio::spawn(async move {
        sleep(Duration::from_millis(5)).await;
        inner_for_update.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.label = Some("Ready".to_string());
        inner_for_update.widgets.record_widget(viewport_id, entry);
        inner_for_update.widgets.finalize_registry(viewport_id);
    });

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 100, poll_interval_ms = 1 }) return eguidev.widget("status"):wait({ visible = true })"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["visible"], true);
    assert_eq!(json["value"]["label"], "Ready");
}

#[tokio::test]
async fn script_eval_wait_for_widget_visible_times_out_when_hidden() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.visible = false;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 30, poll_interval_ms = 1 }) return eguidev.widget("status"):wait({ visible = true })"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "timeout");
    assert_eq!(json["error"]["details"]["kind"], "widget");
}

#[tokio::test]
async fn script_eval_wait_for_widget_closure_times_out() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let entry = make_entry("status", 1, WidgetRole::Label);
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 30 }) return eguidev.widget("status"):wait(function(w) return w ~= nil and w.label == "Never" end)"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "timeout");
    assert_eq!(json["error"]["details"]["kind"], "widget");
}

#[tokio::test]
async fn script_eval_wait_for_widget_closure_respects_script_timeout() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;
    let entry = make_entry("status", 1, WidgetRole::Label);

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.configure({ timeout_ms = 5000, poll_interval_ms = 250 }) return eguidev.widget("status"):wait(function(w) return w ~= nil and w.label == "Never" end)"#
                .to_string(),
            Some(50),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "timeout");
    assert!(
        json["timing"]["total_ms"]
            .as_u64()
            .is_some_and(|elapsed_ms| elapsed_ms < 500),
        "{json:?}"
    );
}

#[tokio::test]
async fn script_eval_wait_for_frames_returns_frame_count() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval("return eguidev.wait_frames(0)".to_string(), None, None)
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], 0);
}

#[tokio::test]
async fn wait_for_fresh_capture_waits_for_new_snapshot() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    capture_test_frame(&inner, &ctx);

    let inner_for_capture = Arc::clone(&inner);
    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        let capture_ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(capture_ctx.run_ui(raw_input, |_| {}));
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    server
        .wait_for_fresh_capture(None, Some(500), Some(1))
        .await
        .expect("wait_for_fresh_capture");
}

#[tokio::test]
async fn shared_scroll_ready_condition_waits_for_stable_scroll_state() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.widgets.clear_registry(egui::ViewportId::ROOT);
    inner.widgets.record_widget(
        egui::ViewportId::ROOT,
        make_scroll_entry(
            "scroll",
            1,
            Vec2 { x: 0.0, y: 150.0 },
            Vec2 { x: 0.0, y: 400.0 },
        ),
    );
    inner.widgets.finalize_registry(egui::ViewportId::ROOT);
    capture_test_frame(&inner, &ctx);

    let inner_for_capture = Arc::clone(&inner);
    tokio::spawn(async move {
        let capture_ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(capture_ctx.run_ui(raw_input, |_| {}));
        sleep(Duration::from_millis(50)).await;
        inner_for_capture
            .widgets
            .clear_registry(egui::ViewportId::ROOT);
        inner_for_capture.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_scroll_entry(
                "scroll",
                1,
                Vec2 { x: 0.0, y: 150.0 },
                Vec2 { x: 0.0, y: 400.0 },
            ),
        );
        inner_for_capture
            .widgets
            .finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner_for_capture, &capture_ctx);
        sleep(Duration::from_millis(60)).await;
        inner_for_capture
            .widgets
            .clear_registry(egui::ViewportId::ROOT);
        inner_for_capture.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_scroll_entry(
                "scroll",
                1,
                Vec2 { x: 0.0, y: 150.0 },
                Vec2 { x: 0.0, y: 400.0 },
            ),
        );
        inner_for_capture
            .widgets
            .finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    let result = server
        .script_eval(
            r#"local state = eguidev.widget("scroll"):wait(
{ scroll_ready = true },
{ timeout_ms = 500, poll_interval_ms = 1 }
)
assert(state ~= nil and state.scroll_state ~= nil)
return state.scroll_state.offset.y"#
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["value"], 150.0);
}

#[test]
fn settle_root_fresh_frame_requires_root_capture() {
    let inner = Arc::new(Inner::new());
    inner.advance_frame();
    inner.advance_frame();
    let report = settle_report(&inner, egui::ViewportId::ROOT, 0, 0, 0);
    let fresh = report
        .phases
        .iter()
        .find(|phase| matches!(phase.phase, SettlePhase::FreshFrame))
        .expect("fresh_frame");
    assert!(!fresh.complete, "{fresh:?}");
}

#[tokio::test]
async fn wait_for_widget_state_matches_existing_widget() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.value = Some(WidgetValue::Text("Ready".to_string()));
    entry.label = Some("Ready".to_string());
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .wait_for_widget_state(
            None,
            widget_ref_id("status"),
            Some(50),
            Some(1),
            "predicate",
            |widget| {
                widget.and_then(|widget| widget.value.as_ref())
                    == Some(&WidgetValue::Text("Ready".to_string()))
            },
        )
        .await
        .expect("wait for widget");
    assert!(result.is_some());
}

#[tokio::test]
async fn wait_for_widget_state_matches_missing_widget() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));

    let result = server
        .wait_for_widget_state(
            None,
            widget_ref_id("missing"),
            Some(50),
            Some(1),
            "predicate",
            |widget| widget.is_none(),
        )
        .await
        .expect("wait for widget");
    assert!(result.is_none());
}

#[tokio::test]
async fn wait_for_widget_state_matches_when_widget_becomes_visible() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("status", 1, WidgetRole::Label);
    entry.visible = false;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let inner_for_update = Arc::clone(&inner);
    tokio::spawn(async move {
        yield_now().await;
        inner_for_update.widgets.clear_registry(viewport_id);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.visible = true;
        inner_for_update.widgets.record_widget(viewport_id, entry);
        inner_for_update.widgets.finalize_registry(viewport_id);
    });

    let result = server
        .wait_for_widget_state(
            None,
            widget_ref_id("status"),
            Some(100),
            Some(1),
            "predicate",
            |widget| widget.is_some_and(|widget| widget.visible),
        )
        .await
        .expect("wait for visible widget");
    assert!(result.is_some_and(|widget| widget.visible));
}

#[tokio::test]
async fn wait_for_widget_state_matches_when_widget_appears() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.finalize_registry(viewport_id);

    let inner_for_update = Arc::clone(&inner);
    tokio::spawn(async move {
        yield_now().await;
        inner_for_update.widgets.clear_registry(viewport_id);
        inner_for_update
            .widgets
            .record_widget(viewport_id, make_entry("status", 1, WidgetRole::Label));
        inner_for_update.widgets.finalize_registry(viewport_id);
    });

    let result = server
        .wait_for_widget_state(
            None,
            widget_ref_id("status"),
            Some(100),
            Some(1),
            "predicate",
            |widget| widget.is_some(),
        )
        .await
        .expect("wait for appearing widget");
    assert!(result.is_some_and(|widget| widget.visible));
}

#[tokio::test]
async fn wait_for_settle_matches_when_idle() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;

    // Run one frame so that the input snapshot is populated.
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    capture_test_frame(&inner, &ctx);
    let inner_for_capture = Arc::clone(&inner);
    let capture_ctx = ctx.clone();
    tokio::spawn(async move {
        yield_now().await;
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    server
        .wait_for_settle(None, Some(50), Some(1))
        .await
        .expect("wait_for_settle");
}

#[tokio::test]
async fn wait_for_settle_requires_a_new_frame() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;

    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    capture_test_frame(&inner, &ctx);

    let error = server
        .wait_for_settle(None, Some(10), Some(1))
        .await
        .expect_err("wait_for_settle should time out");
    assert_eq!(error.code, "timeout");
    let phases = error
        .structured
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("details"))
        .and_then(|details| details.get("phases"))
        .and_then(Value::as_array)
        .expect("settle phases");
    assert!(
        phases
            .iter()
            .any(|phase| phase.get("phase").and_then(Value::as_str) == Some("fresh_frame"))
    );
}

#[tokio::test]
async fn wait_for_settle_reports_busy_runtime_idle_phase() {
    let idle = Arc::new(AtomicBool::new(false));
    let idle_for_provider = Arc::clone(&idle);
    let devmcp = attach_for_tests(
        DevMcp::new()
            .keep_alive(false)
            .on_idle(move || idle_for_provider.load(AtomicOrdering::Relaxed))
            .expect("idle provider"),
    );
    let inner = devmcp.inner_arc().expect("attached inner");
    let runtime = Runtime::for_devmcp(&devmcp).expect("attached runtime");
    let server = DevMcpServer::with_runtime(Arc::clone(&inner), runtime);
    let ctx = egui::Context::default();

    run_instrumented_test_frame(
        &devmcp,
        &ctx,
        egui::ViewportId::ROOT,
        &[egui::ViewportId::ROOT],
    );
    let devmcp_for_frame = devmcp.clone();
    let ctx_for_frame = ctx.clone();
    tokio::spawn(async move {
        yield_now().await;
        run_instrumented_test_frame(
            &devmcp_for_frame,
            &ctx_for_frame,
            egui::ViewportId::ROOT,
            &[egui::ViewportId::ROOT],
        );
    });

    let error = server
        .wait_for_settle(None, Some(20), Some(1))
        .await
        .expect_err("busy app idle provider should block settle");
    let phases = error
        .structured
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("details"))
        .and_then(|details| details.get("phases"))
        .and_then(Value::as_array)
        .expect("settle phases");
    assert!(phases.iter().any(|phase| {
        phase.get("phase").and_then(Value::as_str) == Some("app_idle")
            && phase.get("complete").and_then(Value::as_bool) == Some(false)
    }));
}

#[tokio::test]
async fn wait_for_settle_observes_ui_idle_on_child_viewport() {
    let idle = Arc::new(AtomicBool::new(false));
    let idle_for_provider = Arc::clone(&idle);
    let devmcp = attach_for_tests(
        DevMcp::new()
            .keep_alive(false)
            .on_idle_ui(move |_| idle_for_provider.load(AtomicOrdering::Relaxed))
            .expect("UI idle provider"),
    );
    let inner = devmcp.inner_arc().expect("attached inner");
    let runtime = Runtime::for_devmcp(&devmcp).expect("attached runtime");
    let server = DevMcpServer::with_runtime(Arc::clone(&inner), runtime);
    let root_ctx = egui::Context::default();
    let child_ctx = egui::Context::default();
    let child = egui::ViewportId::from_hash_of("ui-idle-child");
    let live_viewports = [egui::ViewportId::ROOT, child];

    run_instrumented_test_frame(&devmcp, &root_ctx, egui::ViewportId::ROOT, &live_viewports);
    run_instrumented_test_frame(&devmcp, &child_ctx, child, &live_viewports);

    idle.store(true, AtomicOrdering::Relaxed);

    let devmcp_for_frame = devmcp.clone();
    let root_ctx_for_frame = root_ctx.clone();
    let child_ctx_for_frame = child_ctx.clone();
    let frame_task = tokio::spawn(async move {
        yield_now().await;
        run_instrumented_test_frame(
            &devmcp_for_frame,
            &root_ctx_for_frame,
            egui::ViewportId::ROOT,
            &live_viewports,
        );
        run_instrumented_test_frame(
            &devmcp_for_frame,
            &child_ctx_for_frame,
            child,
            &live_viewports,
        );
    });

    let result = server
        .wait_for_settle(Some(viewport_id_to_string(child)), Some(100), Some(1))
        .await;
    frame_task.await.expect("frame task");
    result.expect("child settle should refresh root UI idle state");
}

#[tokio::test]
async fn wait_for_settle_requires_clean_frame_after_action_drain() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;

    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    capture_test_frame(&inner, &ctx);
    inner.queue_action(
        viewport_id,
        InputAction::PointerMove {
            pos: Pos2 { x: 1.0, y: 1.0 },
        },
    );

    let inner_for_capture = Arc::clone(&inner);
    let capture_ctx = ctx.clone();
    tokio::spawn(async move {
        yield_now().await;
        drain_test_actions(&inner_for_capture, viewport_id);
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    let error = server
        .wait_for_settle(None, Some(20), Some(1))
        .await
        .expect_err("wait_for_settle should require a clean post-action frame");
    assert_eq!(error.code, "timeout");
}

#[tokio::test]
async fn wait_for_settle_matches_clean_frame_after_action_drain() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;

    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    capture_test_frame(&inner, &ctx);
    inner.queue_action(
        viewport_id,
        InputAction::PointerMove {
            pos: Pos2 { x: 1.0, y: 1.0 },
        },
    );

    let inner_for_capture = Arc::clone(&inner);
    let capture_ctx = ctx.clone();
    tokio::spawn(async move {
        yield_now().await;
        drain_test_actions(&inner_for_capture, viewport_id);
        capture_test_frame(&inner_for_capture, &capture_ctx);
        yield_now().await;
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    let report = server
        .wait_for_settle(None, Some(100), Some(1))
        .await
        .expect("wait_for_settle");
    assert!(report.settled);
    assert!(
        report
            .phases
            .iter()
            .any(|phase| matches!(phase.phase, SettlePhase::CleanCapture))
    );
    assert!(inner.frame_count() >= 3);
}

#[tokio::test]
async fn wait_for_settle_matches_when_child_viewport_closes_after_action_drain() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::from_hash_of("closing-child");

    let mut raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    raw_input
        .viewports
        .insert(egui::ViewportId::ROOT, Default::default());
    raw_input.viewports.insert(viewport_id, Default::default());
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.viewports.update_viewports(&ctx);
    record_test_snapshot(&inner, viewport_id);

    inner.queue_action(
        viewport_id,
        InputAction::PointerMove {
            pos: Pos2 { x: 1.0, y: 1.0 },
        },
    );

    let inner_for_capture = Arc::clone(&inner);
    let capture_ctx = ctx.clone();
    tokio::spawn(async move {
        yield_now().await;
        drain_test_actions(&inner_for_capture, viewport_id);
        record_test_snapshot(&inner_for_capture, viewport_id);

        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        discard_output(capture_ctx.run_ui(raw_input, |_| {}));
        inner_for_capture.viewports.update_viewports(&capture_ctx);
        record_test_snapshot(&inner_for_capture, egui::ViewportId::ROOT);
    });

    server
        .wait_for_settle(Some(viewport_id_to_string(viewport_id)), Some(100), Some(1))
        .await
        .expect("wait_for_settle should match after child viewport closes");
}
