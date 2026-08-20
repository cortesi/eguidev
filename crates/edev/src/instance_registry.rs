//! Process-owned launcher and app metadata for edev.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "macos")]
use libproc::{bsd_info::BSDInfo, proc_pid::pidinfo};
use serde::{Deserialize, Serialize};

use super::{EdevError, LaunchConfig};

/// Directory used to store instance metadata entries per working directory.
pub const REGISTRY_DIR_NAME: &str = ".edev-instances";
/// Prefix used by one outer launcher record.
const LAUNCHER_PREFIX: &str = "launcher-";
/// Prefix used by one supervised app record.
const APP_PREFIX: &str = "app-";
/// Process-local sequence used to disambiguate same-timestamp launches.
static NEXT_LAUNCH_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize, Deserialize)]
/// Serialized metadata owned by one outer edev launcher.
struct LauncherMetadata {
    /// PID of the owning edev process.
    launcher_pid: u32,
    /// Collision-safe identity for this exact launcher lifetime.
    launcher_token: String,
    /// Canonical working directory associated with the launcher.
    working_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Serialized metadata owned by one supervisor for one app launch.
pub struct AppRecord {
    /// Collision-safe identity for this exact launch.
    pub launch_id: String,
    /// PID of the outer edev launcher.
    pub launcher_pid: u32,
    /// Collision-safe identity of the outer edev launcher.
    pub launcher_token: String,
    /// PID of the supervisor that owns this record.
    pub supervisor_pid: u32,
    /// Process group ID containing the managed app, never the supervisor.
    pub app_process_group_id: i32,
    /// Start time of the app process-group leader, used to reject reused PGIDs.
    #[serde(default)]
    pub app_group_leader_start_time: Option<ProcessStartTime>,
    /// Working directory used for the launch.
    pub working_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
/// Stable process-lifetime ready sourced from macOS process metadata.
pub struct ProcessStartTime {
    /// Whole seconds in the process start timestamp.
    seconds: u64,
    /// Microseconds within the process start timestamp.
    microseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Exact path and identity reserved for one future app record.
pub struct AppLaunch {
    /// Collision-safe launch identity.
    pub launch_id: String,
    /// Exact app record path.
    pub entry_path: PathBuf,
    /// Launcher PID that will be stored in the app record.
    pub launcher_pid: u32,
    /// Launcher identity that will be stored in the app record.
    pub launcher_token: String,
    /// Working directory that will be stored in the app record.
    pub working_dir: PathBuf,
}

impl AppLaunch {
    /// Build the record metadata after both child PIDs are known.
    pub fn record(
        &self,
        supervisor_pid: u32,
        app_process_group_id: i32,
        app_group_leader_start_time: Option<ProcessStartTime>,
    ) -> AppRecord {
        AppRecord {
            launch_id: self.launch_id.clone(),
            launcher_pid: self.launcher_pid,
            launcher_token: self.launcher_token.clone(),
            supervisor_pid,
            app_process_group_id,
            app_group_leader_start_time,
            working_dir: self.working_dir.clone(),
        }
    }
}

#[derive(Debug)]
/// Registry entry guard for one running edev launcher.
pub struct InstanceRegistry {
    /// Path to this launcher's metadata entry.
    entry_path: PathBuf,
    /// Exact metadata owned by this guard.
    metadata: LauncherMetadata,
}

impl InstanceRegistry {
    /// Register this edev process in the working-directory instance registry.
    pub fn register(config: &LaunchConfig) -> Result<Self, EdevError> {
        let working_dir = config.cwd.clone();
        let registry_dir = working_dir.join(REGISTRY_DIR_NAME);
        fs::create_dir_all(&registry_dir).map_err(EdevError::Io)?;
        cleanup_stale_instances(&registry_dir)?;

        let metadata = LauncherMetadata {
            launcher_pid: process::id(),
            launcher_token: next_launch_id(process::id()),
            working_dir,
        };
        let entry_path = launcher_record_path(&metadata.working_dir, metadata.launcher_pid);
        write_json(&entry_path, &metadata)?;
        Ok(Self {
            entry_path,
            metadata,
        })
    }

    /// Remove this launcher's entry from the registry.
    pub fn unregister(&self) -> Result<(), EdevError> {
        remove_launcher_metadata_if_matches(&self.entry_path, &self.metadata)
            .map(|_| ())
            .map_err(EdevError::Io)
    }
}

/// Allocate an app record identity for a launcher configuration.
pub fn app_launch_for(working_dir: &Path, launcher_pid: u32) -> Result<AppLaunch, io::Error> {
    let registry_dir = working_dir.join(REGISTRY_DIR_NAME);
    let launcher_path = launcher_record_path(working_dir, launcher_pid);
    let launcher = read_json::<LauncherMetadata>(&launcher_path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "launcher record is unavailable: {}",
                launcher_path.display()
            ),
        )
    })?;
    if launcher.launcher_pid != launcher_pid || launcher.working_dir != working_dir {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "launcher record identity mismatch: {}",
                launcher_path.display()
            ),
        ));
    }
    let launch_id = next_launch_id(launcher_pid);
    Ok(AppLaunch {
        entry_path: registry_dir.join(format!("{APP_PREFIX}{launch_id}.json")),
        launch_id,
        launcher_pid,
        launcher_token: launcher.launcher_token,
        working_dir: working_dir.to_path_buf(),
    })
}

/// Return the exact path used by one outer launcher record.
pub fn launcher_record_path(working_dir: &Path, launcher_pid: u32) -> PathBuf {
    working_dir
        .join(REGISTRY_DIR_NAME)
        .join(format!("{LAUNCHER_PREFIX}{launcher_pid}.json"))
}

impl Drop for InstanceRegistry {
    fn drop(&mut self) {
        let _remove_result = remove_launcher_metadata_if_matches(&self.entry_path, &self.metadata);
    }
}

/// Generate a process-local identity that also distinguishes rapid restarts.
fn next_launch_id(launcher_pid: u32) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_LAUNCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{launcher_pid}-{timestamp}-{sequence}")
}

/// Atomically publish one app record.
pub fn write_app_record(path: &Path, record: &AppRecord) -> Result<(), io::Error> {
    let payload = serde_json::to_vec(record)
        .map_err(|error| io::Error::other(format!("serialize app record: {error}")))?;
    write_atomic_payload(path, &payload)
}

/// Remove an app record only when it still describes the exact launch supplied by the caller.
pub fn remove_app_record_if_matches(path: &Path, expected: &AppRecord) -> Result<bool, io::Error> {
    let Some(actual) = read_app_record(path)? else {
        return Ok(false);
    };
    if actual != *expected {
        return Ok(false);
    }
    remove_file_if_exists(path)?;
    Ok(true)
}

/// Remove a launcher record only when it still describes the exact launcher.
pub fn remove_launcher_record_if_matches(
    path: &Path,
    expected: &AppRecord,
) -> Result<bool, io::Error> {
    let Some(actual) = read_json::<LauncherMetadata>(path)? else {
        return Ok(false);
    };
    if actual.launcher_pid != expected.launcher_pid
        || actual.launcher_token != expected.launcher_token
        || actual.working_dir != expected.working_dir
    {
        return Ok(false);
    }
    remove_file_if_exists(path)?;
    Ok(true)
}

/// Remove a launcher record only when its exact owner is no longer alive.
pub fn remove_launcher_record_if_dead(
    path: &Path,
    expected: &AppRecord,
) -> Result<bool, io::Error> {
    if is_process_alive(expected.launcher_pid) {
        return Ok(false);
    }
    remove_launcher_record_if_matches(path, expected)
}

/// Remove launcher metadata only when every identity field still matches.
fn remove_launcher_metadata_if_matches(
    path: &Path,
    expected: &LauncherMetadata,
) -> Result<bool, io::Error> {
    let Some(actual) = read_json::<LauncherMetadata>(path)? else {
        return Ok(false);
    };
    if actual.launcher_pid != expected.launcher_pid
        || actual.launcher_token != expected.launcher_token
        || actual.working_dir != expected.working_dir
    {
        return Ok(false);
    }
    remove_file_if_exists(path)?;
    Ok(true)
}

/// Remove stale entries from the instance registry.
fn cleanup_stale_instances(registry_dir: &Path) -> Result<(), EdevError> {
    let entries = fs::read_dir(registry_dir).map_err(EdevError::Io)?;
    for entry in entries {
        let entry = entry.map_err(EdevError::Io)?;
        let file_type = entry.file_type().map_err(EdevError::Io)?;
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if file_name.starts_with(LAUNCHER_PREFIX) {
            cleanup_stale_launcher(&path)?;
        } else if file_name.starts_with(APP_PREFIX) {
            cleanup_stale_app(&path)?;
        }
    }
    Ok(())
}

/// Remove a launcher record when its owning process is gone.
fn cleanup_stale_launcher(path: &Path) -> Result<(), EdevError> {
    let metadata = match read_json::<LauncherMetadata>(path) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return remove_file_if_exists(path).map_err(EdevError::Io);
        }
        Err(error) => return Err(EdevError::Io(error)),
    };
    if metadata.launcher_pid != process::id() && !is_process_alive(metadata.launcher_pid) {
        remove_file_if_exists(path).map_err(EdevError::Io)?;
    }
    Ok(())
}

/// Remove an app record only after both owners are gone.
fn cleanup_stale_app(path: &Path) -> Result<(), EdevError> {
    let metadata = match read_app_record(path) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return remove_file_if_exists(path).map_err(EdevError::Io);
        }
        Err(error) => return Err(EdevError::Io(error)),
    };
    if !is_process_alive(metadata.launcher_pid) && !is_process_alive(metadata.supervisor_pid) {
        if recorded_app_group_is_current(&metadata) {
            terminate_process_group(Some(metadata.app_process_group_id));
        }
        remove_file_if_exists(path).map_err(EdevError::Io)?;
    }
    Ok(())
}

/// Read one app record, tolerating a concurrent removal.
fn read_app_record(path: &Path) -> Result<Option<AppRecord>, io::Error> {
    read_json(path)
}

/// Read an exact app record for outer status and recovery handling.
pub fn read_app_record_for_path(path: &Path) -> Result<Option<AppRecord>, io::Error> {
    read_app_record(path)
}

/// Read one JSON record, tolerating a concurrent removal.
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, io::Error> {
    let payload = match fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&payload).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("decode instance metadata at {}: {error}", path.display()),
        )
    })
}

/// Persist one launcher record.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), EdevError> {
    let payload = serde_json::to_vec(value).map_err(|error| {
        EdevError::InstanceRegistry(format!(
            "failed to serialize instance metadata for {}: {error}",
            path.display()
        ))
    })?;
    write_atomic_payload(path, &payload).map_err(EdevError::Io)
}

/// Write bytes to a private temporary path before atomically publishing them.
fn write_atomic_payload(path: &Path, payload: &[u8]) -> Result<(), io::Error> {
    let tmp_path = path.with_extension(format!("{}.tmp", process::id()));
    fs::write(&tmp_path, payload)?;
    if let Err(error) = fs::rename(&tmp_path, path) {
        let _remove_result = remove_file_if_exists(&tmp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
/// Read the start-time identity of one live process.
pub fn process_start_time(pid: i32) -> Option<ProcessStartTime> {
    let info = pidinfo::<BSDInfo>(pid, 0).ok()?;
    Some(ProcessStartTime {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

#[cfg(not(target_os = "macos"))]
/// Process start-time identity is not used by non-macOS launchers.
pub fn process_start_time(_pid: i32) -> Option<ProcessStartTime> {
    None
}

/// Confirm that a recorded process group still belongs to the original app leader.
pub fn recorded_app_group_is_current(record: &AppRecord) -> bool {
    record
        .app_group_leader_start_time
        .zip(process_start_time(record.app_process_group_id))
        .is_some_and(|(recorded, current)| recorded == current)
}

/// Remove a file, tolerating not-found races.
fn remove_file_if_exists(path: &Path) -> Result<(), io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
/// Return true when a process with the provided pid appears alive.
fn is_process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // The launcher is a same-user process, so BSD info remains available throughout its life.
    pidinfo::<BSDInfo>(pid, 0).is_ok_and(|info| info.pbi_status != libc::SZOMB)
}

#[cfg(target_os = "macos")]
/// Return whether `pid` is alive under the production liveness definition.
pub fn process_is_alive(pid: i32) -> bool {
    u32::try_from(pid).is_ok_and(is_process_alive)
}

#[cfg(all(unix, not(target_os = "macos")))]
/// Return true when a process with the provided pid appears alive.
fn is_process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    !matches!(error.raw_os_error(), Some(libc::ESRCH))
}

#[cfg(not(unix))]
/// Process liveness checks are conservative on non-unix platforms.
fn is_process_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
/// Terminate a process group, ignoring missing-group races.
fn terminate_process_group(process_group_id: Option<i32>) {
    if let Some(pgid) = process_group_id {
        let _result = super::process_lifecycle::kill_process_group(pgid);
    }
}

#[cfg(not(unix))]
/// No-op process group termination on non-unix platforms.
fn terminate_process_group(_process_group_id: Option<i32>) {}

#[cfg(test)]
mod tests {
    use std::{fs, process, time::Duration};

    use eguidev::internal::presentation::Presentation;
    use tempfile::TempDir;

    use super::*;

    fn test_tempdir() -> TempDir {
        fs::create_dir_all("tmp").expect("create tmp");
        tempfile::Builder::new()
            .prefix("edev-registry-test-")
            .tempdir_in("tmp")
            .expect("tempdir")
    }

    fn test_config(cwd: PathBuf) -> LaunchConfig {
        LaunchConfig {
            cwd,
            command: vec![
                "cargo".to_string(),
                "run".to_string(),
                "--dev-mcp".to_string(),
            ],
            env: Default::default(),
            presentation: Presentation::Background,
            shutdown_grace: Duration::from_secs(30),
            verbose: false,
            request_timeout: crate::APP_REQUEST_TIMEOUT,
        }
    }

    #[test]
    fn register_and_unregister_lifecycle() {
        let tempdir = test_tempdir();
        let config = test_config(tempdir.path().to_path_buf());
        let registry = InstanceRegistry::register(&config).expect("register");

        let entry_path = tempdir
            .path()
            .join(REGISTRY_DIR_NAME)
            .join(format!("{LAUNCHER_PREFIX}{}.json", process::id()));
        assert!(entry_path.exists());

        registry.unregister().expect("unregister");
        assert!(!entry_path.exists());
    }

    #[test]
    fn app_records_have_unique_paths_and_exact_deletion() {
        let tempdir = test_tempdir();
        let config = test_config(tempdir.path().to_path_buf());
        let _registry = InstanceRegistry::register(&config).expect("register");
        let first = app_launch_for(tempdir.path(), process::id()).expect("first app launch");
        let second = app_launch_for(tempdir.path(), process::id()).expect("second app launch");
        assert_ne!(first.launch_id, second.launch_id);
        assert_ne!(first.entry_path, second.entry_path);

        let record = first.record(41, i32::MAX, None);
        write_app_record(&first.entry_path, &record).expect("write app record");
        let newer = second.record(41, i32::MAX, None);
        assert!(!remove_app_record_if_matches(&first.entry_path, &newer).expect("compare"));
        assert!(first.entry_path.exists());
        assert!(remove_app_record_if_matches(&first.entry_path, &record).expect("remove"));
        assert!(!first.entry_path.exists());
    }

    #[test]
    fn stale_app_cleanup_requires_both_launcher_and_supervisor_to_be_dead() {
        let tempdir = test_tempdir();
        let config = test_config(tempdir.path().to_path_buf());
        let _registry = InstanceRegistry::register(&config).expect("register");
        let launch = app_launch_for(tempdir.path(), process::id()).expect("app launch");
        let record = AppRecord {
            launch_id: launch.launch_id.clone(),
            launcher_pid: u32::MAX,
            launcher_token: launch.launcher_token.clone(),
            supervisor_pid: u32::MAX,
            app_process_group_id: i32::MAX,
            app_group_leader_start_time: None,
            working_dir: launch.working_dir.clone(),
        };
        write_app_record(&launch.entry_path, &record).expect("write app record");

        cleanup_stale_app(&launch.entry_path).expect("cleanup");
        assert!(!launch.entry_path.exists());

        let live_supervisor = launch.record(process::id(), i32::MAX, None);
        write_app_record(&launch.entry_path, &live_supervisor).expect("write app record");
        cleanup_stale_app(&launch.entry_path).expect("cleanup");
        assert!(launch.entry_path.exists());
    }

    #[test]
    fn old_app_record_cannot_remove_reused_launcher_identity() {
        let tempdir = test_tempdir();
        let config = test_config(tempdir.path().to_path_buf());
        let old_registry = InstanceRegistry::register(&config).expect("register old launcher");
        let old_launch = app_launch_for(tempdir.path(), process::id()).expect("old app launch");
        let old_record = old_launch.record(41, i32::MAX, None);
        let launcher_path = launcher_record_path(tempdir.path(), process::id());
        let newer = LauncherMetadata {
            launcher_pid: process::id(),
            launcher_token: "newer-launcher-token".to_string(),
            working_dir: tempdir.path().to_path_buf(),
        };
        write_json(&launcher_path, &newer).expect("replace launcher identity");

        assert!(
            !remove_launcher_record_if_matches(&launcher_path, &old_record)
                .expect("compare launcher identity")
        );
        old_registry
            .unregister()
            .expect("old guard unregister should be harmless");
        assert_eq!(
            read_json::<LauncherMetadata>(&launcher_path)
                .expect("read launcher")
                .expect("newer launcher remains")
                .launcher_token,
            newer.launcher_token
        );
    }

    #[test]
    fn malformed_app_record_is_removed_during_stale_cleanup() {
        let tempdir = test_tempdir();
        let path = tempdir.path().join("app-torn.json");
        fs::write(&path, br#"{"launch_id":"incomplete"#).expect("write malformed record");

        cleanup_stale_app(&path).expect("cleanup malformed record");

        assert!(!path.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recorded_group_requires_exact_leader_start_time() {
        let start_time = process_start_time(i32::try_from(process::id()).expect("current pid"))
            .expect("current process start time");
        let record = AppRecord {
            launch_id: "ready-test".to_string(),
            launcher_pid: process::id(),
            launcher_token: "launcher".to_string(),
            supervisor_pid: process::id(),
            app_process_group_id: i32::try_from(process::id()).expect("current pid"),
            app_group_leader_start_time: Some(start_time),
            working_dir: PathBuf::from("/"),
        };
        assert!(recorded_app_group_is_current(&record));

        let unavailable = AppRecord {
            app_group_leader_start_time: None,
            ..record.clone()
        };
        assert!(!recorded_app_group_is_current(&unavailable));

        let mismatched = AppRecord {
            app_group_leader_start_time: Some(ProcessStartTime {
                seconds: start_time.seconds.saturating_add(1),
                microseconds: start_time.microseconds,
            }),
            ..record
        };
        assert!(!recorded_app_group_is_current(&mismatched));
    }
}
