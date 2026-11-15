//! Tests of the lifecycle of the managed app.

use super::*;

#[tokio::test]
async fn restart_updates_state_on_success() {
    let (app, _handle) = make_mock_app().await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);

    let status = state
        .restart_with(|_, _| Box::pin(async move { Ok(app) }))
        .await
        .expect("restart");

    assert!(matches!(status, LifecycleStartStatus::Running));
    assert!(matches!(state.status, AppStatus::Running));
    assert!(state.app.is_some());
}

#[tokio::test]
async fn status_reports_starting_during_stalled_spawn() {
    let tempdir = test_tempdir();
    let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let start_state = Arc::clone(&state);
    let start_task = tokio::spawn(async move {
        start_app_unlocked_with(&start_state, |_, _| async move {
            entered_tx
                .send(())
                .expect("start observer should be waiting");
            release_rx.await.expect("test should release stalled spawn");
            Err(AppStartError::Other("stalled spawn".to_string()))
        })
        .await
    });

    entered_rx.await.expect("spawn started");
    let report = timeout(Duration::from_secs(1), async {
        state.lock().await.status_report()
    })
    .await
    .expect("status should not wait on spawn");
    assert_eq!(report.state, "starting");
    release_tx
        .send(())
        .expect("stalled spawn should be waiting");
    let status = start_task.await.expect("start task");
    assert!(matches!(status, Err(EdevError::AppStart(_))));
    assert_eq!(state.lock().await.status_report().state, "not_running");
}

#[tokio::test]
async fn start_is_idempotent_when_running() {
    let (app, _handle) = make_mock_app().await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);
    state.status = AppStatus::Running;
    state.app = Some(app);

    let status = state.start().await.expect("start");
    assert!(matches!(status, StartStatus::AlreadyRunning));
}

#[tokio::test]
async fn restart_reports_startup_failure() {
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);

    let status = state
        .restart_with(|_, _| {
            Box::pin(async { Err(AppStartError::StartupFailed("startup output".to_string())) })
        })
        .await
        .expect("restart");

    assert!(matches!(
        status,
        LifecycleStartStatus::StartupFailed(ref output) if output == "startup output"
    ));
    assert!(matches!(
        state.status,
        AppStatus::StartupFailed { ref output } if output == "startup output"
    ));
}

#[tokio::test]
async fn restart_sets_not_running_on_spawn_error() {
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);

    let error = state
        .restart_with(|_, _| {
            Box::pin(async { Err(AppStartError::Other("spawn failed".to_string())) })
        })
        .await
        .expect_err("restart should fail");

    assert!(matches!(error, EdevError::AppStart(_)));
    assert!(matches!(state.status, AppStatus::NotRunning));
    assert!(state.app.is_none());
}

#[tokio::test]
async fn shutdown_clears_running_app() {
    let (app, _handle) = make_mock_app().await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);
    state.status = AppStatus::Running;
    state.app = Some(app);

    state.shutdown().await.expect("shutdown");

    assert!(matches!(state.status, AppStatus::NotRunning));
    assert!(state.app.is_none());
}

#[tokio::test]
async fn restart_reports_startup_failure_when_readiness_probe_fails() {
    let (app, _handle) = make_failing_app().await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);

    let status = state
        .restart_with(|_, _| Box::pin(async move { Ok(app) }))
        .await
        .expect("restart");

    assert!(matches!(status, LifecycleStartStatus::StartupFailed(_)));
    assert!(matches!(state.status, AppStatus::StartupFailed { .. }));
    assert!(state.app.is_none());
}

#[tokio::test]
async fn readiness_probe_gets_the_request_timeout() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (app, _handle) = make_recording_eval_app(Arc::clone(&requests)).await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);

    let status = state
        .restart_with(|_, _| Box::pin(async move { Ok(app) }))
        .await
        .expect("restart");

    assert!(matches!(status, LifecycleStartStatus::Running));
    let requests = requests.lock().expect("requests");
    assert_eq!(
        requests.first().and_then(|request| request.timeout_ms),
        Some(APP_REQUEST_TIMEOUT.as_millis() as u64)
    );
}

#[tokio::test]
async fn restart_reports_a_readiness_probe_timeout_as_a_timeout() {
    let (app, _handle) = make_probe_timeout_app().await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);

    let result = state
        .restart_with(|_, _| Box::pin(async move { Ok(app) }))
        .await;

    assert!(
        matches!(result, Err(EdevError::StartTimeout(ref message)) if message.contains("timed out")),
        "{result:?}"
    );
    assert!(matches!(state.status, AppStatus::NotRunning));
    assert!(state.app.is_none());
}

#[tokio::test]
async fn start_reports_restart_required_after_prior_startup_failure() {
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);
    state.status = AppStatus::StartupFailed {
        output: "boom".to_string(),
    };

    let status = state.start().await.expect("start");
    assert!(matches!(status, StartStatus::RestartRequired(ref output) if output == "boom"));
}

#[tokio::test]
async fn stop_is_idempotent() {
    let (app, _handle) = make_mock_app().await;
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);
    state.status = AppStatus::Running;
    state.app = Some(app);

    let stopped = state.stop_app().await;
    let already_stopped = state.stop_app().await;

    assert!(matches!(stopped, StopStatus::Stopped(_)));
    assert!(matches!(already_stopped, StopStatus::AlreadyStopped));
    assert!(matches!(state.status, AppStatus::NotRunning));
    assert_eq!(state.last_shutdown, Some(ShutdownResult::Graceful));
}

#[tokio::test]
async fn cancelled_exit_wait_retains_supervisor_task() {
    let (mut app, _handle) = make_mock_app().await;
    let (release, released) = oneshot::channel::<()>();
    app.supervisor_exit_task = Some(tokio::spawn(async move {
        released.await.expect("release supervisor exit");
        Err(std_io::Error::other("observed supervisor exit"))
    }));

    {
        let mut wait = Box::pin(app.wait_for_exit());
        assert!(
            poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }
    let retained = app.supervisor_exit_task.is_some();
    release.send(()).expect("release exit task");
    let result = app.wait_for_exit().await;
    assert!(retained, "cancelled wait lost the supervisor task");
    assert_eq!(
        result,
        Err("supervisor exit failed: observed supervisor exit".to_string())
    );
    assert!(app.supervisor_exit_task.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_exit_wait_retains_direct_child() {
    let (mut app, _handle) = make_mock_app().await;
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn child waiting for stdin");
    // Keep stdin outside Child, since Child::wait closes its owned stdin.
    let stdin = child.stdin.take().expect("piped stdin");
    app.child = Some(child);

    {
        let mut wait = Box::pin(app.wait_for_exit());
        assert!(
            poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }
    let retained = app.child.is_some();
    drop(stdin);
    let result = timeout(Duration::from_secs(5), app.wait_for_exit())
        .await
        .expect("child exits after stdin closes");
    assert!(retained, "cancelled wait lost the direct child");
    assert_eq!(result, Ok(()));
    assert!(app.child.is_none());
}

#[tokio::test]
async fn shutdown_resolution_reports_normal_exit() {
    let result = resolve_shutdown(async { Ok(()) }, async { Ok(()) }, Duration::from_secs(1)).await;
    assert_eq!(result, ShutdownResult::Graceful);
}

#[tokio::test]
async fn shutdown_resolution_reports_missing_mcp() {
    let result = resolve_shutdown(
        async { Err(ShutdownCause::AppMcpUnavailable("closed".to_string())) },
        async { Ok(()) },
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(
        result,
        ShutdownResult::Forced {
            cause: ShutdownCause::AppMcpUnavailable("closed".to_string()),
        }
    );
}

#[tokio::test]
async fn shutdown_resolution_reports_close_failure() {
    let result = resolve_shutdown(
        async { Err(ShutdownCause::AppCloseFailed("rejected".to_string())) },
        async { Ok(()) },
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(
        result,
        ShutdownResult::Forced {
            cause: ShutdownCause::AppCloseFailed("rejected".to_string()),
        }
    );
}

#[tokio::test]
async fn shutdown_resolution_reports_deadline_expiry() {
    let result = resolve_shutdown(
        async { Ok(()) },
        pending::<Result<(), String>>(),
        Duration::ZERO,
    )
    .await;
    assert_eq!(
        result,
        ShutdownResult::Forced {
            cause: ShutdownCause::DeadlineExpired,
        }
    );
}

#[tokio::test]
async fn shutdown_resolution_bounds_close_request() {
    let result = resolve_shutdown(
        pending::<Result<(), ShutdownCause>>(),
        async { Ok(()) },
        Duration::ZERO,
    )
    .await;
    assert_eq!(
        result,
        ShutdownResult::Forced {
            cause: ShutdownCause::DeadlineExpired,
        }
    );
}

#[test]
fn forced_shutdown_fails_one_shot_contract() {
    let result = ShutdownResult::Forced {
        cause: ShutdownCause::DeadlineExpired,
    }
    .ensure_graceful();
    assert!(matches!(result, Err(EdevError::ForcedShutdown(_))));
}
