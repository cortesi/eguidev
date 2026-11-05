use std::collections::BTreeMap;

use super::{
    FixtureParam, FixtureSpec, FixtureTargetSpec, Modifiers, Pos2, RawInputAction, RawInputEvent,
    Rect, RoleState, Vec2, ViewportCondition, ViewportSel, WidgetCondition, WidgetRange,
    WidgetRegistryEntry, WidgetRole, WidgetRoleMeta, WidgetState, WidgetValue,
    validate_viewport_name,
};

#[test]
fn each_role_survives_a_round_trip_through_its_accesskit_role() {
    let roles = [
        WidgetRole::Button,
        WidgetRole::Link,
        WidgetRole::Image,
        WidgetRole::Label,
        WidgetRole::TextEdit,
        WidgetRole::Slider,
        WidgetRole::Checkbox,
        WidgetRole::ComboBox,
        WidgetRole::Radio,
        WidgetRole::DragValue,
        WidgetRole::Toggle,
        WidgetRole::Separator,
        WidgetRole::ScrollArea,
        WidgetRole::CollapsingHeader,
        WidgetRole::Window,
        WidgetRole::ProgressBar,
        WidgetRole::ColorPicker,
        WidgetRole::Unknown,
    ];
    for role in roles {
        assert_eq!(WidgetRole::from(role.accesskit_role()), role, "{role:?}");
    }
    // AccessKit has no role for these, so they share egui's role.
    assert_eq!(WidgetRole::Selectable.accesskit_role(), egui::Role::Button);
    assert_eq!(WidgetRole::MenuButton.accesskit_role(), egui::Role::Button);
    assert_eq!(
        WidgetRole::Spinner.accesskit_role(),
        egui::Role::ProgressIndicator
    );
}

#[test]
fn role_meta_from_accesskit_requires_the_metadata_of_its_entry() {
    let range = WidgetRange { min: 0.0, max: 1.0 };
    assert_eq!(
        WidgetRoleMeta::from_accesskit(egui::Role::Slider, Some(range)),
        Some(WidgetRoleMeta::Slider { range })
    );
    assert_eq!(
        WidgetRoleMeta::from_accesskit(egui::Role::Slider, None),
        None
    );
    assert_eq!(
        WidgetRoleMeta::from_accesskit(egui::Role::ComboBox, None),
        None
    );
    assert_eq!(
        WidgetRoleMeta::from_accesskit(egui::Role::ScrollView, None),
        None
    );
    assert_eq!(WidgetRoleMeta::from_accesskit(egui::Role::Row, None), None);
    let password =
        WidgetRoleMeta::from_accesskit(egui::Role::PasswordInput, None).expect("password input");
    assert_eq!(
        password,
        WidgetRoleMeta::TextEdit {
            multiline: false,
            password: true,
        }
    );
    assert_eq!(password.accesskit_role(), egui::Role::PasswordInput);
    assert_eq!(
        WidgetRoleMeta::from_accesskit(egui::Role::Label, None),
        Some(WidgetRoleMeta::Plain(WidgetRole::Label))
    );
}

#[test]
fn role_meta_projects_role_and_state_for_every_variant() {
    let cases = [
        (
            WidgetRoleMeta::Plain(WidgetRole::Label),
            WidgetRole::Label,
            None,
        ),
        (
            WidgetRoleMeta::ScrollArea {
                offset: Vec2 { x: 0.0, y: 8.0 },
                viewport_size: Vec2 { x: 100.0, y: 40.0 },
                content_size: Vec2 { x: 100.0, y: 400.0 },
            },
            WidgetRole::ScrollArea,
            Some(RoleState::ScrollArea {
                offset: Vec2 { x: 0.0, y: 8.0 },
                viewport_size: Vec2 { x: 100.0, y: 40.0 },
                content_size: Vec2 { x: 100.0, y: 400.0 },
            }),
        ),
        (
            WidgetRoleMeta::Slider {
                range: WidgetRange {
                    min: 0.0,
                    max: 10.0,
                },
            },
            WidgetRole::Slider,
            Some(RoleState::Slider {
                range: WidgetRange {
                    min: 0.0,
                    max: 10.0,
                },
            }),
        ),
        (
            WidgetRoleMeta::DragValue { range: None },
            WidgetRole::DragValue,
            Some(RoleState::DragValue { range: None }),
        ),
        (
            WidgetRoleMeta::ComboBox {
                options: vec!["Alpha".to_string()],
            },
            WidgetRole::ComboBox,
            Some(RoleState::ComboBox {
                options: vec!["Alpha".to_string()],
            }),
        ),
        (
            WidgetRoleMeta::Button { selected: None },
            WidgetRole::Button,
            None,
        ),
        (
            WidgetRoleMeta::Button {
                selected: Some(true),
            },
            WidgetRole::Button,
            Some(RoleState::Button { selected: true }),
        ),
        (
            WidgetRoleMeta::Checkbox {
                indeterminate: Some(false),
            },
            WidgetRole::Checkbox,
            Some(RoleState::Checkbox {
                indeterminate: false,
            }),
        ),
        (
            WidgetRoleMeta::TextEdit {
                multiline: true,
                password: false,
            },
            WidgetRole::TextEdit,
            Some(RoleState::TextEdit {
                multiline: true,
                password: false,
            }),
        ),
    ];

    for (meta, role, state) in cases {
        assert_eq!(meta.role(), role, "role projection for {meta:?}");
        assert_eq!(meta.state(), state, "state projection for {meta:?}");
    }
}

#[test]
fn data_anchor_validation_follows_rfc_6901() {
    let accepted = ["", "/analysed", "/a~0b", "/a~1b", "/nested/0/name"];
    for pointer in accepted {
        FixtureTargetSpec::data("status.summary", pointer, 3)
            .validate()
            .unwrap_or_else(|error| panic!("pointer {pointer:?} rejected: {error}"));
    }

    let rejected = ["analysed", "/a~", "/a~2b"];
    for pointer in rejected {
        let error = FixtureTargetSpec::data("status.summary", pointer, 3)
            .validate()
            .expect_err("malformed pointer must be rejected");
        assert!(error.contains("RFC 6901"), "{error}");
    }
}

#[test]
fn fixture_validation_reports_a_malformed_data_anchor_pointer() {
    let error = FixtureSpec::new("viewer.mixed", "Mixed games")
        .ready("status.summary")
        .ready_data("status.summary", "analysed", 3)
        .validate(true)
        .expect_err("malformed pointer must fail fixture validation");
    assert!(error.contains("ready 2"), "{error}");
}

#[test]
fn fixture_transition_is_derived_from_preconditions() {
    let baseline = FixtureSpec::new("baseline", "Independent baseline").ready("status");
    let transition = FixtureSpec::new("transition", "State transition")
        .precondition("entry")
        .ready("status");

    assert!(!baseline.is_transition());
    assert!(transition.is_transition());
}

#[test]
fn widget_conditions_serialize_as_the_shared_record_shape() {
    let cases = [
        (
            WidgetCondition::visible(),
            serde_json::json!({ "visible": true }),
        ),
        (
            WidgetCondition::scroll_ready(),
            serde_json::json!({ "scroll_ready": true }),
        ),
        (
            WidgetCondition::label("Ready"),
            serde_json::json!({ "label": "Ready" }),
        ),
        (
            WidgetCondition::data("/analysed", 3),
            serde_json::json!({ "data": { "pointer": "/analysed", "equals": 3 } }),
        ),
    ];
    for (condition, expected) in cases {
        assert_eq!(
            serde_json::to_value(&condition).expect("serialize"),
            expected
        );
        assert_eq!(
            serde_json::from_value::<WidgetCondition>(expected).expect("deserialize"),
            condition
        );
    }
}

#[test]
fn absent_conditions_reject_state_fields() {
    let widget = WidgetCondition {
        present: Some(false),
        visible: Some(false),
        ..WidgetCondition::default()
    };
    assert!(widget.validate().is_err());

    let viewport = ViewportCondition {
        present: Some(false),
        title_contains: Some("demo".to_string()),
        ..ViewportCondition::default()
    };
    assert!(viewport.validate().is_err());
}

#[test]
fn raw_input_uses_the_public_tagged_shape() {
    let event = RawInputEvent::Key {
        key: "escape".to_string(),
        action: RawInputAction::Press,
        modifiers: Some(Modifiers {
            shift: true,
            ..Modifiers::default()
        }),
    };
    assert_eq!(
        serde_json::to_value(event).expect("serialize"),
        serde_json::json!({
            "type": "key",
            "key": "escape",
            "action": "press",
            "modifiers": {
                "ctrl": false,
                "shift": true,
                "alt": false,
                "command": false,
            },
        })
    );
}

#[test]
fn role_meta_defaults_to_the_unknown_plain_role() {
    let meta = WidgetRoleMeta::default();
    assert_eq!(meta.role(), WidgetRole::Unknown);
    assert_eq!(meta.state(), None);
}

fn fixture_param_map<const N: usize>(
    entries: [(&str, WidgetValue); N],
) -> BTreeMap<String, WidgetValue> {
    entries
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect()
}

fn param_spec() -> FixtureSpec {
    FixtureSpec::new("param.demo", "Parameterized fixture")
        .param(
            FixtureParam::text("mode", "Mode to apply.")
                .default("fast")
                .choices(["fast", "slow"]),
        )
        .param(FixtureParam::float("offset", "Offset in points.").range(0.0, 10.0))
        .param(FixtureParam::int("count", "Item count."))
}

#[test]
fn widget_value_deserializes_tagged_object() {
    let value: WidgetValue = serde_json::from_value(serde_json::json!({"bool": false}))
        .expect("deserialize tagged bool");
    assert_eq!(value, WidgetValue::Bool(false));
}

#[test]
fn widget_value_deserializes_stringified_object() {
    let value: WidgetValue = serde_json::from_value(serde_json::json!("{\"bool\": false}"))
        .expect("deserialize stringified bool");
    assert_eq!(value, WidgetValue::Bool(false));
}

#[test]
fn widget_value_deserializes_plain_text() {
    let value: WidgetValue =
        serde_json::from_value(serde_json::json!("hello")).expect("deserialize text");
    assert_eq!(value, WidgetValue::Text("hello".to_string()));
}

#[test]
fn widget_value_serialization() {
    let v = WidgetValue::Bool(true);
    assert_eq!(serde_json::to_string(&v).unwrap(), "true");
}

#[test]
fn fixture_params_validate_defaults_and_int_to_float() {
    let params = param_spec()
        .validate_params(fixture_param_map([
            ("offset", WidgetValue::Int(3)),
            ("count", WidgetValue::Int(2)),
        ]))
        .expect("valid params");

    assert_eq!(params.text("mode"), "fast");
    assert_eq!(params.float("offset"), 3.0);
    assert_eq!(params.int("count"), 2);
    assert_eq!(
        params.as_map().get("offset"),
        Some(&WidgetValue::Float(3.0))
    );
}

#[test]
fn fixture_float_param_choices_normalize_int_literals() {
    let spec = FixtureSpec::new("float.choice", "Float choice fixture").param(
        FixtureParam::float("scale", "Scale factor.")
            .default(1_i64)
            .choices([1_i64, 2_i64]),
    );

    let defaults = spec
        .validate_params(BTreeMap::new())
        .expect("default params");
    assert_eq!(defaults.float("scale"), 1.0);

    let explicit = spec
        .validate_params(fixture_param_map([("scale", WidgetValue::Int(2))]))
        .expect("explicit params");
    assert_eq!(explicit.float("scale"), 2.0);
}

#[test]
fn fixture_params_reject_unknown_and_missing_params() {
    let unknown = param_spec()
        .validate_params(fixture_param_map([
            ("offset", WidgetValue::Float(3.0)),
            ("count", WidgetValue::Int(2)),
            ("extra", WidgetValue::Bool(true)),
        ]))
        .expect_err("unknown param rejected");
    assert_eq!(unknown.code, "unknown_param");

    let missing = param_spec()
        .validate_params(fixture_param_map([(
            "mode",
            WidgetValue::Text("fast".to_string()),
        )]))
        .expect_err("missing param rejected");
    assert_eq!(missing.code, "missing_param");
}

#[test]
fn fixture_params_reject_type_choice_and_range_errors() {
    let wrong_type = param_spec()
        .validate_params(fixture_param_map([
            ("mode", WidgetValue::Text("fast".to_string())),
            ("offset", WidgetValue::Text("three".to_string())),
            ("count", WidgetValue::Int(2)),
        ]))
        .expect_err("type rejected");
    assert_eq!(wrong_type.code, "invalid_param_type");

    let bad_choice = param_spec()
        .validate_params(fixture_param_map([
            ("mode", WidgetValue::Text("medium".to_string())),
            ("offset", WidgetValue::Float(3.0)),
            ("count", WidgetValue::Int(2)),
        ]))
        .expect_err("choice rejected");
    assert_eq!(bad_choice.code, "invalid_param_choice");

    let below_min = param_spec()
        .validate_params(fixture_param_map([
            ("mode", WidgetValue::Text("fast".to_string())),
            ("offset", WidgetValue::Float(-1.0)),
            ("count", WidgetValue::Int(2)),
        ]))
        .expect_err("range min rejected");
    assert_eq!(below_min.code, "param_below_min");

    let above_max = param_spec()
        .validate_params(fixture_param_map([
            ("mode", WidgetValue::Text("fast".to_string())),
            ("offset", WidgetValue::Float(11.0)),
            ("count", WidgetValue::Int(2)),
        ]))
        .expect_err("range max rejected");
    assert_eq!(above_max.code, "param_above_max");
}

#[test]
fn viewport_selector_parses_canonical_strings() {
    assert_eq!(
        ViewportSel::parse("root").unwrap().to_selector_string(),
        "root"
    );
    assert_eq!(
        ViewportSel::parse("details").unwrap().to_selector_string(),
        "details"
    );
    assert_eq!(
        ViewportSel::parse("vp:AB").unwrap().to_selector_string(),
        "vp:ab"
    );
    assert_eq!(
        ViewportSel::parse("vp:00ff").unwrap().to_selector_string(),
        "vp:ff"
    );
}

#[test]
fn viewport_selector_rejects_invalid_raw_ids() {
    for selector in ["vp:", "vp:+ff", "vp:zz"] {
        assert!(
            ViewportSel::parse(selector).is_err(),
            "{selector} should be rejected"
        );
    }
}

#[test]
fn viewport_name_validation_rejects_empty_and_reserved_names() {
    for name in ["", "   "] {
        let error = validate_viewport_name(name).expect_err("empty name rejected");
        assert_eq!(error.code, "empty_viewport_name");
    }
    for name in ["root", "vp:123"] {
        let error = validate_viewport_name(name).expect_err("reserved name rejected");
        assert_eq!(error.code, "reserved_viewport_name");
    }
}

#[test]
fn scroll_area_meta_computes_max_offset() {
    let scroll = RoleState::ScrollArea {
        offset: Vec2 { x: 2.0, y: 3.0 },
        viewport_size: Vec2 { x: 100.0, y: 40.0 },
        content_size: Vec2 { x: 180.0, y: 150.0 },
    }
    .scroll_state()
    .expect("scroll metadata");

    assert_eq!(scroll.max_offset.x, 80.0);
    assert_eq!(scroll.max_offset.y, 110.0);
}

#[test]
fn scroll_area_meta_clamps_negative_max_offset() {
    let scroll = RoleState::ScrollArea {
        offset: Vec2 { x: 0.0, y: 0.0 },
        viewport_size: Vec2 { x: 100.0, y: 40.0 },
        content_size: Vec2 { x: 80.0, y: 20.0 },
    }
    .scroll_state()
    .expect("scroll metadata");

    assert_eq!(scroll.max_offset.x, 0.0);
    assert_eq!(scroll.max_offset.y, 0.0);
}

#[test]
fn widget_state_projects_selected_for_toggle_roles() {
    fn entry(
        role: WidgetRole,
        value: Option<WidgetValue>,
        role_state: Option<RoleState>,
    ) -> WidgetRegistryEntry {
        let rect = Rect {
            min: Pos2 { x: 0.0, y: 0.0 },
            max: Pos2 { x: 1.0, y: 1.0 },
        };
        WidgetRegistryEntry {
            id: "control".to_string(),
            explicit_id: true,
            native_id: 1,
            viewport_id: "root".to_string(),
            layer_id: egui::LayerId::background(),
            layer_order: 0,
            rect,
            interact_rect: rect,
            role,
            label: None,
            value,
            data: None,
            layout: None,
            role_state,
            parent_id: None,
            enabled: true,
            visible: true,
            focused: false,
            covered: false,
        }
    }

    let toggle = WidgetState::from(&entry(
        WidgetRole::Toggle,
        Some(WidgetValue::Bool(true)),
        None,
    ));
    assert_eq!(toggle.selected, Some(true));

    let checkbox = WidgetState::from(&entry(
        WidgetRole::Checkbox,
        Some(WidgetValue::Bool(false)),
        Some(RoleState::Checkbox {
            indeterminate: false,
        }),
    ));
    assert_eq!(checkbox.selected, Some(false));

    let indeterminate = WidgetState::from(&entry(
        WidgetRole::Checkbox,
        Some(WidgetValue::Bool(true)),
        Some(RoleState::Checkbox {
            indeterminate: true,
        }),
    ));
    assert_eq!(indeterminate.selected, None);

    let selected_button = WidgetState::from(&entry(
        WidgetRole::Button,
        None,
        Some(RoleState::Button { selected: true }),
    ));
    assert_eq!(selected_button.selected, Some(true));
}
