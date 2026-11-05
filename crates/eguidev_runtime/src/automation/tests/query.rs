//! Tests of widget queries and widget lookup.

use super::*;

#[test]
fn covering_widget_id_reports_the_topmost_hit() {
    let inner = Inner::new();
    let viewport_id = egui::ViewportId::ROOT;
    let button = make_entry_with_rect(
        "under",
        1,
        WidgetRole::Button,
        Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        },
        None,
    );
    let overlay = make_entry_with_rect(
        "cover",
        2,
        WidgetRole::Button,
        Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 10.0, y: 10.0 },
        },
        None,
    );
    inner.widgets.record_widget(viewport_id, button);
    inner.widgets.record_widget(viewport_id, overlay);
    inner.widgets.finalize_registry(viewport_id);
    let pos = Pos2 { x: 5.0, y: 5.0 };
    assert_eq!(
        covering_widget_id(&inner, viewport_id, pos, "under").as_deref(),
        Some("cover")
    );
    assert_eq!(covering_widget_id(&inner, viewport_id, pos, "cover"), None);
}

#[tokio::test]
async fn script_eval_widget_at_point_accepts_boolean_all_layers() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "bottom",
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
            "top",
            2,
            WidgetRole::Button,
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
            r#"local widgets = eguidev.root:widgets_at({ x = 5, y = 5 })
            return { count = #widgets, first = widgets[1].id }"#
                .to_string(),
            None,
            None,
        )
        .await
        .expect("script eval");
    let json = parse_script_eval_json(&result);
    assert_eq!(json["success"], true);
    assert_eq!(json["value"]["count"], 2);
    assert_eq!(json["value"]["first"], "top");
}

#[tokio::test]
async fn widget_list_respects_visibility() {
    let inner = Arc::new(Inner::new());
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput::default();
    discard_output(ctx.run_ui(raw_input, |ctx| {
        inner.capture_context(ctx.viewport_id(), ctx);
        inner.widgets.clear_registry(ctx.viewport_id());
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(40.0)
                .show(ui, |ui| {
                    let visible = ui.button("Visible");
                    let visible_flag = ui.is_rect_visible(visible.rect);
                    record_widget(
                        &inner.widgets,
                        "visible".to_string(),
                        &visible,
                        WidgetMeta {
                            role: WidgetRoleMeta::Button { selected: None },
                            visible: visible_flag,
                            ..Default::default()
                        },
                    );

                    ui.add_space(200.0);

                    let hidden = ui.button("Hidden");
                    let hidden_flag = ui.is_rect_visible(hidden.rect);
                    record_widget(
                        &inner.widgets,
                        "hidden".to_string(),
                        &hidden,
                        WidgetMeta {
                            role: WidgetRoleMeta::Button { selected: None },
                            visible: hidden_flag,
                            ..Default::default()
                        },
                    );
                });
        });
        inner.widgets.finalize_registry(ctx.viewport_id());
    }));

    let result = listed_widgets(&inner, Some(false), None, None, None, None);
    let tags: Vec<_> = result.iter().map(|entry| entry.id.as_str()).collect();
    assert!(tags.contains(&"visible"));
    assert!(!tags.contains(&"hidden"));

    let result = listed_widgets(&inner, Some(true), None, None, None, None);
    let tags: Vec<_> = result.iter().map(|entry| entry.id.as_str()).collect();
    assert!(tags.contains(&"visible"));
    assert!(tags.contains(&"hidden"));
}

#[tokio::test]
async fn widget_lookup_missing_returns_search_details() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);
    for (index, id) in [
        "basic.abort",
        "basic.account",
        "basic.archive",
        "basic.cancel",
        "basic.delete",
    ]
    .into_iter()
    .enumerate()
    {
        inner.widgets.record_widget(
            viewport_id,
            make_entry(id, index as u64 + 1, WidgetRole::Button),
        );
    }
    inner.widgets.record_widget(
        viewport_id,
        make_entry("basic.submit", 20, WidgetRole::Button),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result =
        resolve_widget(&inner, None, &widget_ref_id("basic.submt")).expect_err("missing widget");
    assert_eq!(result.code(), ErrorCode::NotFound);
    assert!(result.message().contains("basic.submt"));
    let suggestions = result
        .details()
        .and_then(|details| details.get("search"))
        .and_then(|search| search.get("suggestions"))
        .and_then(Value::as_array)
        .expect("suggestions");
    assert!(
        suggestions
            .iter()
            .any(|suggestion| suggestion.as_str() == Some("basic.submit"))
    );
}

#[tokio::test]
async fn widget_lookup_missing_reports_exact_match_in_other_viewport() {
    let inner = Arc::new(Inner::new());
    let root = egui::ViewportId::ROOT;
    let secondary = egui::ViewportId::from_hash_of("secondary");

    let ctx = egui::Context::default();
    let mut raw_input = egui::RawInput {
        viewport_id: root,
        ..Default::default()
    };
    raw_input.viewports.insert(root, Default::default());
    raw_input.viewports.insert(secondary, Default::default());
    discard_output(ctx.run_ui(raw_input, |_| {}));
    inner
        .viewports
        .name_viewport(secondary, "secondary".to_string());
    inner.viewports.update_viewports(&ctx);

    inner.widgets.clear_registry(root);
    inner.widgets.finalize_registry(root);
    inner.widgets.clear_registry(secondary);
    inner.widgets.record_widget(
        secondary,
        make_entry("viewports.unwired.value", 1, WidgetRole::Slider),
    );
    inner.widgets.finalize_registry(secondary);

    let result = resolve_widget(&inner, None, &widget_ref_id("viewports.unwired.value"))
        .expect_err("root-scoped lookup should miss secondary widget");
    assert_eq!(result.code(), ErrorCode::NotFound);
    let search = result
        .details()
        .and_then(|details| details.get("search"))
        .expect("search details");
    let exact_matches = search
        .get("exact_matches")
        .and_then(Value::as_array)
        .expect("exact matches");
    assert!(exact_matches.iter().any(|entry| {
        entry.get("id").and_then(Value::as_str) == Some("viewports.unwired.value")
            && entry
                .get("viewport")
                .and_then(|viewport| viewport.get("name"))
                .and_then(Value::as_str)
                == Some("secondary")
    }));
    let suggestions = search
        .get("suggestions")
        .and_then(Value::as_array)
        .expect("suggestions");
    assert!(
        suggestions
            .iter()
            .any(|suggestion| suggestion.as_str() == Some("viewports.unwired.value"))
    );
}

#[tokio::test]
async fn widget_lookup_round_trips_widget_list_id() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    let big_id = (1_u64 << 63) + 5;
    assert!(big_id > (1_u64 << 53));

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("big", big_id, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let list = listed_widgets(&inner, Some(true), None, None, None, None);
    let entry = list
        .iter()
        .find(|entry| entry.id == "big")
        .expect("big widget");
    assert_eq!(entry.id, "big");

    let fetched = resolve_widget(&inner, None, &widget_ref_id(&entry.id)).expect("widget lookup");
    assert_eq!(fetched.id, "big");
}

#[tokio::test]
async fn widget_lookup_fails_on_duplicate_explicit_ids() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "dup",
            1,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            Some("left"),
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "dup",
            2,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            Some("right"),
        ),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = resolve_widget(&inner, None, &widget_ref_id("dup"))
        .expect_err("duplicate explicit ids should block automation");
    assert_eq!(result.code(), ErrorCode::InstrumentationFault);
    let duplicate_ids = result
        .details()
        .and_then(|details| details.get("duplicate_ids"))
        .and_then(|value| value.as_array())
        .expect("duplicate ids");
    assert_eq!(duplicate_ids.len(), 1);
    assert_eq!(
        duplicate_ids[0].get("id").and_then(Value::as_str),
        Some("dup")
    );
    let parent_chain = duplicate_ids[0]
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("parent_chain"))
        .and_then(Value::as_array)
        .expect("parent chain");
    assert!(
        parent_chain
            .iter()
            .any(|parent| parent.as_str() == Some("left"))
    );
}

#[tokio::test]
async fn widget_lookup_generated_duplicates_remain_ambiguous() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut first = make_generated_entry(10, WidgetRole::Button);
    first.id = "dup".to_string();
    inner.widgets.record_widget(viewport_id, first);
    let mut second = make_generated_entry(11, WidgetRole::Button);
    second.id = "dup".to_string();
    inner.widgets.record_widget(viewport_id, second);
    inner.widgets.finalize_registry(viewport_id);

    let result = resolve_widget(&inner, None, &widget_ref_id("dup")).expect_err("ambiguous id");
    assert_eq!(result.code(), ErrorCode::NotFound);
    assert_ne!(result.code(), ErrorCode::InstrumentationFault);
}

#[tokio::test]
async fn widget_lookup_accepts_generated_hex_id() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_generated_entry(5, WidgetRole::Button));
    inner.widgets.finalize_registry(viewport_id);

    let list = listed_widgets(&inner, Some(true), None, None, None, None);
    let entry = list
        .iter()
        .find(|entry| entry.id == "5")
        .expect("generated widget");

    let fetched = resolve_widget(&inner, None, &widget_ref_id(&entry.id))
        .expect("widget lookup by generated id");
    assert_eq!(fetched.id, entry.id);
}

#[tokio::test]
async fn widget_list_filters_role_and_id_prefix() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner
        .widgets
        .record_widget(viewport_id, make_entry("alpha", 1, WidgetRole::Button));
    inner.widgets.record_widget(
        viewport_id,
        make_entry("filter.match", 2, WidgetRole::Slider),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry("filter.skip", 3, WidgetRole::Button),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = listed_widgets(
        &inner,
        Some(true),
        Some(WidgetRole::Slider),
        Some("filter."),
        None,
        None,
    );
    let tags: Vec<_> = result.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(tags, vec!["filter.match"]);
}

#[tokio::test]
async fn pointer_cover_ignores_an_enclosing_container() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    let full = Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 { x: 100.0, y: 100.0 },
    };
    let button = Rect {
        min: Pos2 { x: 10.0, y: 10.0 },
        max: Pos2 { x: 30.0, y: 30.0 },
    };
    let position = Pos2 { x: 20.0, y: 20.0 };

    inner.widgets.clear_registry(viewport_id);
    // A container is recorded after its contents, so it is the last hit at
    // any point inside it.
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "app.action",
            2,
            WidgetRole::Button,
            button,
            Some("app.root"),
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect("app.root", 1, WidgetRole::Unknown, full, None),
    );
    inner.widgets.finalize_registry(viewport_id);

    assert_eq!(
        covering_widget_id(&inner, viewport_id, position, "app.action"),
        None
    );

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "app.action",
            2,
            WidgetRole::Button,
            button,
            Some("app.root"),
        ),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect("app.root", 1, WidgetRole::Unknown, full, None),
    );
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect("app.sheet", 3, WidgetRole::Window, full, None),
    );
    inner.widgets.finalize_registry(viewport_id);

    assert_eq!(
        covering_widget_id(&inner, viewport_id, position, "app.action"),
        Some("app.sheet".to_string())
    );
}

#[tokio::test]
async fn pointer_cover_ranks_a_later_layer_above_registry_order() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;
    let full = Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 { x: 100.0, y: 100.0 },
    };
    let item = Rect {
        min: Pos2 { x: 10.0, y: 10.0 },
        max: Pos2 { x: 30.0, y: 30.0 },
    };
    let position = Pos2 { x: 20.0, y: 20.0 };

    inner.widgets.clear_registry(viewport_id);
    // A menu item paints in a later layer, and the panel behind it is
    // recorded afterwards.
    let mut menu_item =
        make_entry_with_rect("menu.item", 1, WidgetRole::Button, item, Some("menu"));
    menu_item.layer_order = 2;
    inner.widgets.record_widget(viewport_id, menu_item);
    let mut panel = make_entry_with_rect("panel.group", 2, WidgetRole::Unknown, full, None);
    panel.layer_order = 0;
    inner.widgets.record_widget(viewport_id, panel);
    inner.widgets.finalize_registry(viewport_id);

    assert_eq!(
        covering_widget_id(&inner, viewport_id, position, "menu.item"),
        None
    );
    assert_eq!(
        covering_widget_id(&inner, viewport_id, position, "panel.group"),
        Some("menu.item".to_string())
    );
}

#[tokio::test]
async fn widget_list_filters_label_and_label_contains() {
    let inner = Arc::new(Inner::new());
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    let mut ready = make_entry("status.ready", 1, WidgetRole::Label);
    ready.label = Some("Ready state".to_string());
    inner.widgets.record_widget(viewport_id, ready);
    let mut busy = make_entry("status.busy", 2, WidgetRole::Label);
    busy.label = Some("Busy state".to_string());
    inner.widgets.record_widget(viewport_id, busy);
    inner.widgets.finalize_registry(viewport_id);

    let exact = listed_widgets(&inner, Some(true), None, None, Some("Ready state"), None);
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].id, "status.ready");

    let contains = listed_widgets(&inner, Some(true), None, None, None, Some("state"));
    let tags: Vec<_> = contains.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(tags, vec!["status.ready", "status.busy"]);
}

#[test]
fn collect_widget_list_filters_boolean_fields() {
    let inner = Inner::new();
    let viewport_id = egui::ViewportId::ROOT;
    inner.widgets.clear_registry(viewport_id);

    let mut focused = make_entry("focused", 1, WidgetRole::Button);
    focused.focused = true;
    inner.widgets.record_widget(viewport_id, focused);

    let mut hidden = make_entry("hidden", 2, WidgetRole::Button);
    hidden.visible = false;
    inner.widgets.record_widget(viewport_id, hidden);

    let mut disabled = make_entry("disabled", 3, WidgetRole::Button);
    disabled.enabled = false;
    inner.widgets.record_widget(viewport_id, disabled);

    let mut selected = make_entry("selected", 4, WidgetRole::Button);
    selected.role_state = Some(RoleState::Button { selected: true });
    inner.widgets.record_widget(viewport_id, selected);

    let mut covered = make_entry("covered", 5, WidgetRole::Button);
    covered.covered = true;
    inner.widgets.record_widget(viewport_id, covered);

    inner.widgets.finalize_registry(viewport_id);

    let focused_only = collect_widget_list(
        &inner,
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(true),
        None,
        None,
    )
    .expect("focused filter");
    assert_eq!(
        focused_only
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["focused"]
    );

    let hidden_only = collect_widget_list(
        &inner,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(false),
        None,
        None,
        None,
        None,
    )
    .expect("hidden filter");
    assert_eq!(
        hidden_only
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["hidden"]
    );

    let enabled_only = collect_widget_list(
        &inner,
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        Some(true),
        None,
        None,
        None,
    )
    .expect("enabled filter");
    let enabled_ids: Vec<_> = enabled_only.iter().map(|entry| entry.id.as_str()).collect();
    assert!(enabled_ids.contains(&"focused"));
    assert!(enabled_ids.contains(&"selected"));
    assert!(!enabled_ids.contains(&"disabled"));

    let selected_only = collect_widget_list(
        &inner,
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(true),
        None,
    )
    .expect("selected filter");
    assert_eq!(
        selected_only
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["selected"]
    );

    let covered_only = collect_widget_list(
        &inner,
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(true),
    )
    .expect("covered filter");
    assert_eq!(
        covered_only
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["covered"]
    );

    let all = collect_widget_list(
        &inner,
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("unfiltered list");
    let all_ids: Vec<_> = all.iter().map(|entry| entry.id.as_str()).collect();
    assert!(all_ids.contains(&"hidden"));
    assert!(all_ids.contains(&"disabled"));
}

#[tokio::test]
async fn widget_list_includes_values() {
    let inner = Arc::new(Inner::new());
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput::default();
    let mut checked = true;
    let mut intensity = 42.0_f32;
    discard_output(ctx.run_ui(raw_input, |ctx| {
        inner.capture_context(ctx.viewport_id(), ctx);
        inner.widgets.clear_registry(ctx.viewport_id());
        egui::CentralPanel::default().show(ctx, |ui| {
            let checkbox = ui.checkbox(&mut checked, "Enabled");
            let checkbox_visible = ui.is_visible() && ui.is_rect_visible(checkbox.rect);
            record_widget(
                &inner.widgets,
                "basic.enabled".to_string(),
                &checkbox,
                WidgetMeta {
                    role: WidgetRoleMeta::Checkbox {
                        indeterminate: None,
                    },
                    label: Some("Enabled".to_string()),
                    value: Some(WidgetValue::Bool(checked)),
                    visible: checkbox_visible,
                    ..Default::default()
                },
            );

            let slider = ui.add(egui::Slider::new(&mut intensity, 0.0..=100.0));
            let slider_visible = ui.is_visible() && ui.is_rect_visible(slider.rect);
            record_widget(
                &inner.widgets,
                "basic.intensity".to_string(),
                &slider,
                WidgetMeta {
                    role: WidgetRoleMeta::Slider {
                        range: WidgetRange {
                            min: 0.0,
                            max: 100.0,
                        },
                    },
                    value: Some(WidgetValue::Float(f64::from(intensity))),
                    visible: slider_visible,
                    ..Default::default()
                },
            );
        });
        inner.widgets.finalize_registry(ctx.viewport_id());
    }));

    let result = listed_widgets(&inner, Some(true), None, None, None, None);
    let enabled = result
        .iter()
        .find(|entry| entry.id == "basic.enabled")
        .expect("enabled widget");
    let intensity_entry = result
        .iter()
        .find(|entry| entry.id == "basic.intensity")
        .expect("intensity widget");
    assert!(matches!(enabled.value, Some(WidgetValue::Bool(true))));
    assert!(matches!(
        intensity_entry.value,
        Some(WidgetValue::Float(value)) if (value - 42.0).abs() < f64::EPSILON
    ));
}

#[tokio::test]
async fn widget_at_point_reports_topmost() {
    let inner = Arc::new(Inner::new());
    let server = DevMcpServer::new(Arc::clone(&inner));
    let viewport_id = egui::ViewportId::ROOT;

    inner.widgets.clear_registry(viewport_id);
    inner.widgets.record_widget(
        viewport_id,
        make_entry_with_rect(
            "bottom",
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
            "top",
            2,
            WidgetRole::Button,
            Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 10.0, y: 10.0 },
            },
            None,
        ),
    );
    inner.widgets.finalize_registry(viewport_id);

    let result = server
        .widget_at_point_result(Pos2 { x: 5.0, y: 5.0 }, Some(true), None)
        .expect("widget at point");
    assert_eq!(result.widgets.len(), 2);
    assert_eq!(result.widgets[0].id, "top");
}
