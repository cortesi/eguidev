//! Tests of layout checks, text measurement, and the debug overlay.

use super::*;

#[tokio::test]
async fn script_eval_widget_show_debug_overlay_preserves_viewport_scope() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let secondary = egui::ViewportId::from_hash_of("secondary");
    let viewport_selector = viewport_id_to_string(secondary);
    let ctx = egui::Context::default();

    {
        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        raw_input.viewports.insert(secondary, Default::default());
        discard_output(ctx.run_ui(raw_input, |_| {}));
    }
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner.viewports.update_viewports(&ctx);
    inner
        .viewports
        .capture_input_snapshot(&ctx, inner.fixture_epoch(), inner.frame_count() + 1);

    inner.widgets.clear_registry(secondary);
    let mut entry = make_entry("overlay", 1, WidgetRole::Button);
    entry.viewport_id = viewport_id_to_string(secondary);
    inner.widgets.record_widget(secondary, entry);
    inner.widgets.finalize_registry(secondary);

    let result = server
        .script_eval(
            format!(
                "return eguidev.widget(\"overlay\"):show_debug_overlay() -- {viewport_selector}"
            ),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);

    assert!(
        !inner
            .overlays
            .overlay_debug_config(egui::ViewportId::ROOT)
            .enabled
    );
    let config = inner.overlays.overlay_debug_config(secondary);
    let scope = config.scope.expect("widget-scoped overlay");
    assert_eq!(scope.id, "overlay");
    assert_eq!(
        scope.viewport_id.as_deref(),
        Some(viewport_selector.as_str())
    );
}

#[tokio::test]
async fn check_layout_reports_overlap() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "first",
            1,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            None,
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "second",
            2,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 5.0, y: 5.0 },
                max: Pos2 { x: 15.0, y: 15.0 },
            },
            None,
        ),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = server.check_layout(None, None).await.expect("layout check");
    assert!(
        result
            .iter()
            .any(|issue| matches!(issue.kind, LayoutIssueKind::Overlap))
    );
}

#[tokio::test]
async fn text_measure_returns_text() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput::default();
    let text = "Hello".to_string();
    discard_output(ctx.run_ui(raw_input, |ctx| {
        inner.capture_context(ctx.viewport_id(), ctx);
        inner.widgets.clear_registry(ctx.viewport_id());
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.label(&text);
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "label".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::Plain(WidgetRole::Label),
                    label: Some(text.clone()),
                    value: Some(WidgetValue::Text(text.clone())),
                    visible,
                    ..Default::default()
                },
            );
        });
        inner.widgets.finalize_registry(ctx.viewport_id());
    }));

    let result = server
        .text_measure(widget_ref_id("label"))
        .await
        .expect("text measure");
    assert_eq!(result.text, "Hello");
    assert_eq!(result.text.chars().count(), 5);
    assert!(!result.lines.is_empty());
}

#[tokio::test]
async fn script_eval_widget_text_measure_returns_text() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput::default();
    let text = "Hello".to_string();
    discard_output(ctx.run_ui(raw_input, |ctx| {
        inner.capture_context(ctx.viewport_id(), ctx);
        inner.widgets.clear_registry(ctx.viewport_id());
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.label(&text);
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "label".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::Plain(WidgetRole::Label),
                    label: Some(text.clone()),
                    value: Some(WidgetValue::Text(text.clone())),
                    visible,
                    ..Default::default()
                },
            );
        });
        inner.widgets.finalize_registry(ctx.viewport_id());
    }));

    let result = server
        .script_eval(
            r#"return eguidev.widget("label"):text_measure().text"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"], "Hello");
}

#[tokio::test]
async fn text_measure_uses_widget_viewport_context() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let secondary = egui::ViewportId::from_hash_of("secondary");
    for _ in 0..2 {
        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        raw_input.viewports.insert(secondary, Default::default());
        discard_output(ctx.run_ui(raw_input, |_| {}));
    }
    inner.capture_context(secondary, &ctx);
    inner.viewports.update_viewports(&ctx);

    inner.widgets.clear_registry(secondary);
    let mut entry = make_entry("label.secondary", 1, WidgetRole::Label);
    entry.viewport_id = viewport_id_to_string(secondary);
    entry.value = Some(WidgetValue::Text("Hello".to_string()));
    inner.widgets.record_widget(secondary, entry);
    inner.widgets.finalize_registry(secondary);

    let result = server
        .text_measure(WidgetRef {
            id: "label.secondary".to_string(),
            viewport_id: Some(viewport_id_to_string(secondary)),
        })
        .await
        .expect("text measure");
    assert_eq!(result.text, "Hello");
}

#[tokio::test]
async fn check_layout_text_truncation_uses_scope_viewport_context() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let secondary = egui::ViewportId::from_hash_of("secondary");
    for _ in 0..2 {
        let mut raw_input = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            ..Default::default()
        };
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, Default::default());
        raw_input.viewports.insert(secondary, Default::default());
        discard_output(ctx.run_ui(raw_input, |_| {}));
    }
    inner.capture_context(secondary, &ctx);
    inner.viewports.update_viewports(&ctx);

    inner.widgets.clear_registry(secondary);
    let mut entry = make_entry_with_rect(
        "label.secondary",
        1,
        WidgetRole::Label,
        Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 20.0, y: 10.0 },
        },
        None,
    );
    entry.viewport_id = viewport_id_to_string(secondary);
    entry.value = Some(WidgetValue::Text("HelloWorld".to_string()));
    inner.widgets.record_widget(secondary, entry);
    inner.widgets.finalize_registry(secondary);

    let result = server
        .check_layout(Some(viewport_id_to_string(secondary)), None)
        .await
        .expect("layout check");
    assert!(result.iter().all(|issue| !issue.widgets.is_empty()));
}

#[tokio::test]
async fn check_layout_scopes_to_widget_subtree() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("left", 1, WidgetRole::Window));
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "left.row",
            2,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 20.0, y: 10.0 },
            },
            Some("left"),
        ),
    );
    inner
        .widgets
        .record_widget(viewport_id, make_entry("right", 3, WidgetRole::Window));
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "right.row",
            4,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 50.0, y: 0.0 },
                max: Pos2 { x: 80.0, y: 10.0 },
            },
            Some("right"),
        ),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .check_layout(None, Some(widget_ref_id("right.row")))
        .await
        .expect("layout check");
    assert!(result.is_empty());
}

#[tokio::test]
async fn show_clear_debug_overlay() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("overlay", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    server
        .show_debug_overlay(None, Some(OverlayDebugModeName::Bounds), None, None)
        .await
        .expect("show debug overlay");
    let config = inner.overlays.overlay_debug_config(egui::ViewportId::ROOT);
    assert!(config.enabled);
    assert_eq!(config.mode, OverlayDebugMode::Bounds);

    server
        .clear_debug_overlay(None)
        .await
        .expect("hide debug overlay");
    assert!(
        !inner
            .overlays
            .overlay_debug_config(egui::ViewportId::ROOT)
            .enabled
    );
}
