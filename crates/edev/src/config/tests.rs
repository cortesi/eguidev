//! Tests of the edev configuration.

use std::{ffi::OsString, fs};

use tempfile::TempDir;

use super::*;

fn tempdir() -> TempDir {
    fs::create_dir_all("tmp").expect("create tmp");
    tempfile::Builder::new()
        .prefix("edev-config-test-")
        .tempdir_in("tmp")
        .expect("tempdir")
}

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn parse_command_defaults_to_help() {
    let args = Vec::<OsString>::new();
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Help(help) = command else {
        panic!("expected help command");
    };
    assert!(help.contains("Usage:"));
    assert!(!help.to_ascii_lowercase().contains("proxy"));
}

#[test]
fn smoke_only_help_says_union() {
    let help = Cli::command()
        .find_subcommand("smoke")
        .expect("smoke")
        .get_arguments()
        .find(|argument| argument.get_long() == Some("only"))
        .expect("only")
        .get_help()
        .expect("help")
        .to_string();
    assert!(help.contains("union"), "{help}");
    assert!(!help.contains("intersect"), "{help}");
}

#[test]
fn mcp_subcommand_help_does_not_say_proxy() {
    let about = Cli::command()
        .find_subcommand("mcp")
        .expect("mcp")
        .get_about()
        .expect("about")
        .to_string();
    assert!(!about.to_ascii_lowercase().contains("proxy"));
}

#[test]
fn parse_command_accepts_docs_subcommand() {
    let args = os_args(&["docs"]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    assert!(matches!(command, EdevCommand::Docs));
}

#[test]
fn parse_command_rejects_docs_with_extra_args() {
    let args = os_args(&["docs", "mcp"]);
    let current_dir = env::current_dir().unwrap();
    let error =
        EdevCommand::parse_args_in_dir(&args, &current_dir).expect_err("extra args should fail");
    assert!(matches!(error, EdevError::InvalidArgs(_)));
}

#[test]
fn parse_mcp_command_accepts_explicit_argv() {
    let args = os_args(&["mcp", "--", "cargo", "run", "--", "--dev-mcp"]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Mcp(config) = command else {
        panic!("expected mcp command");
    };
    assert_eq!(config.launch.command[0], "cargo");
    assert_eq!(config.launch.command.last().expect("last arg"), "--dev-mcp");
}

#[test]
fn presentation_defaults_to_background_and_accepts_cli_override() {
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(
        &os_args(&["mcp", "--presentation", "foreground", "--", "cargo", "run"]),
        &current_dir,
    )
    .expect("parse command");
    let EdevCommand::Mcp(config) = command else {
        panic!("expected mcp command");
    };
    assert_eq!(
        config.launch.presentation,
        presentation::Presentation::Foreground
    );

    let command =
        EdevCommand::parse_args_in_dir(&os_args(&["mcp", "--", "cargo", "run"]), &current_dir)
            .expect("parse command");
    let EdevCommand::Mcp(config) = command else {
        panic!("expected mcp command");
    };
    assert_eq!(
        config.launch.presentation,
        presentation::Presentation::Background
    );
    assert_eq!(config.launch.shutdown_grace, Duration::from_secs(30));
}

#[test]
fn file_shutdown_grace_overrides_the_default() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::write(
        repo_root.join(DEFAULT_CONFIG_FILE),
        "[app]\ncommand = [\"cargo\", \"run\"]\nshutdown_grace_secs = 7\n",
    )
    .expect("write config");

    let command =
        EdevCommand::parse_args_in_dir(&os_args(&["mcp"]), &repo_root).expect("parse command");
    let EdevCommand::Mcp(config) = command else {
        panic!("expected mcp command");
    };
    assert_eq!(config.launch.shutdown_grace, Duration::from_secs(7));
}

#[test]
fn file_presentation_is_used_and_cli_wins() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::write(
        repo_root.join(DEFAULT_CONFIG_FILE),
        "[app]\ncommand = [\"cargo\", \"run\"]\npresentation = \"foreground\"\n",
    )
    .expect("write config");

    let command =
        EdevCommand::parse_args_in_dir(&os_args(&["mcp"]), &repo_root).expect("parse command");
    let EdevCommand::Mcp(config) = command else {
        panic!("expected mcp command");
    };
    assert_eq!(
        config.launch.presentation,
        presentation::Presentation::Foreground
    );

    let command = EdevCommand::parse_args_in_dir(
        &os_args(&["mcp", "--presentation", "background"]),
        &repo_root,
    )
    .expect("parse command");
    let EdevCommand::Mcp(config) = command else {
        panic!("expected mcp command");
    };
    assert_eq!(
        config.launch.presentation,
        presentation::Presentation::Background
    );
}

#[test]
fn parse_smoke_command_parses_typed_args() {
    let args = os_args(&[
        "smoke",
        "smoketest/10_basic.luau",
        "tmp/ad_hoc.luau",
        "--arg",
        "name=Sky",
        "--arg",
        "count=4",
        "--arg",
        "enabled=true",
        "--script-max-instructions",
        "25000000",
        "--",
        "cargo",
        "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Smoke(config) = command else {
        panic!("expected smoke command");
    };
    assert_eq!(
        config.suite.args.get("name"),
        Some(&ScriptArgValue::String("Sky".to_string()))
    );
    assert_eq!(
        config.suite.args.get("count"),
        Some(&ScriptArgValue::Int(4))
    );
    assert_eq!(
        config.suite.args.get("enabled"),
        Some(&ScriptArgValue::Bool(true))
    );
    assert_eq!(config.suite.max_instructions, Some(25_000_000));
    assert_eq!(
        config.suite.scripts,
        vec![
            PathBuf::from("smoketest/10_basic.luau"),
            PathBuf::from("tmp/ad_hoc.luau"),
        ]
    );
    assert_eq!(config.bundle_dir, None);
}

#[test]
fn parse_smoke_command_accepts_authoring_flags() {
    let args = os_args(&[
        "smoke", "--list", "--json", "--only", "*layout*", "--only", "nested/*", "--repeat", "3",
        "--", "cargo", "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Smoke(config) = command else {
        panic!("expected smoke command");
    };

    assert!(config.list);
    assert!(config.list_json);
    assert_eq!(
        config.suite.only,
        vec!["*layout*".to_string(), "nested/*".to_string()]
    );
    assert_eq!(config.suite.run_mode, SuiteRunMode::Repeat(3));
}

#[test]
fn smoke_and_eval_resolve_the_same_module_directory() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::create_dir_all(repo_root.join("smoketest/modules")).expect("create modules");
    fs::write(
        repo_root.join(DEFAULT_CONFIG_FILE),
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
module_dir = \"smoketest/modules\"
",
    )
    .expect("write config");

    let smoke =
        EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse smoke");
    let EdevCommand::Smoke(smoke) = smoke else {
        panic!("expected smoke command");
    };
    let eval = EdevCommand::parse_args_in_dir(&os_args(&["eval", "tmp/probe.luau"]), &repo_root)
        .expect("parse eval");
    let EdevCommand::Eval(eval) = eval else {
        panic!("expected eval command");
    };

    let expected = repo_root.join("smoketest/modules");
    assert_eq!(smoke.module_dir, Some(expected.clone()));
    assert_eq!(eval.module_dir, Some(expected));
}

#[test]
fn parse_smoke_command_accepts_until_fail() {
    let args = os_args(&["smoke", "--until-fail", "5", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Smoke(config) = command else {
        panic!("expected smoke command");
    };

    assert_eq!(config.suite.run_mode, SuiteRunMode::UntilFail(5));
}

#[test]
fn parse_smoke_command_rejects_only_with_explicit_scripts() {
    let args = os_args(&[
        "smoke",
        "--only",
        "*visual*",
        "smoketest/65_visual_sampling.luau",
        "--",
        "cargo",
        "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
        .expect_err("only with explicit script should fail");

    assert!(
        matches!(error, EdevError::InvalidArgs(message) if message.contains("cannot be combined"))
    );
}

#[test]
fn parse_smoke_list_command_does_not_require_app_command() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::create_dir_all(repo_root.join("smoketest")).expect("create suite dir");
    fs::write(
        repo_root.join("smoketest").join("10_bootstrap.luau"),
        "return true",
    )
    .expect("write script");

    let command = EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--list"]), &repo_root)
        .expect("parse command");
    let EdevCommand::Smoke(config) = command else {
        panic!("expected smoke command");
    };

    assert!(config.list);
    assert!(config.launch.is_none());
    assert_eq!(config.suite.suite_dir, repo_root.join("smoketest"));
}

#[test]
fn parse_smoke_command_accepts_bundle_dir() {
    let args = os_args(&[
        "smoke",
        "--bundle-dir",
        "tmp/custom-bundles",
        "--",
        "cargo",
        "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Smoke(config) = command else {
        panic!("expected smoke command");
    };
    assert_eq!(
        config.bundle_dir,
        Some(current_dir.join("tmp/custom-bundles"))
    );
}

#[test]
fn parse_record_command_accepts_smoke_options_and_scripts() {
    let args = os_args(&[
        "record",
        "tmp/demo.mov",
        "smoketest/10_basic.luau",
        "--window-title",
        "Egui DevMCP Demo",
        "--arg",
        "name=Sky",
        "--fail-fast",
        "--bundle-dir",
        "tmp/record-bundles",
        "--",
        "cargo",
        "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Record(config) = command else {
        panic!("expected record command");
    };

    assert_eq!(config.outfile, current_dir.join("tmp/demo.mov"));
    assert_eq!(config.window_title.as_deref(), Some("Egui DevMCP Demo"));
    assert_eq!(
        config.smoke.suite.scripts,
        vec![PathBuf::from("smoketest/10_basic.luau")]
    );
    assert_eq!(
        config.smoke.suite.args.get("name"),
        Some(&ScriptArgValue::String("Sky".to_string()))
    );
    assert!(config.smoke.suite.fail_fast);
    assert_eq!(
        config.smoke.bundle_dir,
        Some(current_dir.join("tmp/record-bundles"))
    );
    assert_eq!(
        config.smoke.launch.as_ref().expect("launch").command[0],
        "cargo"
    );
}

#[test]
fn parse_record_command_rejects_non_mov_outfile() {
    let args = os_args(&["record", "smoketest/10_basic.luau", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
        .expect_err("record outfile must be mov");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };

    assert!(message.contains("requires a .mov output path"));
    assert!(message.contains("smoketest/10_basic.luau"));
}

#[test]
fn parse_record_command_rejects_list_mode() {
    let args = os_args(&["record", "tmp/demo.mov", "--list"]);
    let current_dir = env::current_dir().unwrap();
    let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
        .expect_err("record list mode should fail");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };

    assert!(message.contains("cannot be combined with `--list`"));
}

#[test]
fn parse_eval_command_parses_script_args_and_output_dir() {
    let args = os_args(&[
        "eval",
        "tmp/probe.luau",
        "--out-dir",
        "tmp/eval-output",
        "--script-max-instructions",
        "30000000",
        "--arg",
        "name=Sky",
        "--arg",
        "count=4",
        "--",
        "cargo",
        "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Eval(config) = command else {
        panic!("expected eval command");
    };
    assert_eq!(config.script, current_dir.join("tmp/probe.luau"));
    assert_eq!(config.out_dir, current_dir.join("tmp/eval-output"));
    assert_eq!(config.max_instructions, Some(30_000_000));
    assert_eq!(
        config.args.get("name"),
        Some(&ScriptArgValue::String("Sky".to_string()))
    );
    assert_eq!(config.args.get("count"), Some(&ScriptArgValue::Int(4)));
    assert_eq!(config.launch.command[0], "cargo");
}

#[test]
fn parse_dump_command_accepts_fixture_viewport_json_out_and_timeout() {
    let args = os_args(&[
        "dump",
        "--fixture",
        "basic.default",
        "--param",
        "offset=180",
        "--param",
        "enabled=true",
        "--viewport",
        "secondary",
        "--script-timeout-secs",
        "9",
        "--json",
        "--out",
        "tmp/tree.json",
        "--",
        "cargo",
        "run",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Dump(config) = command else {
        panic!("expected dump command");
    };
    assert_eq!(config.fixture.as_deref(), Some("basic.default"));
    assert_eq!(config.params.get("offset"), Some(&ScriptArgValue::Int(180)));
    assert_eq!(
        config.params.get("enabled"),
        Some(&ScriptArgValue::Bool(true))
    );
    assert_eq!(config.viewport.as_deref(), Some("secondary"));
    assert!(!config.wait_for_initial_capture);
    assert!(config.json);
    assert_eq!(config.out, Some(current_dir.join("tmp/tree.json")));
    assert_eq!(config.timeout, Some(Duration::from_secs(9)));
    assert_eq!(config.launch.command[0], "cargo");
}

#[test]
fn parse_dump_without_fixture_waits_for_capture() {
    let args = os_args(&["dump", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Dump(config) = command else {
        panic!("expected dump command");
    };

    assert!(config.wait_for_initial_capture);
}

#[test]
fn parse_dump_rejects_param_without_fixture() {
    let args = os_args(&["dump", "--param", "offset=180", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
        .expect_err("dump param without fixture should fail");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };
    assert!(message.contains("--param"));
    assert!(message.contains("--fixture"));
}

#[test]
fn eval_command_uses_smoke_timeout_and_arg_defaults() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
script_timeout_secs = 7
script_max_instructions = 20000000
args = { name = \"File\", count = 4 }
",
    )
    .expect("write config");

    let args = os_args(&["eval", "tmp/probe.luau", "--arg", "name=Cli"]);
    let command = EdevCommand::parse_args_in_dir(&args, &repo_root).expect("parse command");
    let EdevCommand::Eval(config) = command else {
        panic!("expected eval command");
    };

    assert_eq!(config.timeout, Some(Duration::from_secs(7)));
    assert_eq!(config.max_instructions, Some(20_000_000));
    assert_eq!(
        config.args.get("name"),
        Some(&ScriptArgValue::String("Cli".to_string()))
    );
    assert_eq!(config.args.get("count"), Some(&ScriptArgValue::Int(4)));
    assert_eq!(config.launch.request_timeout, crate::APP_REQUEST_TIMEOUT);
}

#[test]
fn mcp_file_verbose_does_not_enable_smoke_or_eval_logs() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::write(
        repo_root.join(DEFAULT_CONFIG_FILE),
        "\
[app]
command = [\"cargo\", \"run\"]

[mcp]
verbose = true
",
    )
    .expect("write config");

    let smoke = EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse");
    let EdevCommand::Smoke(smoke) = smoke else {
        panic!("expected smoke command");
    };
    assert!(!smoke.verbose_output);
    assert!(!smoke.launch.expect("launch").verbose);

    let eval = EdevCommand::parse_args_in_dir(&os_args(&["eval", "tmp/probe.luau"]), &repo_root)
        .expect("parse");
    let EdevCommand::Eval(eval) = eval else {
        panic!("expected eval command");
    };
    assert!(!eval.launch.verbose);

    let mcp = EdevCommand::parse_args_in_dir(&os_args(&["mcp"]), &repo_root).expect("parse");
    let EdevCommand::Mcp(mcp) = mcp else {
        panic!("expected mcp command");
    };
    assert!(mcp.launch.verbose);

    let smoke_verbose =
        EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--verbose"]), &repo_root)
            .expect("parse");
    let EdevCommand::Smoke(smoke_verbose) = smoke_verbose else {
        panic!("expected smoke command");
    };
    assert!(smoke_verbose.verbose_output);
}

#[test]
fn eval_and_smoke_client_timeout_follows_long_script_timeout() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::write(
        repo_root.join(DEFAULT_CONFIG_FILE),
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
script_timeout_secs = 180
",
    )
    .expect("write config");

    let eval = EdevCommand::parse_args_in_dir(&os_args(&["eval", "tmp/probe.luau"]), &repo_root)
        .expect("parse");
    let EdevCommand::Eval(eval) = eval else {
        panic!("expected eval command");
    };
    assert_eq!(eval.timeout, Some(Duration::from_secs(180)));
    assert_eq!(eval.launch.request_timeout, Duration::from_secs(180));

    let smoke = EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse");
    let EdevCommand::Smoke(smoke) = smoke else {
        panic!("expected smoke command");
    };
    assert_eq!(
        smoke.launch.expect("launch").request_timeout,
        Duration::from_secs(180)
    );
}

#[test]
fn smoke_egui_diagnostic_policy_defaults_on_and_accepts_opt_out() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]
",
    )
    .expect("write default config");

    let defaulted =
        EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse");
    let EdevCommand::Smoke(defaulted) = defaulted else {
        panic!("expected smoke command");
    };
    assert!(defaulted.suite.fail_on_egui_diagnostics);

    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
fail_on_egui_diagnostics = false
",
    )
    .expect("write opt-out config");
    let opted_out =
        EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse");
    let EdevCommand::Smoke(opted_out) = opted_out else {
        panic!("expected smoke command");
    };
    assert!(!opted_out.suite.fail_on_egui_diagnostics);
}

#[test]
fn discover_config_stops_at_git_root() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    let nested = repo_root.join("nested").join("deeper");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::create_dir_all(&nested).expect("create nested dirs");
    fs::write(dir.path().join(DEFAULT_CONFIG_FILE), "ignored = true\n").expect("write config");

    let discovered = discover_config_path(&nested).expect("discover config");
    assert!(discovered.is_none());
}

#[test]
fn discover_config_finds_nearest_ancestor_inside_git_root() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    let nested = repo_root.join("nested").join("deeper");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::create_dir_all(&nested).expect("create nested dirs");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(&config_path, "[app]\ncommand = [\"cargo\", \"run\"]\n").expect("write config");

    let discovered = discover_config_path(&nested)
        .expect("discover config")
        .expect("config path");
    assert_eq!(discovered, config_path);
}

#[test]
fn file_config_resolves_relative_paths() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    fs::create_dir_all(repo_root.join("app")).expect("create app dir");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
cwd = \"app\"
command = [\"cargo\", \"run\"]

[smoke]
suite_dir = \"suite\"
",
    )
    .expect("write config");

    let args = os_args(&["smoke"]);
    let command = EdevCommand::parse_args_in_dir(&args, &repo_root).expect("parse");
    let EdevCommand::Smoke(config) = command else {
        panic!("expected smoke command");
    };
    assert_eq!(
        config.launch.as_ref().expect("launch").cwd,
        repo_root.join("app")
    );
    assert_eq!(config.suite.suite_dir, repo_root.join("suite"));
}

#[test]
fn file_config_rejects_missing_cwd() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
cwd = \"missing\"
command = [\"cargo\", \"run\"]
",
    )
    .expect("write config");

    let args = os_args(&["mcp"]);
    let error = EdevCommand::parse_args_in_dir(&args, &repo_root).expect_err("missing cwd");
    assert!(
        matches!(error, EdevError::InvalidArgs(ref message) if message.contains("cwd is not an existing directory")),
        "{error:?}"
    );
}

#[test]
fn smoke_bundle_uses_configured_or_default_dir_only_when_enabled() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
bundle_dir = \"configured-bundles\"
",
    )
    .expect("write config");

    let disabled = EdevCommand::parse_args_in_dir(&os_args(&["smoke"]), &repo_root).expect("parse");
    let EdevCommand::Smoke(config) = disabled else {
        panic!("expected smoke command");
    };
    assert_eq!(config.bundle_dir, None);

    let configured = EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--bundle"]), &repo_root)
        .expect("parse");
    let EdevCommand::Smoke(config) = configured else {
        panic!("expected smoke command");
    };
    assert_eq!(
        config.bundle_dir,
        Some(repo_root.join("configured-bundles"))
    );

    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]
",
    )
    .expect("write config");
    let defaulted = EdevCommand::parse_args_in_dir(&os_args(&["smoke", "--bundle"]), &repo_root)
        .expect("parse");
    let EdevCommand::Smoke(config) = defaulted else {
        panic!("expected smoke command");
    };
    assert_eq!(config.bundle_dir, Some(repo_root.join(DEFAULT_BUNDLE_DIR)));
}

#[test]
fn file_config_rejects_legacy_artifact_dir() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
artifact_dir = \"tmp/artifacts\"
",
    )
    .expect("write config");

    let args = os_args(&["smoke"]);
    let error = EdevCommand::parse_args_in_dir(&args, &repo_root).expect_err("parse");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };
    assert!(message.contains("smoke.artifact_dir is no longer supported"));
}

#[test]
fn file_config_rejects_legacy_filter() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
filter = \"10_*\"
",
    )
    .expect("write config");

    let args = os_args(&["smoke"]);
    let error = EdevCommand::parse_args_in_dir(&args, &repo_root).expect_err("parse");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };
    assert!(message.contains("smoke.filter is no longer supported"));
}

#[test]
fn file_config_rejects_unknown_keys() {
    let dir = tempdir();
    let repo_root = dir.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).expect("create git root");
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE);
    fs::write(
        &config_path,
        "\
[app]
command = [\"cargo\", \"run\"]

[smoke]
fail_fastt = true
",
    )
    .expect("write config");

    let args = os_args(&["smoke"]);
    let error = EdevCommand::parse_args_in_dir(&args, &repo_root).expect_err("parse");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args: {error:?}");
    };
    assert!(
        message.contains("fail_fastt") && message.contains("unknown field"),
        "{message}"
    );
}

#[test]
fn parse_fixtures_command_lists_fixtures() {
    let args = os_args(&["fixtures", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Fixture(config) = command else {
        panic!("expected fixture command");
    };
    assert!(config.name.is_none());
}

#[test]
fn parse_fixtures_command_accepts_json_and_markdown() {
    let current_dir = env::current_dir().unwrap();

    let command = EdevCommand::parse_args_in_dir(&os_args(&["fixtures", "--json"]), &current_dir)
        .expect("parse command");
    let EdevCommand::Fixture(config) = command else {
        panic!("expected fixture command");
    };
    assert!(config.json);
    assert!(!config.markdown);

    let command =
        EdevCommand::parse_args_in_dir(&os_args(&["fixtures", "--markdown"]), &current_dir)
            .expect("parse command");
    let EdevCommand::Fixture(config) = command else {
        panic!("expected fixture command");
    };
    assert!(config.markdown);
    assert!(!config.json);
}

#[test]
fn parse_fixture_command_accepts_name() {
    let args = os_args(&["fixture", "basic.default", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Fixture(config) = command else {
        panic!("expected fixture command");
    };
    assert_eq!(config.name.as_deref(), Some("basic.default"));
}

#[test]
fn parse_fixture_command_accepts_params_and_dump() {
    let args = os_args(&[
        "fixture",
        "basic.scrolled",
        "--param",
        "offset=180",
        "--param",
        "enabled=true",
        "--dump",
        "--no-wait",
    ]);
    let current_dir = env::current_dir().unwrap();
    let command = EdevCommand::parse_args_in_dir(&args, &current_dir).expect("parse command");
    let EdevCommand::Fixture(config) = command else {
        panic!("expected fixture command");
    };
    assert_eq!(config.name.as_deref(), Some("basic.scrolled"));
    assert_eq!(config.params.get("offset"), Some(&ScriptArgValue::Int(180)));
    assert_eq!(
        config.params.get("enabled"),
        Some(&ScriptArgValue::Bool(true))
    );
    assert!(config.dump);
    assert!(config.no_wait);
}

#[test]
fn parse_fixture_command_rejects_missing_name() {
    let args = os_args(&["fixture", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
        .expect_err("fixture without name should fail");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };
    assert!(message.contains("required arguments were not provided"));
}

#[test]
fn parse_fixtures_command_rejects_positional_arg() {
    let args = os_args(&["fixtures", "basic.default", "--", "cargo", "run"]);
    let current_dir = env::current_dir().unwrap();
    let error = EdevCommand::parse_args_in_dir(&args, &current_dir)
        .expect_err("fixtures with name should fail");
    let EdevError::InvalidArgs(message) = error else {
        panic!("expected invalid args");
    };
    assert!(message.contains("unexpected argument"));
}
