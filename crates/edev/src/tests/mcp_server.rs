//! Tests of the MCP server of the launcher.

use super::*;

#[test]
fn tools_list_is_static_across_lifecycle_states() {
    let tempdir = test_tempdir();
    let state = make_state(&tempdir);
    let stopped_names = state
        .tools_list()
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let mut running_state = make_state(&tempdir);
    running_state.status = AppStatus::Running;
    let running_names = running_state
        .tools_list()
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        stopped_names,
        vec![
            "start".to_string(),
            "stop".to_string(),
            "restart".to_string(),
            "status".to_string(),
        ]
    );
    assert_eq!(stopped_names, running_names);
}

#[test]
fn tool_error_reports_restart_failed_kind() {
    let result = tool_error(ErrorKind::RestartFailed, "restart failed");
    let payload = result.structured_content.expect("structured content");
    assert_eq!(payload["error"]["kind"], "restart_failed");
}

#[test]
fn restart_retry_detector_ignores_non_transport_errors() {
    assert!(!restart_result_is_transport_closed(&Err(EdevError::Mcp(
        McpError::InternalError("boom".to_string()),
    ))));
    assert!(!restart_result_is_transport_closed(&Err(
        EdevError::AppStart("spawn failed".to_string(),)
    )));
}

#[test]
fn restart_result_detector_matches_startup_failed_transport_closed() {
    assert!(restart_result_is_transport_closed(&Ok(
        LifecycleStartStatus::StartupFailed("Transport disconnected unexpectedly".to_string()),
    )));
    assert!(!restart_result_is_transport_closed(&Ok(
        LifecycleStartStatus::StartupFailed("error[E0432]: unresolved import".to_string()),
    )));
}

#[test]
fn status_report_covers_all_lifecycle_states() {
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);
    assert_eq!(state.status_report().state, "not_running");
    assert_eq!(state.status_report().idle_shutdown.state, "disabled");

    state.status = AppStatus::Starting;
    assert_eq!(state.status_report().state, "starting");

    state.status = AppStatus::Running;
    assert_eq!(state.status_report().state, "running");

    state.status = AppStatus::StartupFailed {
        output: "boom".to_string(),
    };
    let report = state.status_report();
    assert_eq!(report.state, "startup_failed");
    assert_eq!(report.startup_output.as_deref(), Some("boom"));
}

#[test]
fn client_capabilities_encode_presentation_intent() {
    let capabilities = client_capabilities(Presentation::Foreground);
    assert_eq!(
        capabilities
            .experimental
            .as_ref()
            .and_then(|values| values.get(EXPERIMENTAL_PRESENTATION_CAPABILITY)),
        Some(&serde_json::json!("foreground"))
    );
}

#[test]
fn status_report_covers_mcp_idle_state() {
    let tempdir = test_tempdir();
    let mut state = make_state(&tempdir);
    state.enable_idle_shutdown(Duration::from_secs(30));

    let report = state.status_report();
    assert!(!report.mcp_client_attached);
    assert_eq!(report.idle_shutdown.state, "waiting_for_initial_client");
    assert_eq!(report.idle_shutdown.configured_secs, Some(30));
    assert!(matches!(report.idle_shutdown.remaining_secs, Some(0..=30)));

    state.mark_client_attached();
    let report = state.status_report();
    assert!(report.mcp_client_attached);
    assert_eq!(
        report.idle_shutdown.state,
        "suspended_while_client_attached"
    );
    assert_eq!(report.idle_shutdown.configured_secs, Some(30));
    assert_eq!(report.idle_shutdown.remaining_secs, None);
}

#[tokio::test]
async fn idle_shutdown_waits_for_inactivity() {
    let tempdir = test_tempdir();
    let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
    {
        let mut state_guard = state.lock().await;
        state_guard.mark_activity();
    }

    let waiter = tokio::spawn(wait_for_idle_shutdown(
        Arc::clone(&state),
        Duration::from_millis(80),
    ));

    sleep(Duration::from_millis(30)).await;
    {
        let mut state_guard = state.lock().await;
        state_guard.mark_activity();
    }
    sleep(Duration::from_millis(40)).await;
    assert!(!waiter.is_finished());

    sleep(Duration::from_millis(60)).await;
    assert!(waiter.is_finished());
}

#[tokio::test]
async fn idle_shutdown_stays_pending_while_client_attached() {
    let tempdir = test_tempdir();
    let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
    {
        let mut state_guard = state.lock().await;
        state_guard.last_activity = Instant::now() - Duration::from_millis(250);
        state_guard.mark_client_attached();
    }

    let wait_result = timeout(
        Duration::from_millis(20),
        wait_for_idle_shutdown(Arc::clone(&state), Duration::from_millis(100)),
    )
    .await;
    assert!(
        wait_result.is_err(),
        "attached MCP clients should suspend idle shutdown"
    );
}

#[tokio::test]
async fn list_tools_does_not_delay_idle_shutdown() {
    let tempdir = test_tempdir();
    let state = Arc::new(AsyncMutex::new(make_state(&tempdir)));
    {
        let mut state_guard = state.lock().await;
        state_guard.last_activity = Instant::now() - Duration::from_millis(250);
    }

    let server = EdevServer {
        state: Arc::clone(&state),
    };
    let ctx = TestServerContext::new();
    let _result = server
        .list_tools(ctx.ctx(), None::<Cursor>)
        .await
        .expect("list tools");

    let wait_result = timeout(
        Duration::from_millis(20),
        wait_for_idle_shutdown(Arc::clone(&state), Duration::from_millis(100)),
    )
    .await;
    assert!(
        wait_result.is_ok(),
        "list_tools should not refresh idle activity"
    );
}
