//! Tests of actions and of raw input.

use super::*;

#[tokio::test]
async fn scroll_area_rejects_modifiers_and_clamps_to_max_offset() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    let mut area = make_entry("scroller", 1, WidgetRole::ScrollArea);
    area.role_state = Some(RoleState::ScrollArea {
        offset: Vec2 { x: 0.0, y: 0.0 },
        viewport_size: Vec2 { x: 100.0, y: 40.0 },
        content_size: Vec2 { x: 100.0, y: 80.0 },
    });
    inner.widgets.record_widget(viewport_id, area);
    inner.widgets.finalize_registry(viewport_id);
    let server = DevMcpServer::new(Arc::clone(&inner));
    let target = WidgetRef {
        id: "scroller".to_string(),
        viewport_id: None,
    };
    let error = server
        .action_scroll(
            None,
            target.clone(),
            Vec2 { x: 0.0, y: -10.0 },
            Some(Modifiers {
                shift: true,
                ..Modifiers::default()
            }),
        )
        .await
        .expect_err("modifiers rejected on scroll area");
    assert!(error.to_string().contains("modifiers"), "{error}");
    assert!(!inner.actions.has_pending_actions(viewport_id));
    assert!(inner.take_scroll_override(viewport_id, 1).is_none());

    server
        .action_scroll(None, target, Vec2 { x: 0.0, y: -1000.0 }, None)
        .await
        .expect("scroll");
    let offset = inner
        .take_scroll_override(viewport_id, 1)
        .expect("override");
    assert!((offset.y - 40.0).abs() < f32::EPSILON, "{offset:?}");
}

#[tokio::test]
async fn script_eval_scroll_area_accepts_action_options() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    let mut area = make_entry("scroller", 1, WidgetRole::ScrollArea);
    area.role_state = Some(RoleState::ScrollArea {
        offset: Vec2 { x: 0.0, y: 0.0 },
        viewport_size: Vec2 { x: 100.0, y: 40.0 },
        content_size: Vec2 { x: 100.0, y: 80.0 },
    });
    inner.widgets.record_widget(viewport_id, area);
    inner.widgets.finalize_registry(viewport_id);
    let server = DevMcpServer::new(Arc::clone(&inner));

    let result = server
        .script_eval(
            r#"eguidev.widget("scroller"):scroll({ x = 0, y = -10 }, {
            settle = false, timeout_ms = 1000, poll_interval_ms = 1,
        })"#
            .to_string(),
            Some(TEST_SCRIPT_DEADLINE_MS),
            None,
        )
        .await
        .expect("script eval");
    let result = parse_script_eval_json(&result);
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(
        inner.take_scroll_override(viewport_id, 1),
        Some(egui::vec2(0.0, 10.0))
    );
}

#[tokio::test]
async fn script_eval_drag_relative_accepts_positional_from() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "slider",
            1,
            WidgetRole::Slider,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            None,
        ),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .script_eval(
            r#"eguidev.widget("slider"):drag_relative(
                { x = 0.8, y = 0.5 },
                { from = { x = 0.2, y = 0.5 }, settle = false }
            )"#
            .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);

    let actions = inner
        .actions
        .drain_actions(viewport_id, inner.frame_count());
    let Some(InputAction::PointerMove { pos }) = actions.first() else {
        panic!("expected initial pointer move from drag_relative");
    };
    assert!((pos.x - 2.0).abs() < f32::EPSILON);
    assert!((pos.y - 5.0).abs() < f32::EPSILON);
}

#[tokio::test]
async fn script_eval_action_settle_rejects_invalid_types() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .script_eval(
            r#"eguidev.root:paste("hello", { settle = "fast" })"#.to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "typecheck");
}

#[tokio::test]
async fn script_eval_click_settle_targets_widget_viewport() {
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
    inner.capture_context(egui::ViewportId::ROOT, &ctx);
    inner.viewports.update_viewports(&ctx);
    inner
        .viewports
        .capture_input_snapshot(&ctx, inner.fixture_epoch(), inner.frame_count() + 1);

    inner.widgets.clear_registry(secondary);
    let mut entry = make_entry("status", 1, WidgetRole::Button);
    entry.viewport_id = viewport_id_to_string(secondary);
    inner.widgets.record_widget(secondary, entry);
    inner.widgets.finalize_registry(secondary);

    let viewport_selector = viewport_id_to_string(secondary);
    let result = server
        .script_eval(
            format!(
                "eguidev.configure({{ timeout_ms = 10, poll_interval_ms = 1 }})\nreturn eguidev.widget(\"status\"):click() -- {viewport_selector}"
            ),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["type"], "timeout", "{json:?}");
    assert_eq!(json["error"]["details"]["kind"], "settle", "{json:?}");
    let observation = &json["error"]["details"]["observation"];
    assert_eq!(observation["target_viewport_id"], viewport_selector);
    assert_eq!(observation["action_queue"]["end_queued_actions"], 3);
    assert_eq!(observation["action_queue"]["end_drained_actions"], 0);
    assert_eq!(observation["action_queue"]["pending_actions"], 3);
    assert!(
        observation["diagnosis"]
            .as_str()
            .expect("diagnosis")
            .contains("No target viewport frames were observed")
    );
}

#[tokio::test]
async fn scroll_into_view_reports_each_scrolled_area() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_scroll_entry(
            "scroll",
            1,
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 0.0, y: 400.0 },
        ),
    );
    let mut row = make_entry("row", 2, WidgetRole::Button);
    row.parent_id = Some("scroll".to_string());
    row.rect = Rect {
        min: Pos2 { x: 0.0, y: 300.0 },
        max: Pos2 { x: 100.0, y: 320.0 },
    };
    row.interact_rect = row.rect;
    inner.widgets.record_widget(viewport_id, row);
    let mut plain = make_entry("plain", 3, WidgetRole::Button);
    plain.parent_id = None;
    inner.widgets.record_widget(viewport_id, plain);
    inner.widgets.finalize_registry(viewport_id);

    let applied = server
        .action_scroll_into_view(None, widget_ref_id("row"))
        .await
        .expect("scroll into view");
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].widget_id, "scroll");
    assert!(applied[0].offset.y > 0.0, "{applied:?}");
    assert!(
        inner.take_scroll_override(viewport_id, 1).is_some(),
        "the scroll area received one override"
    );

    let applied = server
        .action_scroll_into_view(None, widget_ref_id("plain"))
        .await
        .expect("scroll into view");
    assert!(
        applied.is_empty(),
        "a target with no scroll ancestor scrolls nothing"
    );
}

#[tokio::test]
async fn action_drag_relative_updates_slider_value() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let mut value = 42.0_f32;

    inner.widgets.clear_registry(viewport_id);
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "slider".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::Slider {
                        range: WidgetRange {
                            min: 0.0,
                            max: 100.0,
                        },
                    },
                    value: Some(WidgetValue::Float(f64::from(value))),
                    visible,
                    ..Default::default()
                },
            );
        });
    }));
    inner.widgets.finalize_registry(viewport_id);

    server
        .action_drag_relative(
            None,
            widget_ref_id("slider"),
            Some(Vec2 { x: 0.2, y: 0.5 }),
            Vec2 { x: 0.8, y: 0.5 },
            None,
        )
        .await
        .expect("drag relative");

    // The drag stages one step per frame: land, press, move, release.
    while inner.actions.has_pending_actions(viewport_id) {
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        discard_output(ctx.run_ui(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
            });
        }));
    }

    assert!(value > 42.0_f32);
}

#[tokio::test]
async fn action_type_clear_replaces_text() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let mut text = "Hello".to_string();

    inner.widgets.clear_registry(viewport_id);
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.text_edit_multiline(&mut text);
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "notes".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::TextEdit {
                        multiline: false,
                        password: false,
                    },
                    value: Some(WidgetValue::Text(text.clone())),
                    visible,
                    ..Default::default()
                },
            );
        });
    }));
    inner.widgets.finalize_registry(viewport_id);

    server
        .action_type(
            None,
            widget_ref_id("notes"),
            "World".to_string(),
            None,
            Some(true),
        )
        .await
        .expect("type action");

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.text_edit_multiline(&mut text);
        });
    }));

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.text_edit_multiline(&mut text);
        });
    }));

    assert_eq!(text, "World");
}

#[tokio::test]
async fn action_type_skips_click_when_widget_already_focused() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("notes", 1, WidgetRole::TextEdit);
    entry.focused = true;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    server
        .action_type(
            None,
            widget_ref_id("notes"),
            "World".to_string(),
            None,
            None,
        )
        .await
        .expect("action type");

    let actions = inner
        .actions
        .drain_actions(viewport_id, inner.frame_count());
    let click_presses = actions
        .iter()
        .filter(|action| matches!(action, InputAction::PointerButton { pressed: true, .. }))
        .count();
    assert_eq!(click_presses, 0);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, InputAction::Text { text } if text == "World"))
    );
}

#[test]
fn interaction_ready_requires_visibility_and_a_visible_fraction() {
    let rect = Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 { x: 10.0, y: 10.0 },
    };
    let clipped_layout = |visible_fraction| WidgetLayout {
        desired_size: Vec2 { x: 10.0, y: 10.0 },
        actual_size: Vec2 { x: 10.0, y: 10.0 },
        clip_rect: rect,
        clipped: true,
        overflow: false,
        available_rect: rect,
        visible_fraction,
        text: None,
    };

    // No layout metadata means nothing is known to clip the widget.
    let plain = make_entry("plain", 1, WidgetRole::Button);
    assert!(interaction_ready(&plain));

    let mut hidden = make_entry("hidden", 1, WidgetRole::Button);
    hidden.visible = false;
    assert!(!interaction_ready(&hidden));

    let mut clipped = make_entry("clipped", 1, WidgetRole::Button);
    clipped.layout = Some(clipped_layout(0.0));
    assert!(!interaction_ready(&clipped));

    let mut partly_clipped = make_entry("partly", 1, WidgetRole::Button);
    partly_clipped.layout = Some(clipped_layout(0.25));
    assert!(interaction_ready(&partly_clipped));

    let mut disabled = make_entry("disabled", 1, WidgetRole::Button);
    disabled.enabled = false;
    assert!(!interaction_ready(&disabled));
}

#[test]
fn pointer_ready_also_requires_the_widget_to_be_uncovered() {
    let plain = make_entry("plain", 1, WidgetRole::Button);
    assert!(interaction_ready(&plain));
    assert!(pointer_ready(&plain));

    let mut covered = make_entry("covered", 1, WidgetRole::Button);
    covered.covered = true;
    // A covered widget still satisfies interaction_ready: it is visible,
    // enabled, and unclipped. scroll_into_view's poll must keep accepting
    // it, since scrolling cannot uncover a widget under a fixed overlay.
    assert!(interaction_ready(&covered));
    assert!(
        !pointer_ready(&covered),
        "a covered widget must not admit a pointer action"
    );

    let mut hidden_and_covered = make_entry("hidden_and_covered", 1, WidgetRole::Button);
    hidden_and_covered.visible = false;
    hidden_and_covered.covered = true;
    assert!(!interaction_ready(&hidden_and_covered));
    assert!(!pointer_ready(&hidden_and_covered));
}

#[tokio::test]
async fn action_click_rejects_invisible_widget() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("hidden", 1, WidgetRole::Button);
    entry.visible = false;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let error = server
        .action_click(None, widget_ref_id("hidden"), None, None, None)
        .await
        .expect_err("hidden widget should not be clicked");

    assert_eq!(error.code, ErrorCode::NotActionable.as_str());
    assert!(error.message.contains("scroll_into_view"));
}

#[tokio::test]
async fn action_click_rejects_fully_clipped_widget() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;
    let rect = Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 { x: 10.0, y: 10.0 },
    };

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("clipped", 1, WidgetRole::Button);
    entry.layout = Some(WidgetLayout {
        desired_size: Vec2 { x: 10.0, y: 10.0 },
        actual_size: Vec2 { x: 10.0, y: 10.0 },
        clip_rect: rect,
        clipped: true,
        overflow: false,
        available_rect: rect,
        visible_fraction: 0.0,
        text: None,
    });
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let error = server
        .action_click(None, widget_ref_id("clipped"), None, None, None)
        .await
        .expect_err("fully clipped widget should not be clicked");

    assert_eq!(error.code, ErrorCode::NotActionable.as_str());
    assert!(error.message.contains("scroll_into_view"));
    let details = error
        .structured
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("details"))
        .expect("details");
    assert_eq!(details["reason"], "invisible_interaction");
    assert_eq!(details["layout"]["visible_fraction"], 0.0);
}

#[tokio::test]
async fn action_click_rejects_covered_widget() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("covered", 1, WidgetRole::Button);
    entry.covered = true;
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    let error = server
        .action_click(None, widget_ref_id("covered"), None, None, None)
        .await
        .expect_err("a covered widget should not be clicked");

    assert_eq!(error.code, ErrorCode::NotActionable.as_str());
    assert!(error.message.contains("covers it"), "{}", error.message);
    let details = error
        .structured
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("details"))
        .expect("details");
    assert_eq!(details["reason"], "covered");
    assert!(
        details["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty()),
        "{details:?}"
    );
}

#[tokio::test]
async fn covered_widget_scroll_into_view_still_settles() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
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

    // An area needs one prior frame before egui treats it as
    // interactable, and the card renders before the background button so
    // a single repeat already reaches steady-state coverage.
    for _ in 0..2 {
        inner.widgets.clear_registry(viewport_id);
        discard_output(ctx.run_ui(raw_input(), |ui| {
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
        }));
        inner.widgets.finalize_registry(viewport_id);
    }
    capture_test_frame(&inner, &ctx);

    let covered = resolve_widget(&inner, None, &widget_ref_id("background.approve"))
        .expect("widget lookup")
        .covered;
    assert!(covered, "the background button should sit under the card");

    // scroll_into_view's settle step needs an advancing frame count to
    // observe its queued action as processed, so pump frames on the
    // unchanged scene while the script awaits settling.
    let inner_for_pump = Arc::clone(&inner);
    let ctx_for_pump = ctx.clone();
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(2)).await;
            capture_test_frame(&inner_for_pump, &ctx_for_pump);
        }
    });

    let result = server
        .script_eval(
            r#"eguidev.widget("background.approve"):scroll_into_view(
{ timeout_ms = 1000, poll_interval_ms = 5 }
)"#
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
async fn action_focus_updates_widget_focus_after_frame() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let mut text = "Hello".to_string();

    inner.widgets.clear_registry(viewport_id);
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.text_edit_multiline(&mut text);
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "notes".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::TextEdit {
                        multiline: false,
                        password: false,
                    },
                    value: Some(WidgetValue::Text(text.clone())),
                    visible,
                    ..Default::default()
                },
            );
        });
    }));
    inner.widgets.finalize_registry(viewport_id);

    server
        .action_focus(None, widget_ref_id("notes"))
        .await
        .expect("action focus");

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    inner.widgets.clear_registry(viewport_id);
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.text_edit_multiline(&mut text);
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "notes".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::TextEdit {
                        multiline: false,
                        password: false,
                    },
                    value: Some(WidgetValue::Text(text.clone())),
                    visible,
                    ..Default::default()
                },
            );
        });
    }));
    inner.widgets.finalize_registry(viewport_id);

    let focused = resolve_widget(&inner, None, &widget_ref_id("notes"))
        .expect("widget lookup")
        .focused;
    assert!(focused);
}

#[tokio::test]
async fn raw_input_rejects_numeric_overflow_without_queuing() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    inner.queue_action(
        egui::ViewportId::ROOT,
        InputAction::Text {
            text: "preserved".to_string(),
        },
    );
    for value in [1e100, -1e100] {
        for event in [
            json!({ "type": "pointer_move", "position": { "x": value, "y": 0 } }),
            json!({ "type": "pointer_button", "position": { "x": 0, "y": value }, "button": "primary", "action": "press" }),
            json!({ "type": "scroll", "delta": { "x": value, "y": 0 } }),
        ] {
            // Raw input is deserialized directly, bypassing parse_f32.
            let event = serde_json::from_value::<RawInputEvent>(event).expect("raw event");
            let error = server
                .input(None, event)
                .await
                .expect_err("invalid coordinates");
            assert_eq!(error.code, ErrorCode::InvalidArgument.as_str());
            assert!(error.message.contains("finite"));
        }
    }
    let queued = inner
        .actions
        .drain_actions(egui::ViewportId::ROOT, inner.frame_count());
    assert!(matches!(queued.as_slice(), [InputAction::Text { text }] if text == "preserved"));
}

#[tokio::test]
async fn viewport_resize_rejects_invalid_sizes_atomically() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(inner);
    let result = server
        .viewport_resize(
            None,
            ResizeOptions {
                min_size: Some(Vec2 { x: -1.0, y: 480.0 }),
                ..Default::default()
            },
        )
        .await
        .expect_err("invalid min_inner_size");
    assert_eq!(result.code, ErrorCode::InvalidArgument.as_str());
    assert!(result.message.contains("min_size"));
}

#[tokio::test]
async fn viewport_resize_queues_one_validated_command_batch() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let inner_size = Vec2 { x: 800.0, y: 600.0 };
    let min_inner_size = Vec2 { x: 400.0, y: 300.0 };
    let max_inner_size = Vec2 {
        x: 1200.0,
        y: 900.0,
    };
    let resize_increments = Vec2 { x: 10.0, y: 20.0 };

    server
        .viewport_resize(
            None,
            ResizeOptions {
                inner_size: Some(inner_size),
                min_size: Some(min_inner_size),
                max_size: Some(max_inner_size),
                increments: Some(resize_increments),
                resizable: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("viewport resize");

    let commands = inner.actions.drain_commands(egui::ViewportId::ROOT);
    assert_eq!(
        commands,
        vec![
            egui::ViewportCommand::InnerSize(inner_size.into()),
            egui::ViewportCommand::MinInnerSize(min_inner_size.into()),
            egui::ViewportCommand::MaxInnerSize(max_inner_size.into()),
            egui::ViewportCommand::ResizeIncrements(Some(resize_increments.into())),
            egui::ViewportCommand::Resizable(true),
        ]
    );
}

#[tokio::test]
async fn input_pointer_button_toggles_checkbox() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let mut checked = false;

    inner.widgets.clear_registry(viewport_id);
    let raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.checkbox(&mut checked, "Enabled");
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "toggle".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::Checkbox {
                        indeterminate: None,
                    },
                    label: Some("Enabled".to_string()),
                    value: Some(WidgetValue::Bool(checked)),
                    visible,
                    ..Default::default()
                },
            );
        });
    }));
    inner.widgets.finalize_registry(viewport_id);

    let widget = inner
        .widgets
        .widget_list(viewport_id)
        .into_iter()
        .find(|entry| entry.id == "toggle")
        .expect("toggle widget");
    let pos = widget.interact_rect.center();

    server
        .input(None, RawInputEvent::PointerMove { position: pos })
        .await
        .expect("pointer move");
    server
        .input(
            None,
            RawInputEvent::PointerButton {
                position: pos,
                button: PointerButton::Primary,
                action: RawInputAction::Press,
                modifiers: None,
            },
        )
        .await
        .expect("pointer down");
    server
        .input(
            None,
            RawInputEvent::PointerButton {
                position: pos,
                button: PointerButton::Primary,
                action: RawInputAction::Release,
                modifiers: None,
            },
        )
        .await
        .expect("pointer up");

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.checkbox(&mut checked, "Enabled");
        });
    }));

    assert!(checked);
}

#[tokio::test]
async fn raw_input_queues_exactly_one_event_per_call() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;
    let pos = Pos2 { x: 12.0, y: 24.0 };
    let modifiers = Modifiers {
        shift: true,
        ..Default::default()
    };

    server
        .input(None, RawInputEvent::PointerMove { position: pos })
        .await
        .expect("pointer move");
    server
        .input(
            None,
            RawInputEvent::PointerButton {
                position: pos,
                button: PointerButton::Primary,
                action: RawInputAction::Press,
                modifiers: Some(modifiers),
            },
        )
        .await
        .expect("pointer button");
    server
        .input(
            None,
            RawInputEvent::Key {
                key: "Enter".to_string(),
                action: RawInputAction::Press,
                modifiers: Some(modifiers),
            },
        )
        .await
        .expect("key input");
    server
        .input(
            None,
            RawInputEvent::Text {
                text: "Hello".to_string(),
            },
        )
        .await
        .expect("text input");
    server
        .input(
            None,
            RawInputEvent::Scroll {
                delta: Vec2 { x: 1.0, y: -2.0 },
                modifiers: Some(modifiers),
            },
        )
        .await
        .expect("scroll input");
    server
        .focus_window("root".to_string())
        .await
        .expect("focus window");

    let queued_actions = inner
        .actions
        .drain_actions(viewport_id, inner.frame_count());
    assert_eq!(queued_actions.len(), 5);
    assert!(queued_actions.iter().any(|action| {
        matches!(
            action,
            InputAction::PointerMove { pos: queued_pos }
                if queued_pos.x == pos.x && queued_pos.y == pos.y
        )
    }));
    assert!(queued_actions.iter().any(|action| {
        matches!(
            action,
            InputAction::PointerButton {
                pos: queued_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: queued_modifiers,
            } if queued_pos.x == pos.x && queued_pos.y == pos.y && queued_modifiers.shift
        )
    }));
    assert!(queued_actions.iter().any(|action| {
        matches!(
            action,
            InputAction::Key {
                key: egui::Key::Enter,
                pressed: true,
                modifiers: queued_modifiers,
            } if queued_modifiers.shift
        )
    }));
    assert!(
        queued_actions
            .iter()
            .any(|action| matches!(action, InputAction::Text { text } if text == "Hello"))
    );
    assert!(queued_actions.iter().any(|action| {
        matches!(
            action,
            InputAction::Scroll {
                delta,
                modifiers: queued_modifiers,
            } if delta.x == 1.0 && delta.y == -2.0 && queued_modifiers.shift
        )
    }));

    let pending_commands = inner.actions.drain_commands(viewport_id);
    assert_eq!(pending_commands, vec![egui::ViewportCommand::Focus]);

    let highlight_rect = Rect {
        min: Pos2 { x: 1.0, y: 2.0 },
        max: Pos2 { x: 3.0, y: 4.0 },
    };
    let highlight_result = server
        .show_highlight(None, None, Some(highlight_rect), "#ff0000".to_string())
        .await
        .expect("show_highlight");
    assert_eq!(highlight_result.rect.min.x, 1.0);
    assert_eq!(highlight_result.rect.min.y, 2.0);
    assert_eq!(highlight_result.rect.max.x, 3.0);
    assert_eq!(highlight_result.rect.max.y, 4.0);
    server
        .clear_highlights(None, None)
        .await
        .expect("clear_highlights");
}

#[test]
fn scroll_input_moves_scroll_area() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    let mut raw_input = egui::RawInput {
        viewport_id,
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 400.0),
        )),
        ..Default::default()
    };
    inner.queue_action(
        viewport_id,
        InputAction::PointerMove {
            pos: Pos2 { x: 50.0, y: 50.0 },
        },
    );
    inner.queue_action(
        viewport_id,
        InputAction::Scroll {
            delta: Vec2 { x: 0.0, y: -120.0 },
            modifiers: Modifiers::default(),
        },
    );

    apply_actions(&inner, &mut raw_input);

    let ctx = egui::Context::default();
    let mut offset = egui::Vec2::ZERO;
    discard_output(ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let output = egui::ScrollArea::vertical()
                .max_height(100.0)
                .show(ui, |ui| {
                    for row in 0..100 {
                        ui.label(format!("Row {row}"));
                    }
                });
            offset = output.state.clamped_offset();
        });
    }));

    assert!(
        offset.y > 0.0,
        "expected scroll offset to move, got {offset:?}"
    );
}

#[tokio::test]
async fn action_click_click_count_queues_multiple_clicks() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("click", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    server
        .action_click(None, widget_ref_id("click"), None, None, Some(2))
        .await
        .expect("action click");

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    let pointer_buttons = raw_input
        .events
        .iter()
        .filter(|event| matches!(event, egui::Event::PointerButton { .. }))
        .count();
    assert_eq!(pointer_buttons, 4);
}

#[tokio::test]
async fn action_click_can_succeed_without_widget_state_change() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut status = make_entry("status", 1, WidgetRole::Label);
    status.value = Some(WidgetValue::Text("stable".to_string()));
    inner.widgets.record_widget(viewport_id, status);
    inner.widgets.finalize_registry(viewport_id);

    let before = inner
        .widgets
        .resolve_widget(&inner.viewports, None, &widget_ref_id("status"))
        .expect("status before");
    server
        .action_click(None, widget_ref_id("status"), None, None, None)
        .await
        .expect("action click");

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    assert!(
        raw_input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::PointerButton { .. }))
    );

    let after = inner
        .widgets
        .resolve_widget(&inner.viewports, None, &widget_ref_id("status"))
        .expect("status after");
    assert_eq!(before.value, after.value);
}

#[tokio::test]
async fn action_key_injects_text_event() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    for (combo, expected) in [
        ("a", Some("a")),
        ("A", Some("A")),
        ("Space", Some(" ")),
        ("space", Some(" ")),
        ("SPACE", Some(" ")),
        ("shift-space", Some(" ")),
        ("ctrl-space", None),
        ("alt-space", None),
        ("cmd-space", None),
    ] {
        let (key, modifiers, name) = parse_key_combo(combo).expect("key combo");
        server
            .action_key(None, key, modifiers, &name, None)
            .await
            .expect("action key");

        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner, &mut raw_input);
        let text = raw_input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(text, expected.into_iter().collect::<Vec<_>>(), "{combo}");
    }
}

#[tokio::test]
async fn action_key_rejects_repeat_count_over_cap() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let error = server
        .action_key(
            None,
            egui::Key::A,
            Modifiers::default(),
            "a",
            Some(MAX_KEY_REPEAT + 1),
        )
        .await
        .expect_err("repeat cap");
    assert_eq!(error.code, ErrorCode::InvalidArgument.as_str());
    assert!(error.message.contains("repeat_count"), "{error:?}");
}

#[tokio::test]
async fn action_paste_queues_paste_event() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    server
        .action_paste(None, "Hello".to_string())
        .await
        .expect("action paste");

    let mut raw_input = egui::RawInput {
        viewport_id,
        ..Default::default()
    };
    apply_actions(&inner, &mut raw_input);
    assert!(
        raw_input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Paste(text) if text == "Hello"))
    );
}

#[tokio::test]
async fn action_hover_queues_pointer_move_and_waits_for_arrival() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;
    Runtime::ensure_for_inner(&inner);

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("hover", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let inner_for_frame = Arc::clone(&inner);
    let frame = tokio::spawn(async move {
        sleep(Duration::from_millis(5)).await;
        let mut raw_input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        apply_actions(&inner_for_frame, &mut raw_input);
        let moved = raw_input.events.iter().any(|event| {
            matches!(event, egui::Event::PointerMoved(pos)
                if (pos.x - 5.0).abs() < f32::EPSILON && (pos.y - 5.0).abs() < f32::EPSILON)
        });
        record_test_pointer_snapshot(&inner_for_frame, viewport_id, Some(Pos2 { x: 5.0, y: 5.0 }));
        moved
    });

    server
        .action_hover(
            None,
            widget_ref_id("hover"),
            None,
            Some(0),
            Some(HoverConfirm {
                timeout_ms: Some(500),
                poll_interval_ms: Some(1),
            }),
        )
        .await
        .expect("action hover");
    assert!(
        frame.await.expect("frame task"),
        "the hover queued one pointer move"
    );
}

#[tokio::test]
async fn action_hover_duration_is_independent_of_frame_rate() {
    for fast_frames in [false, true] {
        let inner = Arc::new(Inner::new());
        let server = DevMcpServer::new(Arc::clone(&inner));
        let viewport_id = egui::ViewportId::ROOT;
        let runtime = Runtime::ensure_for_inner(&inner);
        inner
            .widgets
            .record_widget(viewport_id, make_entry("hover", 1, WidgetRole::Button));
        inner.widgets.finalize_registry(viewport_id);

        let started = Instant::now();
        let frames = fast_frames.then(|| {
            tokio::spawn(async move {
                loop {
                    runtime.frame_notify().notify_waiters();
                    sleep(Duration::from_millis(1)).await;
                }
            })
        });
        let result = server
            .action_hover(None, widget_ref_id("hover"), None, Some(60), None)
            .await;
        let elapsed = started.elapsed();
        if let Some(frames) = frames {
            frames.abort();
            assert!(frames.await.expect_err("stopped frame task").is_cancelled());
        }
        result.expect("hover duration must not require frames");
        assert!(elapsed >= Duration::from_millis(60));
    }
}

#[tokio::test]
async fn action_hover_fails_when_the_pointer_never_arrives() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;
    Runtime::ensure_for_inner(&inner);

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("hover", 1, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let error = server
        .action_hover(
            None,
            widget_ref_id("hover"),
            None,
            Some(0),
            Some(HoverConfirm {
                timeout_ms: Some(20),
                poll_interval_ms: Some(1),
            }),
        )
        .await
        .expect_err("a hover with no pointer snapshot fails");
    assert_eq!(error.code, "not_actionable");
    assert!(error.message.contains("did not reach"), "{}", error.message);
}

#[tokio::test]
async fn widget_set_value_queues_update() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("field", 1, WidgetRole::TextEdit);
    entry.value = Some(WidgetValue::Text("Before".to_string()));
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    server
        .widget_set_value(
            None,
            widget_ref_id("field"),
            WidgetValue::Text("After".to_string()),
        )
        .await
        .expect("widget set value");

    let updated = inner
        .take_widget_value_update(viewport_id, "field")
        .expect("queued update");
    assert!(matches!(updated, WidgetValue::Text(value) if value == "After"));
}

#[tokio::test]
async fn widget_set_value_accepts_collapsing_header_bool() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("advanced", 1, WidgetRole::CollapsingHeader);
    entry.value = Some(WidgetValue::Bool(false));
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    server
        .widget_set_value(None, widget_ref_id("advanced"), WidgetValue::Bool(true))
        .await
        .expect("widget set value");

    let updated = inner
        .take_widget_value_update(viewport_id, "advanced")
        .expect("queued update");
    assert_eq!(updated, WidgetValue::Bool(true));
}

#[tokio::test]
async fn widget_set_value_reports_unconsumed_custom_override() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;

    record_test_snapshot(&inner, viewport_id);
    inner.widgets.clear_registry(viewport_id);
    let mut entry = make_entry("custom.value", 1, WidgetRole::Slider);
    entry.value = Some(WidgetValue::Int(0));
    entry.role_state = Some(RoleState::Slider {
        range: WidgetRange {
            min: 0.0,
            max: 10.0,
        },
    });
    inner.widgets.record_widget(viewport_id, entry);
    inner.widgets.finalize_registry(viewport_id);

    server
        .widget_set_value(None, widget_ref_id("custom.value"), WidgetValue::Int(5))
        .await
        .expect("queue set_value");

    let inner_for_capture = Arc::clone(&inner);
    tokio::spawn(async move {
        yield_now().await;
        record_test_snapshot(&inner_for_capture, viewport_id);
        yield_now().await;
        record_test_snapshot(&inner_for_capture, viewport_id);
    });

    let error = server
        .wait_for_widget_state(
            None,
            widget_ref_id("custom.value"),
            Some(100),
            Some(1),
            "predicate",
            |_| false,
        )
        .await
        .expect_err("unconsumed override should fail");

    assert_eq!(error.code, ErrorCode::InstrumentationFault.as_str());
    assert!(error.message.contains("take_widget_value_override"));
    drop(ctx);
}

/// Verify that injecting Enter via action_key causes a singleline TextEdit
/// to surrender focus, matching the standard egui pattern:
///   response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
/// Uses raw_input.focused = false on non-action frames to prove the
/// mechanism works even when the window lacks OS focus.
#[tokio::test]
async fn action_key_enter_surrenders_focus_on_singleline() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let ctx = egui::Context::default();
    let viewport_id = egui::ViewportId::ROOT;
    let mut text = String::new();
    let mut enter_detected = false;

    let render_widget =
        |inner: &Inner, ctx: &egui::Context, text: &mut String, raw_input: egui::RawInput| {
            discard_output(ctx.run_ui(raw_input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.text_edit_singleline(text);
                    let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
                    record_widget(
                        &inner.widgets,
                        "input".to_string(),
                        &response,
                        WidgetMeta {
                            role: WidgetRoleMeta::TextEdit {
                                multiline: false,
                                password: false,
                            },
                            value: Some(WidgetValue::Text(text.clone())),
                            visible,
                            ..Default::default()
                        },
                    );
                    response.lost_focus()
                });
            }));
            inner.widgets.finalize_registry(viewport_id);
        };

    // Simulate no OS window focus — platform frames report focused=false.
    let no_focus_raw = || egui::RawInput {
        viewport_id,
        focused: false,
        ..Default::default()
    };

    // Frame 1: initial render (unfocused, no OS focus).
    inner.widgets.clear_registry(viewport_id);
    render_widget(&inner, &ctx, &mut text, no_focus_raw());

    // Type text (queues click-to-focus + text input).
    server
        .action_type(
            None,
            widget_ref_id("input"),
            "hello".to_string(),
            None,
            None,
        )
        .await
        .expect("type action");

    // Frame 2: apply click-to-focus.
    let mut raw = no_focus_raw();
    apply_actions(&inner, &mut raw);
    render_widget(&inner, &ctx, &mut text, raw);

    // Frame 3: apply staged text input after focus is established.
    let mut raw = no_focus_raw();
    apply_actions(&inner, &mut raw);
    render_widget(&inner, &ctx, &mut text, raw);
    assert_eq!(text, "hello");

    // Widget should report focused=true via egui memory focus even when
    // raw_input.focused was forced only for the action frame.
    let widget = inner
        .widgets
        .resolve_widget(&inner.viewports, None, &widget_ref_id("input"))
        .expect("widget should exist");
    assert!(widget.focused, "widget should have egui memory focus");

    // Frame 4: no actions, no OS focus — the settle frame.
    // Widget must retain egui memory focus even though the window
    // doesn't have OS focus.
    render_widget(&inner, &ctx, &mut text, no_focus_raw());
    let widget = inner
        .widgets
        .resolve_widget(&inner.viewports, None, &widget_ref_id("input"))
        .expect("widget should exist");
    assert!(
        widget.focused,
        "widget must retain memory focus across settle frame without OS focus"
    );

    // Queue Enter key.
    server
        .action_key(None, egui::Key::Enter, Modifiers::default(), "Enter", None)
        .await
        .expect("key action");

    // Frame 5: apply Enter and detect lost_focus + key_pressed(Enter).
    let mut raw = no_focus_raw();
    apply_actions(&inner, &mut raw);
    discard_output(ctx.run_ui(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = ui.text_edit_singleline(&mut text);
            let visible = ui.is_visible() && ui.is_rect_visible(response.rect);
            record_widget(
                &inner.widgets,
                "input".to_string(),
                &response,
                WidgetMeta {
                    role: WidgetRoleMeta::TextEdit {
                        multiline: false,
                        password: false,
                    },
                    value: Some(WidgetValue::Text(text.clone())),
                    visible,
                    ..Default::default()
                },
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                enter_detected = true;
            }
        });
    }));
    inner.widgets.finalize_registry(viewport_id);

    assert!(
        enter_detected,
        "Enter key should cause lost_focus + key_pressed(Enter)"
    );
}
