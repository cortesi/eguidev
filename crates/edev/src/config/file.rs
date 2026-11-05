//! The project configuration file of edev and its discovery.

use super::*;

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct FileConfig {
    #[serde(default)]
    pub(super) app: FileAppConfig,
    #[serde(default)]
    pub(super) smoke: FileSmokeConfig,
    #[serde(default)]
    pub(super) mcp: FileMcpConfig,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct FileAppConfig {
    pub(super) cwd: Option<PathBuf>,
    pub(super) command: Option<Vec<String>>,
    pub(super) presentation: Option<presentation::Presentation>,
    pub(super) shutdown_grace_secs: Option<u64>,
    #[serde(default)]
    pub(super) env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct FileSmokeConfig {
    pub(super) suite_dir: Option<PathBuf>,
    #[serde(rename = "filter")]
    pub(super) legacy_filter: Option<String>,
    pub(super) suite_timeout_secs: Option<u64>,
    pub(super) script_timeout_secs: Option<u64>,
    pub(super) script_max_instructions: Option<u64>,
    pub(super) fail_fast: Option<bool>,
    pub(super) fail_on_egui_diagnostics: Option<bool>,
    #[serde(rename = "artifact_dir")]
    pub(super) legacy_artifact_dir: Option<PathBuf>,
    pub(super) bundle_dir: Option<PathBuf>,
    pub(super) module_dir: Option<PathBuf>,
    #[serde(default)]
    pub(super) args: ScriptArgs,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct FileMcpConfig {
    pub(super) verbose: Option<bool>,
    pub(super) idle_shutdown_after_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct LoadedConfig {
    pub(super) path: PathBuf,
    pub(super) file: FileConfig,
}

pub(super) fn load_project_config(
    explicit_config: Option<PathBuf>,
    current_dir: &Path,
) -> Result<Option<LoadedConfig>, EdevError> {
    let Some(path) = resolve_config_path(explicit_config, current_dir)? else {
        return Ok(None);
    };
    let payload = fs::read_to_string(&path)?;
    let file = toml::from_str::<FileConfig>(&payload).map_err(|error| {
        EdevError::InvalidArgs(format!("failed to parse {}: {error}", path.display()))
    })?;
    Ok(Some(LoadedConfig { path, file }))
}

pub(super) fn resolve_config_path(
    explicit_config: Option<PathBuf>,
    current_dir: &Path,
) -> Result<Option<PathBuf>, EdevError> {
    if let Some(path) = explicit_config {
        let path = absolutize_path(&path, current_dir);
        if !path.is_file() {
            return Err(EdevError::InvalidArgs(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        return Ok(Some(path));
    }
    discover_config_path(current_dir)
}

pub(super) fn discover_config_path(current_dir: &Path) -> Result<Option<PathBuf>, EdevError> {
    let mut dir = current_dir.canonicalize()?;
    loop {
        let candidate = dir.join(DEFAULT_CONFIG_FILE);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
        let stop_here = dir.join(".git").exists();
        let Some(parent) = dir.parent() else {
            return Ok(None);
        };
        if stop_here {
            return Ok(None);
        }
        dir = parent.to_path_buf();
    }
}
