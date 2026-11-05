//! Tests of the smoke run and its failure bundles.

use super::*;

#[test]
fn stable_hash8_is_deterministic_and_path_sensitive() {
    assert_eq!(
        stable_hash8("nested/fail.luau"),
        stable_hash8("nested/fail.luau")
    );
    assert_ne!(
        stable_hash8("nested/fail.luau"),
        stable_hash8("other/fail.luau")
    );
    assert_eq!(stable_hash8("nested/fail.luau").len(), 8);
}

#[test]
fn replace_dir_overwrites_existing_bundle_directory() {
    let tempdir = test_tempdir();
    let bundle_dir = tempdir.path().join("bundle");
    fs::create_dir_all(&bundle_dir).expect("create bundle");
    fs::write(bundle_dir.join("old.txt"), "old").expect("write old");

    replace_dir(&bundle_dir).expect("replace dir");

    assert!(bundle_dir.is_dir());
    assert!(!bundle_dir.join("old.txt").exists());
}

#[test]
fn bundle_meta_and_failure_text_include_script_context() {
    let tempdir = test_tempdir();
    let outcome = parse_script_eval_outcome(
        &CallToolResult::new()
            .with_json_text(serde_json::json!({
                "success": false,
                "logs": ["before failure"],
                "assertions": [{
                    "passed": false,
                    "message": "expected ready",
                    "location": "fail.luau:3"
                }],
                "fixtures": [{
                    "name": "basic.default",
                    "params": {
                        "offset": 180
                    }
                }],
                "timing": {
                    "compile_ms": 0,
                    "exec_ms": 1,
                    "total_ms": 1
                },
                "egui_diagnostics": {
                    "entries": [{
                        "kind": "id_clash",
                        "severity": "warning",
                        "message": "duplicate id",
                        "viewport_id": "root",
                        "frame": 4
                    }],
                    "dropped": 0
                },
                "error": {
                    "type": "assertion",
                    "message": "expected ready",
                    "code": "assertion_failed",
                    "details": {
                        "widget": "basic.status"
                    }
                }
            }))
            .expect("script eval json"),
    )
    .expect("outcome");
    let context = BundleContext {
        dir: tempdir.path().join("bundles"),
        launch: test_config(tempdir.path().to_path_buf()),
        stderr_buffer: Arc::new(Mutex::new(Vec::new())),
        stdout_buffer: Arc::new(Mutex::new(Vec::new())),
        collection_timeout_ms: 10_000,
    };
    let args = ScriptArgs::from([(
        "name".to_string(),
        ScriptArgValue::String("Sky".to_string()),
    )]);

    let modules = ScriptModules::from([(
        "layout.luau".to_string(),
        "return { strict = true }".to_string(),
    )]);
    let meta = bundle_meta(
        &context,
        "nested/fail.luau",
        Some(2),
        &args,
        &modules,
        &outcome,
    )
    .expect("bundle meta");
    let meta: serde_json::Value = serde_json::from_str(&meta).expect("meta json");
    assert_eq!(meta["script"]["path"], "nested/fail.luau");
    assert_eq!(meta["script"]["round"], 2);
    assert_eq!(meta["script"]["args"]["name"], "Sky");
    assert_eq!(meta["modules"][0], "layout.luau");
    assert_eq!(meta["fixtures"][0]["name"], "basic.default");
    assert_eq!(meta["fixtures"][0]["params"]["offset"], 180);
    assert_eq!(meta["failure"]["details"]["widget"], "basic.status");
    assert_eq!(
        meta["failure"]["egui_diagnostics"]["entries"][0]["kind"],
        "id_clash"
    );

    let text = failure_text(&outcome).expect("failure text");
    assert!(text.contains("failure: expected ready"));
    assert!(text.contains("before failure"));
    assert!(text.contains("basic.default"));
    assert!(text.contains("egui diagnostics"));
    assert!(text.contains("duplicate id"));
}

#[test]
fn snapshot_output_preserves_captured_output() {
    let empty = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(snapshot_output(&empty), "");

    let captured = Arc::new(Mutex::new(b"captured stdout\n".to_vec()));
    assert_eq!(snapshot_output(&captured), "captured stdout\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_smoke_suite_writes_deterministic_failure_bundle() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (app, _handle) = make_bundle_smoke_app(Arc::clone(&requests)).await;
    let tempdir = test_tempdir();
    let suite_dir = tempdir.path().join("suite");
    fs::create_dir_all(&suite_dir).expect("create suite");
    fs::write(suite_dir.join("10_fail.luau"), "assert(false, \"boom\")").expect("write script");
    let module_dir = tempdir.path().join("modules");
    fs::create_dir_all(&module_dir).expect("create modules");
    fs::write(module_dir.join("layout.luau"), "return { strict = true }").expect("write module");
    let bundle_dir = tempdir.path().join("bundles");
    let stderr_buffer = Arc::new(Mutex::new(b"app stderr\n".to_vec()));
    let stdout_buffer = Arc::new(Mutex::new(b"app stdout\n".to_vec()));
    let context = BundleContext {
        dir: bundle_dir.clone(),
        launch: test_config(tempdir.path().to_path_buf()),
        stderr_buffer,
        stdout_buffer,
        collection_timeout_ms: 1_000,
    };
    let config = SmokeConfig {
        launch: Some(test_config(tempdir.path().to_path_buf())),
        suite: SuiteConfig {
            suite_dir,
            scripts: Vec::new(),
            only: Vec::new(),
            suite_timeout: Duration::from_secs(10),
            script_timeout: Some(Duration::from_secs(1)),
            max_instructions: None,
            fail_fast: false,
            fail_on_egui_diagnostics: true,
            run_mode: SuiteRunMode::ONCE,
            args: ScriptArgs::from([(
                "name".to_string(),
                ScriptArgValue::String("Sky".to_string()),
            )]),
        },
        verbose_output: false,
        list: false,
        list_json: false,
        bundle_dir: Some(bundle_dir.clone()),
        module_dir: Some(module_dir),
    };

    let result = run_smoke_suite(Arc::clone(&app.client), &config, Some(context.clone()))
        .await
        .expect("smoke suite");

    assert_eq!(result.failed(), 1);
    assert_eq!(result.results[0].message.as_deref(), Some("boom"));
    assert_eq!(result.results[0].path, "10_fail.luau");
    let script_dir = bundle_dir.join(format!(
        "{}-{}",
        safe_file_component(&result.results[0].path),
        stable_hash8(&result.results[0].path)
    ));
    assert!(
        script_dir.join("meta.json").is_file(),
        "bundle entries: {:?}",
        fs::read_dir(&bundle_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    );
    assert!(script_dir.join("failure.txt").is_file());
    assert_eq!(
        fs::read_to_string(script_dir.join("script.luau")).expect("script source"),
        "assert(false, \"boom\")"
    );
    assert_eq!(
        fs::read_to_string(script_dir.join("modules/layout.luau")).expect("module source"),
        "return { strict = true }"
    );
    assert!(script_dir.join("tree.json").is_file());
    assert!(script_dir.join("tree.txt").is_file());
    assert!(script_dir.join("diagnostics.json").is_file());
    assert_eq!(
        fs::read_to_string(bundle_dir.join("app.stderr.log")).expect("stderr"),
        "app stderr\n"
    );
    assert_eq!(
        fs::read_to_string(bundle_dir.join("app.stdout.log")).expect("stdout"),
        "app stdout\n"
    );
    assert!(!script_dir.join("app.stderr.log").exists());
    assert!(!script_dir.join("app.stdout.log").exists());
    assert_eq!(
        fs::read(script_dir.join("viewport-root.jpg")).expect("viewport image"),
        b"jpeg"
    );
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(script_dir.join("meta.json")).expect("meta"))
            .expect("meta json");
    assert_eq!(meta["script"]["args"]["name"], "Sky");
    assert_eq!(meta["fixtures"][0]["name"], "basic.default");
    assert_eq!(meta["fixtures"][0]["params"]["offset"], 180);
    assert_eq!(meta["app_output"]["stderr"], "../app.stderr.log");
    assert_eq!(meta["app_output"]["stdout"], "../app.stdout.log");

    // A second run removes each bundle of the first run and keeps a
    // folder that is not a bundle.
    fs::write(script_dir.join("stale.txt"), "stale").expect("write stale");
    let renamed = bundle_dir.join("20_renamed-00000000");
    fs::create_dir_all(&renamed).expect("create stale bundle");
    fs::write(renamed.join("meta.json"), "{}").expect("write stale meta");
    fs::write(renamed.join("failure.txt"), "old").expect("write stale failure");
    let notes = bundle_dir.join("notes");
    fs::create_dir_all(&notes).expect("create notes");
    let second = run_smoke_suite(Arc::clone(&app.client), &config, Some(context))
        .await
        .expect("second smoke suite");
    assert_eq!(second.failed(), 1);
    assert!(!script_dir.join("stale.txt").exists());
    assert!(!renamed.exists(), "a bundle of an earlier run is removed");
    assert!(notes.is_dir(), "a folder that is not a bundle stays");
    assert_eq!(requests.lock().expect("requests").len(), 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_smoke_suite_writes_round_suffixed_failure_bundles() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (app, _handle) = make_bundle_smoke_app(Arc::clone(&requests)).await;
    let tempdir = test_tempdir();
    let suite_dir = tempdir.path().join("suite");
    fs::create_dir_all(&suite_dir).expect("create suite");
    fs::write(suite_dir.join("10_fail.luau"), "assert(false, \"boom\")").expect("write script");
    let bundle_dir = tempdir.path().join("bundles");
    let context = BundleContext {
        dir: bundle_dir.clone(),
        launch: test_config(tempdir.path().to_path_buf()),
        stderr_buffer: Arc::new(Mutex::new(Vec::new())),
        stdout_buffer: Arc::new(Mutex::new(Vec::new())),
        collection_timeout_ms: 1_000,
    };
    let config = SmokeConfig {
        launch: Some(test_config(tempdir.path().to_path_buf())),
        suite: SuiteConfig {
            suite_dir,
            scripts: Vec::new(),
            only: Vec::new(),
            suite_timeout: Duration::from_secs(10),
            script_timeout: Some(Duration::from_secs(1)),
            max_instructions: None,
            fail_fast: false,
            fail_on_egui_diagnostics: true,
            run_mode: SuiteRunMode::Repeat(2),
            args: ScriptArgs::default(),
        },
        verbose_output: false,
        list: false,
        list_json: false,
        bundle_dir: Some(bundle_dir.clone()),
        module_dir: None,
    };

    let result = run_smoke_suite(Arc::clone(&app.client), &config, Some(context))
        .await
        .expect("smoke suite");

    assert_eq!(result.failed(), 2);
    for round in 1_u32..=2 {
        let key = format!("10_fail.luau-round-{round}");
        let script_dir = bundle_dir.join(format!(
            "{}-{}",
            safe_file_component(&key),
            stable_hash8(&key)
        ));
        assert!(
            script_dir.join("meta.json").is_file(),
            "bundle entries: {:?}",
            fs::read_dir(&bundle_dir)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.file_name())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        );
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(script_dir.join("meta.json")).expect("meta"))
                .expect("meta json");
        assert_eq!(meta["script"]["path"], "10_fail.luau");
        assert_eq!(meta["script"]["round"].as_u64(), Some(u64::from(round)));
        assert!(!script_dir.join("app.stderr.log").exists());
    }
    assert!(bundle_dir.join("app.stderr.log").is_file());
    assert!(bundle_dir.join("app.stdout.log").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn run_smoke_suite_preserves_failure_when_bundle_write_fails() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (app, _handle) = make_bundle_smoke_app(Arc::clone(&requests)).await;
    let tempdir = test_tempdir();
    let suite_dir = tempdir.path().join("suite");
    fs::create_dir_all(&suite_dir).expect("create suite");
    fs::write(suite_dir.join("10_fail.luau"), "assert(false, \"boom\")").expect("write script");
    let bundle_root = tempdir.path().join("bundle-root-file");
    fs::write(&bundle_root, "not a directory").expect("write bundle root file");
    let context = BundleContext {
        dir: bundle_root.clone(),
        launch: test_config(tempdir.path().to_path_buf()),
        stderr_buffer: Arc::new(Mutex::new(Vec::new())),
        stdout_buffer: Arc::new(Mutex::new(Vec::new())),
        collection_timeout_ms: 1_000,
    };
    let config = SmokeConfig {
        launch: Some(test_config(tempdir.path().to_path_buf())),
        suite: SuiteConfig {
            suite_dir,
            scripts: Vec::new(),
            only: Vec::new(),
            suite_timeout: Duration::from_secs(10),
            script_timeout: Some(Duration::from_secs(1)),
            max_instructions: None,
            fail_fast: false,
            fail_on_egui_diagnostics: true,
            run_mode: SuiteRunMode::ONCE,
            args: ScriptArgs::default(),
        },
        verbose_output: false,
        list: false,
        list_json: false,
        bundle_dir: Some(bundle_root),
        module_dir: None,
    };

    let result = run_smoke_suite(Arc::clone(&app.client), &config, Some(context))
        .await
        .expect("smoke suite");

    assert_eq!(result.failed(), 1);
    assert_eq!(result.results[0].message.as_deref(), Some("boom"));
    assert_eq!(result.results[0].logs, vec!["before failure"]);
    assert_eq!(result.results[0].fixtures[0].name, "basic.default");
    assert_eq!(requests.lock().expect("requests").len(), 1);
}

#[test]
fn safe_file_component_keeps_filenames_portable() {
    assert_eq!(safe_file_component("form/result 1"), "form-result-1");
    assert_eq!(safe_file_component("***"), "image");
}
