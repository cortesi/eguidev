//! Embedded runtime attachment for DevMCP automation.

#[cfg(any(test, target_os = "macos"))]
use std::pin::pin;
use std::{any::Any, env, future::Future, sync::Arc, thread, time::Duration};

use egui::{Context, FullOutput};
use eguidev::internal::{
    devmcp::RuntimeHooks,
    presentation::Presentation,
    registry::{Inner, viewport_id_to_string},
};
#[cfg(test)]
use tokio::time::sleep;
use tokio::{sync::Notify, time::timeout};

#[cfg(target_os = "macos")]
use crate::macos::{
    configure_session, disconnect_session, platform_window_states, reassert_background_policy,
};
use crate::{
    DevMcp, McpEndpoint, ScriptErrorInfo, ScriptEvalOptions, ScriptEvalOutcome,
    automation::{
        DEFAULT_SCRIPT_EVAL_TIMEOUT_MS,
        script::{run_script_eval, warm_checker_baseline},
    },
    egui_diagnostics::EguiDiagnosticJournal,
    mcp_endpoint_from,
    screenshots::{ScreenshotDebugSnapshot, ScreenshotKind, ScreenshotManager, ScreenshotState},
    server::start_server,
};

const PRESENTATION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct Runtime {
    screenshots: ScreenshotManager,
    frame_notify: Notify,
    egui_diagnostics: EguiDiagnosticJournal,
    diagnostic_barriers_enabled: bool,
}

#[derive(Debug)]
struct RuntimeHooksImpl {
    runtime: Arc<Runtime>,
}

impl Runtime {
    fn new(diagnostic_barriers_enabled: bool) -> Self {
        Self {
            screenshots: ScreenshotManager::new(),
            frame_notify: Notify::new(),
            egui_diagnostics: EguiDiagnosticJournal::new(),
            diagnostic_barriers_enabled,
        }
    }

    #[cfg(test)]
    pub(crate) fn ensure_for_inner(inner: &Arc<Inner>) -> Arc<Self> {
        if let Some(runtime) = Self::from_inner(inner) {
            return runtime;
        }
        let runtime = Arc::new(Self::new(false));
        inner.set_runtime_hooks(Arc::new(RuntimeHooksImpl {
            runtime: Arc::clone(&runtime),
        }));
        runtime
    }

    #[cfg(test)]
    pub(crate) fn ensure_for_inner_with_diagnostic_barriers(inner: &Arc<Inner>) -> Arc<Self> {
        if let Some(runtime) = Self::from_inner(inner) {
            return runtime;
        }
        let runtime = Arc::new(Self::new(true));
        inner.set_runtime_hooks(Arc::new(RuntimeHooksImpl {
            runtime: Arc::clone(&runtime),
        }));
        runtime
    }

    pub(crate) fn from_inner(inner: &Inner) -> Option<Arc<Self>> {
        let hooks = inner.runtime_hooks()?;
        let hooks = hooks.as_any().downcast_ref::<RuntimeHooksImpl>()?;
        Some(Arc::clone(&hooks.runtime))
    }

    pub(crate) fn for_devmcp(devmcp: &DevMcp) -> Option<Arc<Self>> {
        let hooks = devmcp.runtime_hooks()?;
        let hooks = hooks.as_any().downcast_ref::<RuntimeHooksImpl>()?;
        Some(Arc::clone(&hooks.runtime))
    }

    pub(crate) fn frame_notify(&self) -> &Notify {
        &self.frame_notify
    }

    pub(crate) fn egui_diagnostics(&self) -> &EguiDiagnosticJournal {
        &self.egui_diagnostics
    }

    pub(crate) fn diagnostic_barriers_enabled(&self) -> bool {
        self.diagnostic_barriers_enabled
    }

    pub(crate) fn screenshot_state(&self, request_id: u64) -> Option<ScreenshotState> {
        self.screenshots.screenshot_state(request_id)
    }

    pub(crate) fn insert_screenshot(&self, request_id: u64, state: ScreenshotState) {
        self.screenshots.insert_screenshot(request_id, state);
    }

    pub(crate) fn take_screenshot(&self, request_id: u64) -> Option<ScreenshotState> {
        self.screenshots.take_screenshot(request_id)
    }

    pub(crate) fn record_screenshot_request(
        &self,
        inner: &Inner,
        request_id: u64,
        viewport_id: egui::ViewportId,
        kind: &ScreenshotKind,
    ) {
        self.screenshots.record_screenshot_request(
            request_id,
            viewport_id,
            kind,
            inner.verbose_logging(),
            inner.frame_count(),
        );
    }

    pub(crate) fn record_screenshot_command_sent(
        &self,
        inner: &Inner,
        viewport_id: egui::ViewportId,
        request_id: Option<u64>,
    ) {
        self.screenshots.record_screenshot_command_sent(
            viewport_id,
            request_id,
            inner.verbose_logging(),
            inner.frame_count(),
        );
    }

    pub(crate) fn screenshot_debug_snapshot(&self, inner: &Inner) -> ScreenshotDebugSnapshot {
        self.screenshots
            .screenshot_debug_snapshot(inner.verbose_logging(), inner.frame_count())
    }

    pub(crate) fn log_screenshot(&self, inner: &Inner, message: String) {
        self.screenshots
            .log_screenshot(inner.verbose_logging(), message);
    }

    pub(crate) async fn configure_presentation(
        &self,
        inner: &Inner,
        session_id: u64,
        presentation: Presentation,
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            let mut frame_complete = pin!(self.frame_notify.notified());
            frame_complete.as_mut().enable();
            let needs_frame = timeout(
                PRESENTATION_HANDSHAKE_TIMEOUT,
                configure_session(session_id, presentation),
            )
            .await
            .map_err(|_| "timed out waiting for macOS presentation handshake".to_string())??;
            if needs_frame {
                inner.request_repaint_of(egui::ViewportId::ROOT);
                self.await_frame_notification(frame_complete, PRESENTATION_HANDSHAKE_TIMEOUT)
                    .await?;
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (inner, session_id, presentation);
        }
        Ok(())
    }

    async fn await_frame_notification(
        &self,
        notified: impl Future<Output = ()>,
        bound: Duration,
    ) -> Result<(), String> {
        timeout(bound, notified).await.map_err(|_| {
            "timed out waiting for a UI frame after presentation configure".to_string()
        })
    }

    pub(crate) async fn disconnect_presentation(&self, session_id: u64) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        disconnect_session(session_id).await?;
        #[cfg(not(target_os = "macos"))]
        let _ = session_id;
        Ok(())
    }

    fn capture_screenshot_events(&self, inner: &Inner, events: &[egui::Event]) {
        self.screenshots.capture_screenshot_events(
            events,
            inner.verbose_logging(),
            inner.frame_count(),
        );
    }

    fn finish_frame(&self, inner: &Inner, ctx: &Context) {
        let viewport_id = ctx.viewport_id();
        let mut sent_viewport_command = false;
        for command in inner.actions.drain_commands(viewport_id) {
            sent_viewport_command = true;
            if let egui::ViewportCommand::Screenshot(user_data) = &command {
                let request_id = user_data
                    .data
                    .as_ref()
                    .and_then(|data| data.downcast_ref::<u64>())
                    .copied();
                self.record_screenshot_command_sent(inner, viewport_id, request_id);
            }
            ctx.send_viewport_cmd(command);
        }
        if sent_viewport_command {
            ctx.request_repaint();
        }
        inner.viewports.update_viewports(ctx);
        inner.forget_dead_viewports();
        #[cfg(target_os = "macos")]
        inner
            .viewports
            .merge_platform_state(&platform_window_states());
        #[cfg(target_os = "macos")]
        if let Some(conflict) = reassert_background_policy() {
            let diagnostic = serde_json::to_string(&conflict)
                .expect("macOS presentation conflict should serialize");
            eprintln!("eguidev: macos.presentation_conflict {diagnostic}");
        }
        inner.paint_overlays(ctx);
        self.frame_notify.notify_waiters();
    }

    fn capture_egui_output(
        &self,
        inner: &Inner,
        viewport_id: egui::ViewportId,
        output: &FullOutput,
    ) {
        self.egui_diagnostics.record_output(
            viewport_id_to_string(viewport_id),
            inner.frame_count(),
            output,
        );
    }
}

impl RuntimeHooks for RuntimeHooksImpl {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }

    fn on_raw_input(&self, inner: &Inner, events: &[egui::Event]) {
        self.runtime.capture_screenshot_events(inner, events);
    }

    fn on_frame_end(&self, inner: &Inner, ctx: &Context) {
        self.runtime.finish_frame(inner, ctx);
    }

    fn on_egui_output(&self, inner: &Inner, viewport_id: egui::ViewportId, output: &FullOutput) {
        self.runtime.capture_egui_output(inner, viewport_id, output);
    }
}

/// Attach the embedded runtime to an inert `DevMcp` handle.
pub fn attach(devmcp: DevMcp) -> DevMcp {
    match mcp_endpoint_from(env::var_os(crate::MCP_ADDR_ENV).as_deref()) {
        McpEndpoint::Absent => attach_internal(devmcp, false, None, true),
        McpEndpoint::Invalid(message) => {
            eprintln!("eguidev: {message}");
            attach_internal(devmcp, false, None, true)
        }
        McpEndpoint::Valid(addr) => attach_internal(devmcp, true, Some(addr), true),
    }
}

fn attach_internal(
    devmcp: DevMcp,
    automation_active: bool,
    server_addr: Option<String>,
    diagnostic_barriers_enabled: bool,
) -> DevMcp {
    if devmcp.is_enabled() || !automation_active {
        return devmcp;
    }

    #[cfg(target_os = "macos")]
    {
        use crate::macos::install_occlusion_hook;
        install_occlusion_hook();
    }

    let inner = Arc::new(Inner::new());
    let runtime = Arc::new(Runtime::new(diagnostic_barriers_enabled));
    let hooks = Arc::new(RuntimeHooksImpl {
        runtime: Arc::clone(&runtime),
    });
    let devmcp = devmcp.activate_runtime(Arc::clone(&inner), hooks);
    // Prepare the script checker off the startup path. A script that arrives
    // first still blocks on the same work, so this only ever moves the cost.
    drop(thread::spawn(warm_checker_baseline));
    if let Some(addr) = server_addr {
        start_server(inner, runtime, addr);
    }
    devmcp
}

#[cfg(test)]
pub fn attach_for_tests(devmcp: DevMcp) -> DevMcp {
    attach_internal(devmcp, true, None, false)
}

/// Evaluate a Luau script directly against this attached `DevMcp` instance.
pub async fn eval_script(
    devmcp: &DevMcp,
    script_source: &str,
    timeout_ms: Option<u64>,
    options: ScriptEvalOptions,
) -> ScriptEvalOutcome {
    let Some(inner) = devmcp.inner_arc() else {
        return ScriptEvalOutcome::error_only(ScriptErrorInfo {
            error_type: "runtime".to_string(),
            message: "DevMCP runtime is not attached".to_string(),
            location: None,
            backtrace: None,
            code: None,
            details: None,
        });
    };
    let runtime = Runtime::for_devmcp(devmcp).expect("runtime attached");
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_SCRIPT_EVAL_TIMEOUT_MS);
    let source_name = options
        .source_name
        .unwrap_or_else(|| "script.luau".to_string());
    run_script_eval(
        inner,
        runtime,
        script_source.to_string(),
        timeout_ms,
        source_name,
        options.args,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{reset_start_server_calls, start_server_calls};

    #[test]
    fn inactive_handle_does_not_start_server() {
        reset_start_server_calls();
        let _devmcp = DevMcp::new();
        assert_eq!(start_server_calls(), 0);
    }

    #[test]
    fn inactive_attach_stays_inert() {
        reset_start_server_calls();
        let devmcp = attach_internal(DevMcp::new(), false, Some("127.0.0.1:0".to_string()), true);
        assert!(!devmcp.is_enabled());
        assert_eq!(start_server_calls(), 0);
    }

    #[test]
    fn active_attach_starts_server_once() {
        reset_start_server_calls();
        let devmcp = attach_internal(DevMcp::new(), true, Some("127.0.0.1:0".to_string()), true);
        assert!(devmcp.is_enabled());
        assert_eq!(start_server_calls(), 1);
    }

    #[tokio::test]
    async fn await_frame_notification_times_out_without_frames() {
        use std::time::Instant;

        let runtime = Runtime::new(false);
        let mut notified = pin!(runtime.frame_notify.notified());
        notified.as_mut().enable();
        let started = Instant::now();
        let error = runtime
            .await_frame_notification(notified, Duration::from_millis(30))
            .await
            .expect_err("missing frames should time out");
        assert!(error.contains("timed out waiting for a UI frame"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn await_frame_notification_succeeds_after_notify() {
        let runtime = Arc::new(Runtime::new(false));
        let mut notified = pin!(runtime.frame_notify.notified());
        notified.as_mut().enable();
        let runtime_for_notify = Arc::clone(&runtime);
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            runtime_for_notify.frame_notify.notify_waiters();
        });
        runtime
            .await_frame_notification(notified, Duration::from_secs(1))
            .await
            .expect("notify should complete the wait");
    }
}
