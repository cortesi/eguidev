//! Tests of the macOS supervisor.

use std::{
    ffi::CString,
    fs::{self, File},
    io::{BufRead, BufReader},
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::PathBuf,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use eguidev::internal::presentation::Presentation;
use tempfile::Builder;
use tokio::runtime;

use super::*;

fn test_tempdir() -> tempfile::TempDir {
    fs::create_dir_all("tmp").expect("create tmp");
    Builder::new()
        .prefix("edev-lifecycle-test-")
        .tempdir_in("tmp")
        .expect("tempdir")
}

fn test_config(cwd: PathBuf) -> LaunchConfig {
    LaunchConfig {
        cwd,
        command: vec!["/usr/bin/true".to_string()],
        env: Default::default(),
        presentation: Presentation::Background,
        shutdown_grace: Duration::from_secs(30),
        verbose: false,
        request_timeout: crate::APP_REQUEST_TIMEOUT,
    }
}

fn is_cloexec(fd: RawFd) -> bool {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "read fd flags");
    flags & libc::FD_CLOEXEC != 0
}

#[test]
fn inherited_pipe_ends_remain_cloexec_in_the_parent() {
    let (reader, writer) = create_inherited_pipe().expect("pipe");
    assert!(is_cloexec(reader.as_raw_fd()));
    assert!(is_cloexec(writer.as_raw_fd()));
}

#[test]
fn concurrent_supervisor_launches_do_not_retain_private_pipe_endpoints() {
    const LAUNCHES: usize = 4;

    let tempdir = test_tempdir();
    let _registry =
        instance_registry::InstanceRegistry::register(&test_config(tempdir.path().into()))
            .expect("register launcher");
    let script = tempdir.path().join("config-reader.sh");
    fs::write(
        &script,
        concat!(
            "#!/bin/sh\nset -eu\nfd=\"$3\"\n",
            "payload=$(eval \"cat <&$fd\")\n",
            "owner_fd=$(printf '%s\\n' \"$payload\" | ",
            "sed -n 's/.*\"ownership_fd\":\\([0-9][0-9]*\\).*/\\1/p')\n",
            "test -n \"$owner_fd\"\n",
            "printf '%s\\n' \"$$\" > \"$PWD/ready\"\n",
            "eval \"cat <&$owner_fd\" >/dev/null\n",
        ),
    )
    .expect("write helper");
    let mut permissions = fs::metadata(&script)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make helper executable");

    let ready_path = tempdir.path().join("ready");
    let ready_c_path = CString::new(ready_path.as_os_str().as_encoded_bytes()).expect("ready path");
    assert_eq!(
        unsafe { libc::mkfifo(ready_c_path.as_ptr(), 0o600) },
        0,
        "create ready fifo"
    );
    let ready_fd = unsafe { libc::open(ready_c_path.as_ptr(), libc::O_RDWR) };
    assert!(ready_fd >= 0, "open ready fifo");
    let ready_file = unsafe { File::from_raw_fd(ready_fd) };

    let barrier = Arc::new(Barrier::new(LAUNCHES + 1));
    thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut shutdown_senders = Vec::new();
        let mut completion_receivers = Vec::new();
        for _ in 0..LAUNCHES {
            let barrier = Arc::clone(&barrier);
            let cwd = tempdir.path().to_path_buf();
            let executable = script.clone();
            let (shutdown_sender, shutdown_receiver) = mpsc::channel();
            let (completion_sender, completion_receiver) = mpsc::channel();
            shutdown_senders.push(shutdown_sender);
            completion_receivers.push(completion_receiver);
            handles.push(scope.spawn(move || {
                let runtime = runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                runtime.block_on(async move {
                    barrier.wait();
                    let config = test_config(cwd);
                    let mut process =
                        spawn_supervised_with_executable(&config, LogState::new(false), executable)
                            .await
                            .expect("spawn supervisor");
                    process.stdout.take();
                    process.stderr.take();
                    shutdown_receiver.recv().expect("shutdown signal");
                    shutdown_spawned(process, &LogState::new(false)).await;
                    completion_sender.send(()).expect("completion receiver");
                });
            }));
        }

        barrier.wait();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let ready_handle = scope.spawn(move || {
            let mut reader = BufReader::new(ready_file);
            let mut lines = Vec::new();
            let result = (|| {
                for _ in 0..LAUNCHES {
                    let mut line = String::new();
                    reader
                        .read_line(&mut line)
                        .map_err(|error| error.to_string())?;
                    if line.is_empty() {
                        return Err("ready fifo closed before all launches reported".to_string());
                    }
                    lines.push(line);
                }
                Ok::<_, String>(lines)
            })();
            ready_sender.send(result).expect("ready receiver");
        });
        let ready_result = ready_receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "concurrent launches retained a config reader".to_string())
            .expect("concurrent launches should read private config pipes")
            .expect("all supervisors should read their config");
        assert_eq!(ready_result.len(), LAUNCHES);

        for (shutdown_sender, completion_receiver) in
            shutdown_senders.into_iter().zip(completion_receivers)
        {
            shutdown_sender.send(()).expect("launch worker");
            completion_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("a sibling launch retained the ownership writer");
        }

        for handle in handles {
            handle.join().expect("launch worker");
        }
        ready_handle.join().expect("ready reader");
    });
}
