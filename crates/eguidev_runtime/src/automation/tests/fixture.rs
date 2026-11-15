//! Tests of fixture application and fixture readiness.

use super::*;

#[tokio::test]
async fn fixture_apply_applies_handler_without_waiting_for_a_new_frame() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("test_fixture", "Test fixture.").ready("status"),
    ]);
    set_runtime_fixture_handler(&inner, |_call| fixture_ok());
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner
        .viewports
        .capture_input_snapshot(&ctx, inner.fixture_epoch(), inner.frame_count() + 1);
    let server = DevMcpServer::new(Arc::clone(&inner));
    server
        .fixture_apply("test_fixture".to_string(), None)
        .await
        .expect("fixture_apply result");
}

#[tokio::test]
async fn script_fixture_waits_for_preconditions_before_handler() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("delayed", "Delayed fixture.")
            .precondition_value("ready", WidgetValue::Bool(true))
            .ready("status"),
    ]);
    let handler_called = Arc::new(AtomicBool::new(false));

    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    let mut ready = make_entry("ready", 1, WidgetRole::Label);
    ready.value = Some(WidgetValue::Bool(false));
    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(viewport_id, ready);
    inner.widgets.finalize_registry(viewport_id);
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.capture_context(viewport_id, &ctx);
    inner
        .viewports
        .capture_input_snapshot(&ctx, inner.fixture_epoch(), inner.frame_count() + 1);

    let inner_for_fixture = Arc::clone(&inner);
    let ctx_for_fixture = ctx.clone();
    let handler_called_for_fixture = Arc::clone(&handler_called);
    set_runtime_fixture_handler(&inner, move |_call| {
        let ready = resolve_widget(&inner_for_fixture, None, &widget_ref_id("ready"))
            .expect("precondition widget");
        assert_eq!(ready.value, Some(WidgetValue::Bool(true)));
        handler_called_for_fixture.store(true, AtomicOrdering::Release);
        inner_for_fixture.widgets.clear_registry(viewport_id);
        inner_for_fixture.widgets.record_widget(viewport_id, ready);
        inner_for_fixture
            .widgets
            .record_widget(viewport_id, make_entry("status", 2, WidgetRole::Label));
        inner_for_fixture.widgets.finalize_registry(viewport_id);
        let inner_for_ready = Arc::clone(&inner_for_fixture);
        let ctx_for_ready = ctx_for_fixture.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(10)).await;
            let raw_input = egui::RawInput {
                viewport_id,
                ..Default::default()
            };
            discard_output(ctx_for_ready.run_ui(raw_input, |_| {}));
            capture_test_frame(&inner_for_ready, &ctx_for_ready);
        });
        fixture_ok()
    });

    let inner_for_update = Arc::clone(&inner);
    let ctx_for_update = ctx.clone();
    tokio::spawn(async move {
        yield_now().await;
        let mut ready = make_entry("ready", 1, WidgetRole::Label);
        ready.value = Some(WidgetValue::Bool(true));
        inner_for_update.widgets.clear_registry(viewport_id);
        inner_for_update.widgets.record_widget(viewport_id, ready);
        inner_for_update.widgets.finalize_registry(viewport_id);
        let raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        discard_output(ctx_for_update.run_ui(raw_input, |_| {}));
        inner_for_update.capture_context(viewport_id, &ctx_for_update);
        capture_test_frame(&inner_for_update, &ctx_for_update);
    });

    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server
        .script_eval(
            "return eguidev.fixture(\"delayed\", nil, { timeout_ms = 2000, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
    assert!(handler_called.load(AtomicOrdering::Acquire));
}

#[tokio::test]
async fn fixture_precondition_timeout_does_not_apply_handler() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("blocked", "Blocked fixture.")
            .precondition_value("ready", WidgetValue::Bool(true))
            .ready("status"),
    ]);
    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_for_fixture = Arc::clone(&handler_called);
    set_runtime_fixture_handler(&inner, move |_call| {
        handler_called_for_fixture.store(true, AtomicOrdering::Release);
        fixture_ok()
    });

    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server
        .script_eval(
            "return eguidev.fixture(\"blocked\", nil, { timeout_ms = 20, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false, "{json:?}");
    assert_eq!(json["error"]["type"], "eguidev", "{json:?}");
    assert_eq!(json["error"]["code"], "fixture_failed", "{json:?}");
    assert_eq!(
        json["error"]["details"]["details"]["reason"], "precondition_unmet",
        "{json:?}"
    );
    assert_eq!(
        json["error"]["details"]["details"]["widget"], "ready",
        "{json:?}"
    );
    assert!(!handler_called.load(AtomicOrdering::Relaxed));
}

#[tokio::test]
async fn fixture_returns_error_when_handler_fails() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("broken", "Broken fixture.").ready("status"),
    ]);
    inner
        .fixtures
        .set_handler(FixtureHandler::Runtime(Arc::new(|_call| {
            Err(FixtureError::new("fixture_failed", "fixture failed"))
        })))
        .expect("fixture handler");
    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server.fixture_apply("broken".to_string(), None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn fixture_returns_error_when_no_handler_registered() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("no_handler", "No handler fixture.").ready("status"),
    ]);
    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server.fixture_apply("no_handler".to_string(), None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn script_eval_fixture_wait_false_succeeds_without_a_new_frame() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("slow", "Slow fixture.").ready("status"),
    ]);
    set_runtime_fixture_handler(&inner, |_call| fixture_ok());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server
        .script_eval(
            "eguidev.fixture(\"slow\", nil, { wait = false })".to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
}

#[tokio::test]
async fn script_eval_returns_sorted_fixtures() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("zeta", "Last fixture.").ready("status"),
        FixtureSpec::new("alpha", "First fixture.").ready("status"),
    ]);
    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server
        .script_eval(
            r#"local catalog = eguidev.fixtures()
return { first = catalog[1].name, count = #catalog }"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], json!({ "first": "alpha", "count": 2 }));
}

#[tokio::test]
async fn script_eval_fixture_timeout_reports_fresh_frame_diagnostics() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("stale", "Stale fixture.").ready("status"),
    ]);
    set_runtime_fixture_handler(&inner, |_call| fixture_ok());

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    let entry = make_entry("status", 1, WidgetRole::Label);
    inner.widgets.clear_registry(egui::ViewportId::ROOT);
    inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
    inner.widgets.finalize_registry(egui::ViewportId::ROOT);
    capture_test_frame(&inner, &ctx);

    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server
        .script_eval(
            "eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 }) eguidev.fixture(\"stale\")"
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "timeout");
    assert_eq!(json["error"]["details"]["kind"], "frames");
    assert!(
        json["error"]["message"]
            .as_str()
            .expect("timeout message")
            .contains("Timed out waiting for 1 frame"),
        "timeout should name the fresh-frame requirement"
    );
    assert!(
        json["error"]["details"]["observation"]["diagnosis"]
            .as_str()
            .expect("observation diagnosis")
            .contains("No target viewport frames were observed"),
        "timeout details should explain the missing fresh capture"
    );
}

#[tokio::test]
async fn fixture_timeout_reports_zero_frame_observation_once() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("blocked", "Blocked fixture.").ready("status"),
    ]);
    set_runtime_fixture_handler(&inner, |_call| fixture_ok());
    let server = DevMcpServer::new(Arc::clone(&inner));

    let result = server
        .script_eval(
            "eguidev.configure({ timeout_ms = 20, poll_interval_ms = 1 }) eguidev.fixture(\"blocked\")"
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    let message = json["error"]["message"].as_str().expect("timeout message");
    assert_eq!(
        message
            .matches("Ensure the app wraps rendered frames")
            .count(),
        1,
        "guidance sentence should appear once"
    );
    let details = &json["error"]["details"];
    assert_eq!(details["observation"]["target_viewport_id"], "root");
    assert_eq!(details["observation"]["frames_observed"], 0);
    assert_eq!(details["observation"]["stalled"], true);
    assert_eq!(
        details["observation"]["diagnosis"],
        "No target viewport frames were observed while waiting."
    );
}

#[test]
fn frame_started_before_fixture_epoch_does_not_satisfy_fixture_capture() {
    let inner = Arc::new(Inner::new());
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));

    inner.begin_frame(viewport_id);
    let fixture_epoch = inner.begin_fixture_epoch();
    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("menu.item", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);
    inner.viewports.capture_input_snapshot(
        &ctx,
        inner
            .finish_frame_fixture_epoch(viewport_id)
            .expect("frame start epoch"),
        inner.frame_count() + 1,
    );

    let capture = inner
        .viewports
        .capture_snapshot(viewport_id)
        .expect("capture snapshot");
    assert!(
        capture.fixture_epoch < fixture_epoch,
        "mid-frame captures must not be stamped with the new fixture epoch"
    );
}

#[tokio::test]
async fn script_fixture_rejects_late_captures_from_the_previous_fixture_epoch() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("repeated", "Repeated fixture.").ready("status"),
    ]);
    let called = Arc::new(AtomicBool::new(false));
    let called_by_handler = Arc::clone(&called);
    set_runtime_fixture_handler(&inner, move |_call| {
        called_by_handler.store(true, AtomicOrdering::Release);
        fixture_ok()
    });
    let previous_epoch = inner.fixture_epoch();
    let publishing = Arc::clone(&inner);
    let publisher = tokio::spawn(async move {
        let ctx = egui::Context::default();
        discard_output(ctx.run_ui(egui::RawInput::default(), |_| {}));
        while !called.load(AtomicOrdering::Acquire) {
            sleep(Duration::from_millis(1)).await;
        }
        loop {
            let viewport = egui::ViewportId::ROOT;
            publishing.widgets.clear_registry(viewport);
            publishing
                .widgets
                .record_widget(viewport, make_entry("status", 1, WidgetRole::Label));
            publishing.widgets.finalize_registry(viewport);
            publishing.viewports.capture_input_snapshot(
                &ctx,
                previous_epoch,
                publishing.frame_count() + 1,
            );
            publishing.advance_frame();
            sleep(Duration::from_millis(1)).await;
        }
    });
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            "return eguidev.fixture('repeated', nil, { timeout_ms = 100, poll_interval_ms = 1 })"
                .to_owned(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script result");
    publisher.abort();
    let _stopped = publisher.await;
    let json = parse_script_eval_json(&result);
    assert_eq!(
        json["success"], false,
        "a stale capture must not make the fixture ready: {json:?}"
    );
    assert_eq!(json["error"]["type"], "timeout");
    assert_eq!(json["error"]["details"]["kind"], "frames");
}

#[tokio::test]
async fn fixture_waits_for_multiviewport_anchor_capture() {
    let inner = Arc::new(Inner::new());
    let secondary = egui::ViewportId::from_hash_of("fixture.secondary");
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("multi", "Multi viewport fixture.").ready_in("status", secondary),
    ]);
    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_for_fixture = Arc::clone(&handler_called);
    set_runtime_fixture_handler(&inner, move |_call| {
        handler_called_for_fixture.store(true, AtomicOrdering::Release);
        fixture_ok()
    });

    let root_ctx = egui::Context::default();
    let mut root_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    root_input
        .viewports
        .insert(egui::ViewportId::ROOT, Default::default());
    root_input.viewports.insert(secondary, Default::default());
    discard_output(root_ctx.run_ui(root_input, |_| {}));
    inner.capture_context(egui::ViewportId::ROOT, &root_ctx);
    inner.viewports.update_viewports(&root_ctx);
    capture_test_frame(&inner, &root_ctx);

    let server = DevMcpServer::new(Arc::clone(&inner));
    let inner_for_capture = Arc::clone(&inner);
    let root_ctx_for_capture = root_ctx.clone();
    let handler_called_for_capture = Arc::clone(&handler_called);
    tokio::spawn(async move {
        while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
            sleep(Duration::from_millis(1)).await;
        }
        sleep(Duration::from_millis(10)).await;
        capture_test_frame(&inner_for_capture, &root_ctx_for_capture);
        inner_for_capture.widgets.clear_registry(secondary);
        let mut entry = make_entry("status", 1, WidgetRole::Label);
        entry.viewport_id = viewport_id_to_string(secondary);
        inner_for_capture.widgets.record_widget(secondary, entry);
        inner_for_capture.widgets.finalize_registry(secondary);
        record_test_snapshot(&inner_for_capture, secondary);
    });

    let result = server
        .script_eval(
            "return eguidev.fixture(\"multi\", nil, { timeout_ms = 200, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
}

#[tokio::test]
async fn fixture_data_anchor_waits_for_the_installed_data() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("viewer.mixed", "Analysed and unanalysed games.").ready_data(
            "status.summary",
            "/analysed",
            3,
        ),
    ]);
    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_for_fixture = Arc::clone(&handler_called);
    set_runtime_fixture_handler(&inner, move |_call| {
        handler_called_for_fixture.store(true, AtomicOrdering::Release);
        fixture_ok()
    });

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));

    // The widget set never changes; only the published data does.
    let publish = |analysed: i64| {
        let mut entry = make_entry("status.summary", 1, WidgetRole::Label);
        entry.data = Some(json!({ "analysed": analysed }));
        inner.widgets.clear_registry(egui::ViewportId::ROOT);
        inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
        inner.widgets.finalize_registry(egui::ViewportId::ROOT);
    };
    publish(0);
    capture_test_frame(&inner, &ctx);

    let server = DevMcpServer::new(Arc::clone(&inner));
    let inner_for_capture = Arc::clone(&inner);
    let handler_called_for_capture = Arc::clone(&handler_called);
    tokio::spawn(async move {
        let capture_ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(capture_ctx.run_ui(raw_input, |_| {}));
        while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
            sleep(Duration::from_millis(1)).await;
        }
        sleep(Duration::from_millis(10)).await;
        let mut entry = make_entry("status.summary", 1, WidgetRole::Label);
        entry.data = Some(json!({ "analysed": 3 }));
        inner_for_capture
            .widgets
            .clear_registry(egui::ViewportId::ROOT);
        inner_for_capture
            .widgets
            .record_widget(egui::ViewportId::ROOT, entry);
        inner_for_capture
            .widgets
            .finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    let result = server
        .script_eval(
            "return eguidev.fixture(\"viewer.mixed\", nil, { timeout_ms = 500, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
}

#[tokio::test]
async fn fixture_data_anchor_times_out_on_unmatched_data() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("viewer.mixed", "Analysed and unanalysed games.").ready_data(
            "status.summary",
            "/analysed",
            3,
        ),
    ]);
    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_for_fixture = Arc::clone(&handler_called);
    set_runtime_fixture_handler(&inner, move |_call| {
        handler_called_for_fixture.store(true, AtomicOrdering::Release);
        fixture_ok()
    });

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    let mut entry = make_entry("status.summary", 1, WidgetRole::Label);
    entry.data = Some(json!({ "analysed": 1 }));
    inner.widgets.clear_registry(egui::ViewportId::ROOT);
    inner.widgets.record_widget(egui::ViewportId::ROOT, entry);
    inner.widgets.finalize_registry(egui::ViewportId::ROOT);
    capture_test_frame(&inner, &ctx);

    let server = DevMcpServer::new(Arc::clone(&inner));
    let inner_for_capture = Arc::clone(&inner);
    let ctx_for_capture = ctx.clone();
    let handler_called_for_capture = Arc::clone(&handler_called);
    tokio::spawn(async move {
        while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
            sleep(Duration::from_millis(1)).await;
        }
        sleep(Duration::from_millis(10)).await;
        capture_test_frame(&inner_for_capture, &ctx_for_capture);
    });
    let result = server
        .script_eval(
            "return eguidev.fixture(\"viewer.mixed\", nil, { timeout_ms = 80, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false, "{json:?}");
    assert_eq!(json["error"]["type"], "timeout");
    assert!(
        json["error"]["details"]["widget"]["data"]["analysed"] == 1,
        "timeout should preserve the unmatched widget data: {json:?}"
    );
}

#[tokio::test]
async fn fixture_waits_for_scroll_anchor_stability() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("scroll", "Scroll fixture.").ready_scroll_at(
            "scroll",
            Vec2 { x: 0.0, y: 300.0 },
            1.0,
        ),
    ]);
    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_for_fixture = Arc::clone(&handler_called);
    set_runtime_fixture_handler(&inner, move |_call| {
        handler_called_for_fixture.store(true, AtomicOrdering::Release);
        fixture_ok()
    });

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    capture_test_frame(&inner, &ctx);

    let server = DevMcpServer::new(Arc::clone(&inner));
    let inner_for_capture = Arc::clone(&inner);
    let handler_called_for_capture = Arc::clone(&handler_called);
    tokio::spawn(async move {
        let capture_ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        discard_output(capture_ctx.run_ui(raw_input, |_| {}));
        while !handler_called_for_capture.load(AtomicOrdering::Acquire) {
            sleep(Duration::from_millis(1)).await;
        }
        sleep(Duration::from_millis(10)).await;
        inner_for_capture
            .widgets
            .clear_registry(egui::ViewportId::ROOT);
        inner_for_capture.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_scroll_entry(
                "scroll",
                1,
                Vec2 { x: 0.0, y: 300.0 },
                Vec2 { x: 0.0, y: 600.0 },
            ),
        );
        inner_for_capture
            .widgets
            .finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner_for_capture, &capture_ctx);
        sleep(Duration::from_millis(25)).await;
        inner_for_capture
            .widgets
            .clear_registry(egui::ViewportId::ROOT);
        inner_for_capture.widgets.record_widget(
            egui::ViewportId::ROOT,
            make_scroll_entry(
                "scroll",
                1,
                Vec2 { x: 0.0, y: 300.0 },
                Vec2 { x: 0.0, y: 600.0 },
            ),
        );
        inner_for_capture
            .widgets
            .finalize_registry(egui::ViewportId::ROOT);
        capture_test_frame(&inner_for_capture, &capture_ctx);
    });

    let result = server
        .script_eval(
            "return eguidev.fixture(\"scroll\", nil, { timeout_ms = 250, poll_interval_ms = 1 })"
                .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true, "{json:?}");
}

#[tokio::test]
async fn fixture_rejects_unregistered_names() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("known", "Known fixture.").ready("status"),
    ]);
    let server = DevMcpServer::new(Arc::clone(&inner));
    let result = server.fixture_apply("unknown".to_string(), None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn fixtures_are_sorted_for_scripts() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("zeta", "Last fixture.").ready("status"),
        FixtureSpec::new("alpha", "First fixture.").ready("status"),
    ]);
    let specs = inner.fixtures.fixtures_sorted();
    let specs: Vec<_> = specs.into_iter().map(|fixture| fixture.name).collect();
    assert_eq!(specs, vec!["alpha".to_string(), "zeta".to_string()]);
}

#[tokio::test]
async fn fixture_clears_transient_automation_state_on_apply_boundaries() {
    let inner = Arc::new(Inner::new());
    inner.fixtures.set_fixtures(vec![
        FixtureSpec::new("reset", "Reset fixture.").ready("status"),
    ]);
    let applied = Arc::new(AtomicBool::new(false));
    let applied_handler = Arc::clone(&applied);
    set_runtime_fixture_handler(&inner, move |_call| {
        applied_handler.store(true, AtomicOrdering::Relaxed);
        fixture_ok()
    });
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        viewport_id: egui::ViewportId::ROOT,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner
        .viewports
        .capture_input_snapshot(&ctx, inner.fixture_epoch(), inner.frame_count() + 1);
    let viewport_id = egui::ViewportId::ROOT;
    inner.queue_action(
        viewport_id,
        InputAction::Text {
            text: "stale".to_string(),
        },
    );
    inner.queue_action_with_timing(
        viewport_id,
        ActionTiming::AfterOneFrame,
        InputAction::Text {
            text: "staged".to_string(),
        },
    );
    inner.queue_command(
        viewport_id,
        egui::ViewportCommand::Title("stale title".to_string()),
    );
    inner.queue_widget_value_update(
        viewport_id,
        "field".to_string(),
        WidgetValue::Text("queued".to_string()),
    );
    inner.set_scroll_override(viewport_id, 7, egui::vec2(1.0, 2.0));
    let secondary = egui::ViewportId::from_hash_of("secondary");
    for viewport_id in [viewport_id, secondary] {
        inner.set_overlay_debug_config(
            viewport_id,
            OverlayDebugConfig {
                enabled: true,
                ..Default::default()
            },
        );
    }

    let inner_for_frame = Arc::clone(&inner);
    let runtime_for_frame = Runtime::ensure_for_inner(&inner);
    tokio::spawn(async move {
        for _ in 0..4 {
            yield_now().await;
            inner_for_frame.advance_frame();
            runtime_for_frame.frame_notify().notify_waiters();
        }
    });
    let server = DevMcpServer::new(Arc::clone(&inner));
    server
        .fixture_apply("reset".to_string(), None)
        .await
        .expect("fixture result");

    assert!(applied.load(AtomicOrdering::Relaxed));
    // After fixture application, transient state should be cleared.
    assert!(!inner.actions.has_pending_actions(viewport_id));
    assert!(!inner.actions.has_pending_commands(viewport_id));
    assert!(
        inner
            .take_widget_value_update(viewport_id, "field")
            .is_none()
    );
    assert!(inner.take_scroll_override(viewport_id, 7).is_none());
    assert!(!inner.overlays.overlay_debug_config(secondary).enabled);
    assert!(
        !inner
            .overlays
            .overlay_debug_config(egui::ViewportId::ROOT)
            .enabled
    );
}
