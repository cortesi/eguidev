#![cfg(target_os = "macos")]

//! Process-lifecycle acceptance tests for the macOS supervisor boundary.

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        fs,
        io::{self, Write},
        mem,
        os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        path::{Path, PathBuf},
        process::{self, ExitStatus, Stdio},
        ptr,
    };

    use libproc::processes::{self, ProcFilter};
    use serde_json::json;
    use tempfile::Builder;
    use tmcp::Client;
    use tokio::{
        io::{Interest, unix::AsyncFd},
        process::Command,
        time::{Duration, timeout},
    };

    const CONFIG_SECRET: &str = "watchdog-config-secret-not-in-ps";

    struct ProcessExitObserver {
        queue: AsyncFd<OwnedFd>,
    }

    impl ProcessExitObserver {
        fn new() -> io::Result<Self> {
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let queue = AsyncFd::with_interest(fd, Interest::READABLE)?;
            Ok(Self { queue })
        }

        fn watch_pid(&self, pid: i32) -> io::Result<bool> {
            if pid <= 0 {
                return Ok(false);
            }
            let event = libc::kevent {
                ident: usize::try_from(pid).unwrap_or_default(),
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: ptr::null_mut(),
            };
            let result = unsafe {
                libc::kevent(
                    self.queue.get_ref().as_raw_fd(),
                    &event,
                    1,
                    ptr::null_mut(),
                    0,
                    ptr::null(),
                )
            };
            if result == 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        }

        async fn wait_for_cleanup(
            &self,
            app_process_group_id: i32,
            supervisor_pid: i32,
        ) -> io::Result<()> {
            loop {
                let app_members = live_process_group_members(app_process_group_id);
                let supervisor_alive = process_alive(supervisor_pid);
                if app_members.is_empty() && !supervisor_alive {
                    return Ok(());
                }
                let mut watched_process_vanished = false;
                for pid in app_members {
                    watched_process_vanished |= !self.watch_pid(pid)?;
                }
                if supervisor_alive {
                    watched_process_vanished |= !self.watch_pid(supervisor_pid)?;
                }
                if watched_process_vanished {
                    continue;
                }
                self.next_event().await?;
            }
        }

        async fn next_event(&self) -> io::Result<()> {
            loop {
                let mut readiness = self.queue.readable().await?;
                match readiness.try_io(|inner| read_events(inner.get_ref().as_raw_fd())) {
                    Ok(result) => {
                        result?;
                        return Ok(());
                    }
                    Err(_would_block) => {}
                }
            }
        }
    }

    fn read_events(fd: RawFd) -> io::Result<()> {
        let mut events = [unsafe { mem::zeroed::<libc::kevent>() }; 32];
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let result = unsafe {
            libc::kevent(
                fd,
                ptr::null(),
                0,
                events.as_mut_ptr(),
                i32::try_from(events.len()).unwrap_or(i32::MAX),
                &timeout,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        Ok(())
    }

    fn live_process_group_members(process_group_id: i32) -> Vec<i32> {
        let Ok(pgrpid) = u32::try_from(process_group_id) else {
            return Vec::new();
        };
        processes::pids_by_type(ProcFilter::ByProgramGroup { pgrpid })
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pid| i32::try_from(pid).ok())
            .collect()
    }

    fn process_alive(pid: i32) -> bool {
        if pid <= 0 {
            return false;
        }
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    fn inherited_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        set_cloexec(reader.as_raw_fd(), true)?;
        set_cloexec(writer.as_raw_fd(), true)?;
        Ok((reader, writer))
    }

    fn set_cloexec(fd: RawFd, enabled: bool) -> io::Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = if enabled {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn test_tempdir() -> tempfile::TempDir {
        fs::create_dir_all("tmp").expect("create tmp");
        Builder::new()
            .prefix("edev-supervisor-test-")
            .tempdir_in("tmp")
            .expect("tempdir")
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root")
    }

    fn launcher_command(config_path: &Path, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_edev"));
        command.process_group(0);
        command.current_dir(cwd).args([
            "--config",
            config_path.to_str().expect("config path"),
            "mcp",
        ]);
        command
    }

    fn write_config<S: AsRef<str>>(path: &Path, cwd: &Path, command: &[S], with_secret: bool) {
        let cwd = serde_json::to_string(&cwd.to_string_lossy().to_string()).expect("cwd JSON");
        let command = command.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        let command = serde_json::to_string(&command).expect("command JSON");
        let env = if with_secret {
            format!("env = {{ EDEV_WATCHDOG_SECRET = {CONFIG_SECRET:?} }}\n")
        } else {
            String::new()
        };
        fs::write(
            path,
            format!("[app]\ncwd = {cwd}\ncommand = {command}\n{env}"),
        )
        .expect("write config");
    }

    fn write_demo_config(path: &Path, cwd: &Path, workspace: &Path, with_secret: bool) {
        let manifest_path = workspace.join("Cargo.toml");
        let manifest_path = manifest_path.to_string_lossy().into_owned();
        write_config(
            path,
            cwd,
            &[
                "cargo".to_string(),
                "run".to_string(),
                "--quiet".to_string(),
                "--manifest-path".to_string(),
                manifest_path,
                "-p".to_string(),
                "eguidev_demo".to_string(),
                "--features".to_string(),
                "devtools".to_string(),
                "--bin".to_string(),
                "eguidev_demo".to_string(),
                "--".to_string(),
                "--dev-mcp".to_string(),
            ],
            with_secret,
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn early_app_exit_closes_transport_and_removes_exact_record() -> Result<(), Box<dyn Error>>
    {
        let tempdir = test_tempdir();
        let config_path = tempdir.path().join("early-exit.toml");
        write_config(
            &config_path,
            tempdir.path(),
            &["/bin/sh", "-c", "exit 0"],
            false,
        );

        let mut client = Client::new("early-exit-test", env!("CARGO_PKG_VERSION"))
            .with_request_timeout(Duration::from_secs(10));
        let spawned = client
            .connect_process(launcher_command(&config_path, tempdir.path()))
            .await?;
        let process = spawned.process;
        let launcher_pid = process.id().ok_or("launcher PID unavailable")?;
        let launcher_record_path = tempdir
            .path()
            .join(".edev-instances")
            .join(format!("launcher-{launcher_pid}.json"));
        assert!(launcher_record_path.exists());

        let start = client.call_tool("start", json!({})).await?;
        assert!(
            start.is_error(),
            "early app exit should fail startup: {start:?}"
        );
        let status = client.call_tool("status", json!({})).await?;
        let status = status
            .structured_content
            .ok_or("status did not include structured content")?;
        assert_eq!(status["state"], "startup_failed");

        let registry_dir = tempdir.path().join(".edev-instances");
        let app_records = fs::read_dir(&registry_dir)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("app-"))
            .collect::<Vec<_>>();
        assert!(app_records.is_empty(), "early app exit left app records");

        let stop = client.call_tool("stop", json!({})).await?;
        assert!(!stop.is_error(), "normal stop should succeed: {stop:?}");
        assert!(
            launcher_record_path.exists(),
            "normal stop must leave the outer launcher record for unregister"
        );
        drop(client);
        let mut process = process;
        timeout(Duration::from_secs(10), process.wait()).await??;
        assert!(
            !launcher_record_path.exists(),
            "outer unregister should remove the launcher record"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn app_spawn_failure_leaves_only_the_live_launcher_record() -> Result<(), Box<dyn Error>>
    {
        let tempdir = test_tempdir();
        let config_path = tempdir.path().join("spawn-failure.toml");
        write_config(
            &config_path,
            tempdir.path(),
            &["/path/that/does/not/exist/eguidev-watchdog-test"],
            false,
        );

        let mut client = Client::new("spawn-failure-test", env!("CARGO_PKG_VERSION"))
            .with_request_timeout(Duration::from_secs(10));
        let spawned = client
            .connect_process(launcher_command(&config_path, tempdir.path()))
            .await?;
        let mut process = spawned.process;
        let launcher_pid = process.id().ok_or("launcher PID unavailable")?;
        let registry_dir = tempdir.path().join(".edev-instances");
        let launcher_record_path = registry_dir.join(format!("launcher-{launcher_pid}.json"));

        let start = client.call_tool("start", json!({})).await?;
        assert!(
            start.is_error(),
            "app spawn failure should fail start: {start:?}"
        );
        assert!(launcher_record_path.exists());
        assert!(
            fs::read_dir(&registry_dir)?
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with("app-")),
            "app spawn failure left an app record"
        );

        let stop = client.call_tool("stop", json!({})).await?;
        assert!(
            !stop.is_error(),
            "stop after failed start should succeed: {stop:?}"
        );
        assert!(launcher_record_path.exists());
        drop(client);
        timeout(Duration::from_secs(10), process.wait()).await??;
        assert!(!launcher_record_path.exists());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_start_stop_clears_app_but_preserves_launcher_until_unregister()
    -> Result<(), Box<dyn Error>> {
        let tempdir = test_tempdir();
        let workspace = workspace_root();
        let config_path = tempdir.path().join("normal-stop.toml");
        write_demo_config(&config_path, tempdir.path(), &workspace, false);

        let mut client = Client::new("normal-stop-watchdog-test", env!("CARGO_PKG_VERSION"))
            .with_request_timeout(Duration::from_secs(120));
        let spawned = client
            .connect_process(launcher_command(&config_path, tempdir.path()))
            .await?;
        let mut process = spawned.process;
        let launcher_pid = process.id().ok_or("launcher PID unavailable")?;
        let launcher_record_path = tempdir
            .path()
            .join(".edev-instances")
            .join(format!("launcher-{launcher_pid}.json"));
        assert!(launcher_record_path.exists());

        let start = client.call_tool("start", json!({})).await?;
        assert_eq!(
            start.structured_content.as_ref().map(|value| &value["ok"]),
            Some(&json!(true))
        );
        let status = client.call_tool("status", json!({})).await?;
        let status = status
            .structured_content
            .ok_or("status did not include structured content")?;
        let app_process_group_id = i32::try_from(
            status["process_group_id"]
                .as_i64()
                .ok_or("status did not report app process group")?,
        )?;
        let supervisor_pid = i32::try_from(
            status["supervisor_pid"]
                .as_u64()
                .ok_or("status did not report supervisor PID")?,
        )?;
        let record_path = PathBuf::from(
            status["registry_entry_path"]
                .as_str()
                .ok_or("status did not report app record path")?,
        );
        assert!(record_path.exists());
        assert!(
            !live_process_group_members(app_process_group_id).is_empty(),
            "managed app should be running before normal stop"
        );
        assert!(process_alive(supervisor_pid));

        let stop = client.call_tool("stop", json!({})).await?;
        assert!(!stop.is_error(), "normal stop should succeed: {stop:?}");
        let stopped = client.call_tool("status", json!({})).await?;
        let stopped = stopped
            .structured_content
            .ok_or("stopped status did not include structured content")?;
        assert_eq!(stopped["state"], "not_running");
        assert_eq!(stopped["app_present"], false);
        assert!(live_process_group_members(app_process_group_id).is_empty());
        assert!(!process_alive(supervisor_pid));
        assert!(
            !record_path.exists(),
            "normal stop should remove the exact managed-app record"
        );
        assert!(
            launcher_record_path.exists(),
            "normal stop must preserve the outer launcher record"
        );

        drop(client);
        timeout(Duration::from_secs(10), process.wait()).await??;
        assert!(
            !launcher_record_path.exists(),
            "outer unregister should remove the launcher record"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliberate_owner_shutdown_is_silent_success() -> Result<(), Box<dyn Error>> {
        let tempdir = test_tempdir();
        let registry_dir = tempdir.path().join(".edev-instances");
        fs::create_dir(&registry_dir)?;
        let record_path = registry_dir.join("app-normal-owner.json");
        let (owner_reader, owner_writer) = inherited_pipe()?;
        let (config_reader, config_writer) = inherited_pipe()?;
        let owner_fd = owner_reader.as_raw_fd();
        let config_fd = config_reader.as_raw_fd();
        let payload = serde_json::to_vec(&json!({
            "working_dir": tempdir.path(),
            "command": ["/bin/cat"],
            "env": {},
            "launcher_pid": process::id(),
            "launcher_token": "normal-owner-launcher",
            "launch_id": "normal-owner",
            "app_record_path": record_path,
            "ownership_fd": owner_fd,
        }))?;

        let mut command = Command::new(env!("CARGO_BIN_EXE_edev"));
        unsafe {
            command.pre_exec(move || {
                set_cloexec(owner_fd, false)?;
                set_cloexec(config_fd, false)
            });
        }
        command
            .args(["__edev_supervisor", "--config-fd", &config_fd.to_string()])
            .current_dir(tempdir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn()?;
        drop(owner_reader);
        drop(config_reader);

        let mut config_writer = fs::File::from(config_writer);
        config_writer.write_all(&payload)?;
        drop(config_writer);
        let mut owner_writer = fs::File::from(owner_writer);
        owner_writer.write_all(b"N")?;
        drop(owner_writer);

        let output = timeout(Duration::from_secs(10), child.wait_with_output()).await??;
        assert!(output.status.success(), "supervisor failed: {output:?}");
        assert!(
            output.stderr.is_empty(),
            "normal shutdown wrote stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!record_path.exists());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_death_recovers_group_and_restart_uses_fresh_identity()
    -> Result<(), Box<dyn Error>> {
        let tempdir = test_tempdir();
        let workspace = workspace_root();
        let config_path = tempdir.path().join("supervisor-recovery.toml");
        write_demo_config(&config_path, tempdir.path(), &workspace, false);

        let mut client = Client::new("supervisor-recovery-test", env!("CARGO_PKG_VERSION"))
            .with_request_timeout(Duration::from_secs(120));
        let spawned = client
            .connect_process(launcher_command(&config_path, tempdir.path()))
            .await?;
        let mut process = spawned.process;
        let launcher_pid = process.id().ok_or("launcher PID unavailable")?;
        let launcher_record_path = tempdir
            .path()
            .join(".edev-instances")
            .join(format!("launcher-{launcher_pid}.json"));

        let start = client.call_tool("start", json!({})).await?;
        assert!(!start.is_error(), "initial start should succeed: {start:?}");
        let status = client
            .call_tool("status", json!({}))
            .await?
            .structured_content
            .ok_or("status did not include structured content")?;
        let old_process_group_id = i32::try_from(
            status["process_group_id"]
                .as_i64()
                .ok_or("status did not report app process group")?,
        )?;
        let old_supervisor_pid = i32::try_from(
            status["supervisor_pid"]
                .as_u64()
                .ok_or("status did not report supervisor PID")?,
        )?;
        let old_record_path = PathBuf::from(
            status["registry_entry_path"]
                .as_str()
                .ok_or("status did not report app record path")?,
        );

        let observer = ProcessExitObserver::new()?;
        for pid in live_process_group_members(old_process_group_id) {
            let _exists = observer.watch_pid(pid)?;
        }
        let _exists = observer.watch_pid(old_supervisor_pid)?;
        assert_eq!(unsafe { libc::kill(old_supervisor_pid, libc::SIGKILL) }, 0);
        timeout(
            Duration::from_secs(30),
            observer.wait_for_cleanup(old_process_group_id, old_supervisor_pid),
        )
        .await??;

        let restart = client.call_tool("restart", json!({})).await?;
        assert!(!restart.is_error(), "restart should recover: {restart:?}");
        assert!(
            !old_record_path.exists(),
            "supervisor monitor should remove the old exact app record"
        );
        assert!(
            launcher_record_path.exists(),
            "supervisor death must not remove a live outer launcher record"
        );
        let restarted = client
            .call_tool("status", json!({}))
            .await?
            .structured_content
            .ok_or("restarted status did not include structured content")?;
        let new_record_path = PathBuf::from(
            restarted["registry_entry_path"]
                .as_str()
                .ok_or("restarted status did not report app record path")?,
        );
        assert_ne!(new_record_path, old_record_path);
        assert!(new_record_path.exists());
        assert_ne!(
            restarted["supervisor_pid"], status["supervisor_pid"],
            "restart should use a fresh supervisor"
        );

        let stop = client.call_tool("stop", json!({})).await?;
        assert!(
            !stop.is_error(),
            "stop after recovery should succeed: {stop:?}"
        );
        assert!(!new_record_path.exists());
        assert!(launcher_record_path.exists());
        drop(client);
        timeout(Duration::from_secs(10), process.wait()).await??;
        assert!(!launcher_record_path.exists());
        Ok(())
    }

    async fn abnormal_launcher_group_exit(
        signal: i32,
        assert_secret_free_args: bool,
    ) -> Result<(), Box<dyn Error>> {
        let tempdir = test_tempdir();
        let workspace = workspace_root();
        let config_path = tempdir.path().join("demo.toml");
        write_demo_config(&config_path, tempdir.path(), &workspace, true);

        let mut client = Client::new("hup-watchdog-test", env!("CARGO_PKG_VERSION"))
            .with_request_timeout(Duration::from_secs(120));
        let spawned = client
            .connect_process(launcher_command(&config_path, tempdir.path()))
            .await?;
        let mut process = spawned.process;
        let launcher_pid = i32::try_from(process.id().ok_or("launcher PID unavailable")?)?;

        let start = client.call_tool("start", json!({})).await?;
        assert_eq!(
            start.structured_content.as_ref().map(|value| &value["ok"]),
            Some(&json!(true))
        );
        let status = client.call_tool("status", json!({})).await?;
        let status = status
            .structured_content
            .ok_or("status did not include structured content")?;
        let app_process_group_id = status["process_group_id"]
            .as_i64()
            .ok_or("status did not report app process group")?;
        let app_process_group_id = i32::try_from(app_process_group_id)?;
        let supervisor_pid = i32::try_from(
            status["supervisor_pid"]
                .as_u64()
                .ok_or("status did not report supervisor PID")?,
        )?;
        let record_path = PathBuf::from(
            status["registry_entry_path"]
                .as_str()
                .ok_or("status did not report app record path")?,
        );
        assert!(
            record_path.exists(),
            "app record should exist while running"
        );
        let launcher_record_path = tempdir
            .path()
            .join(".edev-instances")
            .join(format!("launcher-{launcher_pid}.json"));
        assert!(
            launcher_record_path.exists(),
            "launcher record should exist while running"
        );

        let launcher_pgid = unsafe { libc::getpgid(launcher_pid) };
        let supervisor_pgid = unsafe { libc::getpgid(supervisor_pid) };
        assert!(launcher_pgid > 0, "launcher PGID unavailable");
        assert!(supervisor_pgid > 0, "supervisor PGID unavailable");
        assert_eq!(
            launcher_pgid, launcher_pid,
            "launcher should lead its own PGID"
        );
        assert_ne!(
            launcher_pgid, supervisor_pgid,
            "supervisor must not share the outer launcher's process group"
        );

        if assert_secret_free_args {
            let ps = Command::new("ps")
                .args(["-p", &supervisor_pid.to_string(), "-o", "command="])
                .output()
                .await?;
            assert!(ps.status.success());
            let supervisor_command = String::from_utf8_lossy(&ps.stdout);
            assert!(
                !supervisor_command.contains(CONFIG_SECRET),
                "supervisor argv exposed app env secret: {supervisor_command}"
            );
            assert!(
                supervisor_command.contains("--config-fd"),
                "supervisor should receive only a config fd: {supervisor_command}"
            );
        }

        let observer = ProcessExitObserver::new()?;
        for pid in live_process_group_members(app_process_group_id) {
            let _exists = observer.watch_pid(pid)?;
        }
        let _exists = observer.watch_pid(supervisor_pid)?;

        assert_eq!(unsafe { libc::killpg(launcher_pgid, signal) }, 0);
        drop(client);

        let cleanup = timeout(
            Duration::from_secs(30),
            observer.wait_for_cleanup(app_process_group_id, supervisor_pid),
        )
        .await;
        if cleanup.is_err() {
            let _kill_group_result = unsafe { libc::killpg(app_process_group_id, libc::SIGKILL) };
            let _kill_supervisor_result = unsafe { libc::kill(supervisor_pid, libc::SIGKILL) };
            let _wait_result = process.wait().await;
            return Err("supervisor did not finish abnormal cleanup".into());
        }
        cleanup??;
        let launcher_status: ExitStatus = process.wait().await?;
        assert!(
            !launcher_status.success(),
            "abnormal launcher termination should terminate the launcher"
        );
        assert!(
            !record_path.exists(),
            "exact app record should be removed after cleanup"
        );
        assert!(
            !launcher_record_path.exists(),
            "exact launcher record should be removed after abnormal cleanup"
        );
        assert!(live_process_group_members(app_process_group_id).is_empty());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hup_isolated_supervisor_cleans_app_group_and_secret_free_process_args()
    -> Result<(), Box<dyn Error>> {
        abnormal_launcher_group_exit(libc::SIGHUP, true).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sigkill_isolated_supervisor_cleans_app_and_launcher_records()
    -> Result<(), Box<dyn Error>> {
        abnormal_launcher_group_exit(libc::SIGKILL, false).await
    }
}
