//! Resolution of the command line and the project file into the configuration
//! of a command.

use super::*;

pub(super) fn resolve_fixture_config(
    cli: FixtureCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<FixtureConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?.with_script_timeout(
        loaded.and_then(|config| {
            config
                .file
                .smoke
                .script_timeout_secs
                .map(Duration::from_secs)
        }),
    );
    Ok(FixtureConfig {
        launch,
        name: cli.name,
        params: cli.params,
        json: cli.json,
        markdown: cli.markdown,
        dump: cli.dump,
        no_wait: cli.no_wait,
    })
}

pub(super) fn resolve_dump_config(
    cli: DumpCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<DumpConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    let file_smoke = loaded.map(|config| &config.file.smoke);
    if cli.fixture.is_none() && !cli.params.is_empty() {
        return Err(EdevError::InvalidArgs(
            "`edev dump --param` requires `--fixture`".to_string(),
        ));
    }
    let wait_for_initial_capture = cli.fixture.is_none();
    let timeout = Some(Duration::from_secs(
        cli.script_timeout_secs
            .or_else(|| file_smoke.and_then(|smoke| smoke.script_timeout_secs))
            .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS),
    ));
    Ok(DumpConfig {
        launch: launch.with_script_timeout(timeout),
        fixture: cli.fixture,
        params: cli.params,
        viewport: cli.viewport,
        wait_for_initial_capture,
        json: cli.json,
        out: cli
            .out
            .as_ref()
            .map(|path| absolutize_path(path, current_dir)),
        timeout,
    })
}

pub(super) fn resolve_eval_config(
    cli: EvalCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<EvalConfig, EdevError> {
    let launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    let file_smoke = loaded.map(|config| &config.file.smoke);
    let script = absolutize_path(&cli.script, current_dir);
    let out_dir = cli
        .out_dir
        .as_ref()
        .map(|path| absolutize_path(path, current_dir))
        .unwrap_or_else(|| {
            script
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| current_dir.to_path_buf())
        });
    let mut args = file_smoke
        .map(|smoke| smoke.args.clone())
        .unwrap_or_default();
    args.extend(cli.args);
    let timeout = Some(Duration::from_secs(
        cli.script_timeout_secs
            .or_else(|| file_smoke.and_then(|smoke| smoke.script_timeout_secs))
            .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS),
    ));
    Ok(EvalConfig {
        launch: launch.with_script_timeout(timeout),
        script,
        out_dir,
        timeout,
        max_instructions: cli
            .script_max_instructions
            .or_else(|| file_smoke.and_then(|smoke| smoke.script_max_instructions)),
        args,
        module_dir: resolve_optional_path(
            cli.module_dir.as_ref(),
            file_smoke.and_then(|smoke| smoke.module_dir.as_ref()),
            current_dir,
            loaded
                .and_then(|config| config.path.parent())
                .unwrap_or(current_dir),
        ),
    })
}

pub(super) fn resolve_mcp_config(
    cli: &McpCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<McpConfig, EdevError> {
    let mut launch = resolve_launch_config(&cli.common, loaded, current_dir)?;
    if cli.common.verbose.is_none()
        && let Some(verbose) = loaded.and_then(|config| config.file.mcp.verbose)
    {
        launch.verbose = verbose;
    }
    let idle_shutdown_after = Duration::from_secs(
        cli.idle_shutdown_after_secs
            .or_else(|| loaded.and_then(|config| config.file.mcp.idle_shutdown_after_secs))
            .unwrap_or(DEFAULT_IDLE_SHUTDOWN_AFTER_SECS),
    );
    Ok(McpConfig {
        launch,
        idle_shutdown_after,
    })
}

pub(super) fn resolve_smoke_config(
    cli: SmokeCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<SmokeConfig, EdevError> {
    let suite_base_dir = loaded
        .and_then(|config| config.path.parent())
        .unwrap_or(current_dir);
    let file_smoke = loaded.map(|config| &config.file.smoke);
    if let Some(path) = file_smoke.and_then(|smoke| smoke.legacy_artifact_dir.as_ref()) {
        return Err(EdevError::InvalidArgs(format!(
            "smoke.artifact_dir is no longer supported (found {}); use --verbose for inline failure diagnostics",
            path.display()
        )));
    }
    if file_smoke
        .and_then(|smoke| smoke.legacy_filter.as_ref())
        .is_some()
    {
        return Err(EdevError::InvalidArgs(
            "smoke.filter is no longer supported; pass explicit script paths to `edev smoke`"
                .to_string(),
        ));
    }
    if !cli.scripts.is_empty() && !cli.only.is_empty() {
        return Err(EdevError::InvalidArgs(
            "`--only` cannot be combined with explicit smoke script paths".to_string(),
        ));
    }
    let suite_dir = resolve_path(
        cli.suite_dir.as_ref(),
        file_smoke.and_then(|smoke| smoke.suite_dir.as_ref()),
        current_dir,
        suite_base_dir,
        Path::new(DEFAULT_SUITE_DIR),
    )?;
    let mut args = file_smoke
        .map(|smoke| smoke.args.clone())
        .unwrap_or_default();
    args.extend(cli.args);
    let bundle_dir = if cli.bundle || cli.bundle_dir.is_some() {
        Some(resolve_path(
            cli.bundle_dir.as_ref(),
            file_smoke.and_then(|smoke| smoke.bundle_dir.as_ref()),
            current_dir,
            suite_base_dir,
            Path::new(DEFAULT_BUNDLE_DIR),
        )?)
    } else {
        None
    };
    let module_dir = resolve_optional_path(
        cli.module_dir.as_ref(),
        file_smoke.and_then(|smoke| smoke.module_dir.as_ref()),
        current_dir,
        suite_base_dir,
    );
    let suite = SuiteConfig {
        suite_dir,
        scripts: cli.scripts,
        only: cli.only,
        suite_timeout: Duration::from_secs(
            cli.suite_timeout_secs
                .or_else(|| file_smoke.and_then(|smoke| smoke.suite_timeout_secs))
                .unwrap_or(DEFAULT_SUITE_TIMEOUT_SECS),
        ),
        script_timeout: Some(Duration::from_secs(
            cli.script_timeout_secs
                .or_else(|| file_smoke.and_then(|smoke| smoke.script_timeout_secs))
                .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS),
        )),
        max_instructions: cli
            .script_max_instructions
            .or_else(|| file_smoke.and_then(|smoke| smoke.script_max_instructions)),
        fail_fast: cli
            .fail_fast
            .or_else(|| file_smoke.and_then(|smoke| smoke.fail_fast))
            .unwrap_or(false),
        fail_on_egui_diagnostics: file_smoke
            .and_then(|smoke| smoke.fail_on_egui_diagnostics)
            .unwrap_or(true),
        run_mode: cli
            .until_fail
            .map(SuiteRunMode::UntilFail)
            .unwrap_or_else(|| SuiteRunMode::Repeat(cli.repeat.unwrap_or(1))),
        args,
    };
    let launch = if cli.list {
        None
    } else {
        Some(
            resolve_launch_config(&cli.common, loaded, current_dir)?
                .with_script_timeout(suite.script_timeout),
        )
    };
    let verbose_output = launch.as_ref().is_some_and(|launch| launch.verbose);
    Ok(SmokeConfig {
        verbose_output,
        list: cli.list,
        list_json: cli.list_json,
        launch,
        suite,
        bundle_dir,
        module_dir,
    })
}

pub(super) fn resolve_record_config(
    cli: RecordCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<RecordConfig, EdevError> {
    let outfile = absolutize_path(&cli.outfile, current_dir);
    validate_record_outfile(&outfile)?;
    if cli.smoke.list {
        return Err(EdevError::InvalidArgs(
            "`edev record` cannot be combined with `--list` because recording requires launching an app window"
                .to_string(),
        ));
    }
    let smoke = resolve_smoke_config(cli.smoke, loaded, current_dir)?;
    Ok(RecordConfig {
        outfile,
        window_title: cli.window_title,
        smoke,
    })
}

pub(super) fn validate_record_outfile(path: &Path) -> Result<(), EdevError> {
    let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    if extension.eq_ignore_ascii_case("mov") {
        return Ok(());
    }
    Err(EdevError::InvalidArgs(format!(
        "`edev record OUTFILE` requires a .mov output path; got {}",
        path.display()
    )))
}

pub(super) fn resolve_launch_config(
    cli: &CommonCliOptions,
    loaded: Option<&LoadedConfig>,
    current_dir: &Path,
) -> Result<LaunchConfig, EdevError> {
    let file_base_dir = loaded
        .and_then(|config| config.path.parent())
        .unwrap_or(current_dir);
    let file_app = loaded.map(|config| &config.file.app);
    let cwd = resolve_path(
        cli.cwd.as_ref(),
        file_app.and_then(|app| app.cwd.as_ref()),
        current_dir,
        file_base_dir,
        current_dir,
    )?;
    if !cwd.is_dir() {
        return Err(EdevError::InvalidArgs(format!(
            "cwd is not an existing directory: {}",
            cwd.display()
        )));
    }
    let command = cli
        .command
        .clone()
        .or_else(|| file_app.and_then(|app| app.command.clone()))
        .ok_or_else(|| {
            EdevError::InvalidArgs(
                "no app command configured; add app.command to .edev.toml or pass one after `--`"
                    .to_string(),
            )
        })?;
    if command.is_empty() {
        return Err(EdevError::InvalidArgs(
            "app command must not be empty".to_string(),
        ));
    }
    let presentation = cli
        .presentation
        .or_else(|| file_app.and_then(|app| app.presentation))
        .unwrap_or(presentation::Presentation::Background);
    let shutdown_grace = Duration::from_secs(
        file_app
            .and_then(|app| app.shutdown_grace_secs)
            .unwrap_or(DEFAULT_SHUTDOWN_GRACE_SECS),
    );
    Ok(LaunchConfig {
        cwd,
        command,
        env: file_app.map(|app| app.env.clone()).unwrap_or_default(),
        presentation,
        shutdown_grace,
        verbose: cli.verbose.unwrap_or(false),
        request_timeout: crate::APP_REQUEST_TIMEOUT,
    })
}

pub(super) fn resolve_path(
    cli: Option<&PathBuf>,
    file: Option<&PathBuf>,
    current_dir: &Path,
    file_base_dir: &Path,
    default: &Path,
) -> Result<PathBuf, EdevError> {
    let path = if let Some(path) = cli {
        absolutize_path(path, current_dir)
    } else if let Some(path) = file {
        absolutize_path(path, file_base_dir)
    } else {
        absolutize_path(default, current_dir)
    };
    Ok(path.canonicalize().unwrap_or(path))
}

pub(super) fn resolve_optional_path(
    cli: Option<&PathBuf>,
    file: Option<&PathBuf>,
    current_dir: &Path,
    file_base_dir: &Path,
) -> Option<PathBuf> {
    let path = cli
        .map(|path| absolutize_path(path, current_dir))
        .or_else(|| file.map(|path| absolutize_path(path, file_base_dir)))?;
    Some(path.canonicalize().unwrap_or(path))
}

pub(super) fn absolutize_path(path: &Path, base_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

/// Parse a `KEY=VALUE` flag. The error is a bare message: clap wraps it, and
/// `EdevError::InvalidArgs` then adds its prefix once.
pub(super) fn parse_typed_key_value_flag(
    raw: &str,
    flag_name: &'static str,
) -> Result<(String, ScriptArgValue), String> {
    let Some((key, raw_value)) = raw.split_once('=') else {
        return Err(format!("{flag_name} requires KEY=VALUE"));
    };
    if key.is_empty() {
        return Err(format!("{flag_name} requires a non-empty key"));
    }
    Ok((key.to_string(), parse_script_arg_value(raw_value)))
}

pub(super) fn parse_script_arg_value(raw: &str) -> ScriptArgValue {
    match raw {
        "true" => ScriptArgValue::Bool(true),
        "false" => ScriptArgValue::Bool(false),
        _ => match raw.parse::<i64>() {
            Ok(value) => ScriptArgValue::Int(value),
            Err(_) => match raw.parse::<f64>() {
                Ok(value) => ScriptArgValue::Float(value),
                Err(_) => ScriptArgValue::String(raw.to_string()),
            },
        },
    }
}
