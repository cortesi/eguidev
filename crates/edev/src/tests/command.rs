//! Tests of the commands that evaluate a script, dump a tree, or apply a
//! fixture.

use super::*;

#[test]
fn script_eval_outcome_accepts_json_text_with_image_sidecars() {
    let result = CallToolResult::new()
        .with_content(ContentBlock::image("AQID", "image/png"))
        .with_json_text(serde_json::json!({
            "success": true,
            "value": 42,
            "logs": [],
            "assertions": [],
            "timing": {
                "compile_ms": 0,
                "exec_ms": 0,
                "total_ms": 0
            }
        }))
        .expect("script eval json")
        .with_content(ContentBlock::image("BAUG", "image/png"));

    let outcome = parse_script_eval_outcome(&result).expect("script eval outcome");

    assert_eq!(outcome.value, Some(serde_json::json!(42)));
}

#[test]
fn dump_projection_args_wait_for_initial_capture_without_fixture() {
    let tempdir = test_tempdir();
    let mut config = dump_config(&tempdir);
    config.viewport = Some("secondary".to_string());

    let args = dump_script_args(&config);
    assert_eq!(args["__dump_wait_capture"], ScriptArgValue::Bool(true));
    assert_eq!(args["__dump_json"], ScriptArgValue::Bool(false));
    assert_eq!(
        args["__dump_viewport"],
        ScriptArgValue::String("secondary".to_string())
    );
}

#[test]
fn dump_projection_args_use_fixture_without_extra_capture_wait() {
    let tempdir = test_tempdir();
    let mut config = dump_config(&tempdir);
    config.fixture = Some("basic.default".to_string());
    config.wait_for_initial_capture = false;
    config.json = true;

    let args = dump_script_args(&config);
    assert_eq!(
        args["__fixture_name"],
        ScriptArgValue::String("basic.default".to_string())
    );
    assert_eq!(args["__dump_wait_capture"], ScriptArgValue::Bool(false));
    assert_eq!(args["__dump_json"], ScriptArgValue::Bool(true));
}

#[test]
fn dump_projection_args_pass_fixture_params() {
    let tempdir = test_tempdir();
    let mut config = dump_config(&tempdir);
    config.fixture = Some("basic.scrolled".to_string());
    config
        .params
        .insert("enabled".to_string(), ScriptArgValue::Bool(true));
    config.params.insert(
        "label".to_string(),
        ScriptArgValue::String("A|B".to_string()),
    );
    config
        .params
        .insert("offset".to_string(), ScriptArgValue::Int(180));
    config.wait_for_initial_capture = false;

    let args = dump_script_args(&config);
    assert_eq!(args["enabled"], ScriptArgValue::Bool(true));
    assert_eq!(args["label"], ScriptArgValue::String("A|B".to_string()));
    assert_eq!(args["offset"], ScriptArgValue::Int(180));
}

#[test]
fn eval_output_value_adds_image_files() {
    let mut outcome = successful_outcome(&serde_json::json!({
        "capture": {
            "type": "image_ref",
            "id": "image-0"
        }
    }));
    outcome.images = Some(vec![ScriptImageInfo {
        id: "image-0".to_string(),
        content_index: 1,
        kind: "viewport".to_string(),
        viewport_id: Some("root".to_string()),
        target: None,
        rect: None,
        metadata: None,
    }]);
    let files = BTreeMap::from([(
        "image-0".to_string(),
        PathBuf::from("/tmp/eval/capture-image-0.jpg"),
    )]);

    let output = eval_output_value(&outcome, &files).expect("eval output");

    assert_eq!(
        output["images"][0]["file"],
        serde_json::json!("/tmp/eval/capture-image-0.jpg")
    );
}

#[tokio::test]
async fn eval_script_calls_script_eval_with_timeout_and_args() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (app, _handle) = make_recording_eval_app(Arc::clone(&requests)).await;
    let tempdir = test_tempdir();
    let module_dir = tempdir.path().join("modules");
    fs::create_dir_all(module_dir.join("shared")).expect("create modules");
    fs::write(
        module_dir.join("shared/value.luau"),
        "return { value = 42 }",
    )
    .expect("write module");
    let config = EvalConfig {
        launch: test_config(tempdir.path().to_path_buf()),
        script: tempdir.path().join("probe.luau"),
        out_dir: tempdir.path().join("eval-out"),
        timeout: Some(Duration::from_millis(1_234)),
        max_instructions: Some(12_345),
        args: ScriptArgs::from([(
            "name".to_string(),
            ScriptArgValue::String("Sky".to_string()),
        )]),
        module_dir: Some(module_dir),
    };

    run_eval_script(
        Arc::clone(&app.client),
        &config,
        "return args.name".to_string(),
    )
    .await
    .expect("eval script");

    let requests = requests.lock().expect("requests lock poisoned");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.script, "return args.name");
    assert_eq!(request.timeout_ms, Some(1_234));
    assert_eq!(
        request.options.as_ref().expect("options").max_instructions,
        Some(12_345)
    );
    assert_eq!(
        request.options.as_ref().expect("options").args.get("name"),
        Some(&ScriptArgValue::String("Sky".to_string()))
    );
    assert_eq!(
        request
            .options
            .as_ref()
            .expect("options")
            .modules
            .get("shared/value.luau")
            .map(String::as_str),
        Some("return { value = 42 }")
    );
}

#[test]
fn parse_fixture_list_decodes_registered_fixtures() {
    let fixtures = parse_fixture_list(successful_outcome(&serde_json::json!([
        {
            "name": "basic.default",
            "description": "baseline",
            "ready": [{
                "widget_id": "basic.status",
                "viewport_id": "secondary",
                "condition": { "visible": true }
            }],
            "params": [{
                "name": "offset",
                "kind": "float",
                "description": "Scroll offset.",
                "default": 300.0,
                "min": 0.0,
                "max": 600.0
            }],
            "tags": ["scroll"]
        }
    ])))
    .expect("fixtures");

    assert_eq!(fixtures.len(), 1);
    assert_eq!(fixtures[0].name, "basic.default");
    assert_eq!(fixtures[0].description, "baseline");
    assert_eq!(fixtures[0].ready.len(), 1);
    assert_eq!(fixtures[0].ready[0].widget_id, "basic.status");
    assert_eq!(
        fixtures[0].ready[0].viewport_id.as_deref(),
        Some("secondary")
    );
    assert_eq!(fixtures[0].params[0].name, "offset");
    assert_eq!(fixtures[0].tags, vec!["scroll"]);
}

#[test]
fn parse_fixture_list_defaults_omitted_empty_collections() {
    let fixtures = parse_fixture_list(successful_outcome(&serde_json::json!([
        {
            "name": "basic.empty",
            "description": "empty collections"
        }
    ])))
    .expect("fixtures");

    assert_eq!(fixtures.len(), 1);
    assert!(fixtures[0].preconditions.is_empty());
    assert!(fixtures[0].ready.is_empty());
    assert!(fixtures[0].params.is_empty());
    assert!(fixtures[0].tags.is_empty());
}

#[test]
fn parse_fixture_list_accepts_the_empty_table_of_an_app_without_fixtures() {
    let fixtures = parse_fixture_list(successful_outcome(&serde_json::json!({})))
        .expect("an empty Luau table is an empty fixture list");

    assert!(fixtures.is_empty());
}

#[test]
fn parse_fixture_list_rejects_missing_payload() {
    let outcome = successful_outcome(&serde_json::Value::Null);

    let error = parse_fixture_list(outcome).expect_err("missing payload should fail");
    assert!(matches!(
        error,
        EdevError::FixtureFailed(ref message) if message == "fixtures() returned no value"
    ));
}

#[test]
fn script_eval_error_message_prefers_runtime_error() {
    let message = script_eval_error_message(
        Some(&ScriptErrorInfo {
            error_type: "runtime".to_string(),
            message: "script exploded".to_string(),
            location: None,
            backtrace: None,
            code: None,
            details: None,
        }),
        "fallback",
    );

    assert_eq!(message, "script exploded");
}

#[test]
fn exit_codes_separate_failed_scripts_from_failed_runs() {
    let text = || "failed".to_string();
    for error in [
        EdevError::SmokeFailed(text()),
        EdevError::EvalFailed(text()),
        EdevError::FixtureFailed(text()),
    ] {
        assert_eq!(error.exit_code(), 1, "{error}");
    }
    assert_eq!(EdevError::InvalidArgs(text()).exit_code(), 2);
    for error in [
        EdevError::Mcp(McpError::InternalError(text())),
        EdevError::Io(std_io::Error::other(text())),
        EdevError::AppStart(text()),
        EdevError::RecordFailed(text()),
        EdevError::InstanceRegistry(text()),
        EdevError::ForcedShutdown(text()),
        EdevError::StartTimeout(text()),
    ] {
        assert_eq!(error.exit_code(), 3, "{error}");
    }
}
