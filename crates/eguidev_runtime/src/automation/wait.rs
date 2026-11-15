//! Shared wait-policy values.

use super::*;

pub(super) fn parameters(timeout_ms: Option<u64>, poll_interval_ms: Option<u64>) -> (u64, u64) {
    (
        timeout_ms.unwrap_or(super::DEFAULT_WAIT_TIMEOUT_MS),
        poll_interval_ms.unwrap_or(super::DEFAULT_POLL_INTERVAL_MS),
    )
}

impl DevMcpServer {
    pub(super) async fn wait_for_frame_count(
        &self,
        count: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> ToolResult<u64> {
        let count = count.unwrap_or(1);
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let start_frame = self.inner.frame_count();
        let target_frame = start_frame + count;
        let fixture_epoch = self.inner.fixture_epoch();

        let (matched, _, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            DEFAULT_POLL_INTERVAL_MS,
            Some(egui::ViewportId::ROOT),
            None,
            || async {
                self.inner.request_repaint_all();
                let current = self.inner.frame_count();
                // Finishing an older in-flight frame advances the counter but
                // cannot establish readiness for the current fixture.
                let fixture_captured = count == 0
                    || fixture_epoch == 0
                    || self
                        .inner
                        .viewports
                        .capture_snapshot(egui::ViewportId::ROOT)
                        .is_some_and(|capture| capture.fixture_epoch >= fixture_epoch);
                Ok::<_, ToolError>((current >= target_frame && fixture_captured, None::<()>))
            },
        )
        .await?;

        let end_frame = self.inner.frame_count();
        if matched {
            return Ok(end_frame);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for {count} frame(s) after {timeout_ms}ms."),
                &observation,
            ),
        )
        .with_details(wait_timeout_details(
            "frames",
            elapsed_ms,
            None,
            None,
            Some(start_frame),
            Some(end_frame),
            &observation,
        ))
        .into())
    }

    /// Wait until the target viewport has produced a fresh captured snapshot.
    pub(super) async fn wait_for_fresh_capture(
        &self,
        viewport_id: Option<String>,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let (timeout_ms, poll_interval_ms) = parameters(timeout_ms, poll_interval_ms);
        let start_capture = self
            .inner
            .viewports
            .capture_snapshot(viewport_id)
            .map(|snapshot| snapshot.frame_count)
            .unwrap_or(0);

        let (matched, _, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            Some(viewport_id),
            None,
            || async {
                self.inner.request_repaint_of(viewport_id);
                let current = self
                    .inner
                    .viewports
                    .capture_snapshot(viewport_id)
                    .map(|snapshot| snapshot.frame_count)
                    .unwrap_or(0);
                Ok::<_, ToolError>((current > start_capture, None::<()>))
            },
        )
        .await?;

        if matched {
            return Ok(());
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for a fresh capture after {timeout_ms}ms"),
                &observation,
            ),
        )
        .with_details(wait_timeout_details(
            "capture",
            elapsed_ms,
            None,
            viewport_snapshot_for(&self.inner, viewport_id).as_ref(),
            Some(start_capture),
            self.inner
                .viewports
                .capture_snapshot(viewport_id)
                .map(|snapshot| snapshot.frame_count),
            &observation,
        ))
        .into())
    }

    /// Wait until the UI has settled: all input actions and viewport commands
    /// are drained and at least one clean frame has been captured after the
    /// last input drain, unless the target child viewport closed while
    /// handling the action.
    pub(super) async fn wait_for_settle(
        &self,
        viewport_id: Option<String>,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> ToolResult<SettleReport> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let (timeout_ms, poll_interval_ms) = parameters(timeout_ms, poll_interval_ms);
        let start_capture = self
            .inner
            .viewports
            .capture_snapshot(viewport_id)
            .map(|snapshot| snapshot.frame_count)
            .unwrap_or(0);
        let start_frame = self.inner.frame_count();
        let start = Instant::now();

        let (matched, report, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            Some(viewport_id),
            None,
            || async {
                // The poll repaints the target viewport. An immediate child
                // viewport paints inside its parent, so the root repaints too.
                if viewport_id != egui::ViewportId::ROOT {
                    self.inner.request_repaint();
                }
                let report = settle_report(
                    &self.inner,
                    viewport_id,
                    start_capture,
                    start_frame,
                    start.elapsed().as_millis() as u64,
                );
                Ok::<_, ToolError>((report.settled, Some(report)))
            },
        )
        .await?;

        let mut report = report.unwrap_or_else(|| {
            settle_report(
                &self.inner,
                viewport_id,
                start_capture,
                start_frame,
                elapsed_ms,
            )
        });
        report.elapsed_ms = elapsed_ms;
        if matched {
            return Ok(report);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(settle_timeout_message(timeout_ms, &report), &observation),
        )
        .with_details(settle_timeout_details(
            elapsed_ms,
            viewport_snapshot_for(&self.inner, viewport_id).as_ref(),
            start_capture,
            self.inner
                .viewports
                .capture_snapshot(viewport_id)
                .map(|snapshot| snapshot.frame_count),
            &observation,
            &report,
        ))
        .into())
    }

    /// Wait for a widget to match a predicate over its current snapshot.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn wait_for_widget_state<F>(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
        condition: &str,
        mut predicate: F,
    ) -> ToolResult<Option<WidgetRegistryEntry>>
    where
        F: FnMut(Option<&WidgetRegistryEntry>) -> bool,
    {
        let (timeout_ms, poll_interval_ms) = parameters(timeout_ms, poll_interval_ms);

        let target_viewport = viewport_id
            .clone()
            .or_else(|| target.viewport_id.clone())
            .and_then(|viewport_id| {
                self.inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            });
        let (matched, widget, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            None,
            || {
                let result = match resolve_wait_widget(&self.inner, viewport_id.as_deref(), &target)
                {
                    Ok(widget) => {
                        if let Ok(resolved_viewport_id) = self
                            .inner
                            .viewports
                            .resolve_viewport_id(Some(widget.viewport_id.clone()))
                        {
                            if let Some(value) = widget.value.as_ref() {
                                self.inner.clear_widget_value_update_if_matches(
                                    resolved_viewport_id,
                                    &widget.id,
                                    value,
                                );
                            }
                            if let Some(error) = self.inner.expired_widget_value_update_error(
                                resolved_viewport_id,
                                Some(&widget.id),
                            ) {
                                Err(error.into())
                            } else {
                                let matched = predicate(Some(&widget));
                                Ok::<_, ToolError>((matched, Some(widget)))
                            }
                        } else {
                            let matched = predicate(Some(&widget));
                            Ok::<_, ToolError>((matched, Some(widget)))
                        }
                    }
                    Err(error) => {
                        if error.code() == ErrorCode::NotFound {
                            let matched = predicate(None);
                            Ok((matched, None))
                        } else {
                            Err(error)
                        }
                    }
                };
                async move { result }
            },
        )
        .await?;

        if matched {
            return Ok(widget);
        }

        let viewport = widget
            .as_ref()
            .map(|widget| widget.viewport_id.as_str())
            .or(viewport_id.as_deref())
            .or(target.viewport_id.as_deref());
        let mut details = with_widget_target(
            wait_timeout_details(
                "widget",
                elapsed_ms,
                widget.as_ref(),
                None,
                None,
                None,
                &observation,
            ),
            &target.id,
            viewport,
        );
        if widget.is_none()
            && let Err(error) = resolve_wait_widget(&self.inner, viewport_id.as_deref(), &target)
        {
            merge_missing_widget_search(&mut details, &error);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            widget_wait_timeout_message(&target.id, viewport, condition, timeout_ms, &observation),
        )
        .with_details(details)
        .into())
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SettleReport {
    pub settled: bool,
    pub elapsed_ms: u64,
    pub phases: Vec<SettlePhaseStatus>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SettlePhaseStatus {
    pub phase: SettlePhase,
    pub complete: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettlePhase {
    InputDrained,
    CommandsDrained,
    ActionFrameProcessed,
    CleanCapture,
    FreshFrame,
    AppIdle,
}

impl SettlePhase {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::InputDrained => "input_drained",
            Self::CommandsDrained => "commands_drained",
            Self::ActionFrameProcessed => "action_frame_processed",
            Self::CleanCapture => "clean_capture",
            Self::FreshFrame => "fresh_frame",
            Self::AppIdle => "app_idle",
        }
    }
}

pub(super) fn settle_report(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    start_capture: u64,
    start_frame: u64,
    elapsed_ms: u64,
) -> SettleReport {
    let capture = inner.viewports.capture_snapshot(viewport_id);
    let capture_frame = capture.map(|snapshot| snapshot.frame_count);
    let observed_new_capture = capture_frame.is_some_and(|frame| frame > start_capture);
    let target_viewport_closed =
        viewport_id != egui::ViewportId::ROOT && !inner.viewports.is_live_viewport(viewport_id);
    let pending_actions = inner.actions.pending_action_count(viewport_id);
    let pending_commands = inner.actions.pending_command_count(viewport_id);
    let last_action_frame = inner.last_action_frame.load(Ordering::Relaxed);
    let processed_last_action = inner.frame_count() > last_action_frame;
    let action_stats = inner.actions.stats(viewport_id);
    let observed_clean_action_frame = action_stats.last_drain_frame.is_none_or(|frame| {
        capture_frame.is_some_and(|capture_frame| capture_frame > frame.saturating_add(1))
    });

    let mut phases = vec![
        SettlePhaseStatus {
            phase: SettlePhase::InputDrained,
            complete: pending_actions == 0,
            detail: if pending_actions == 0 {
                "no pending input actions".to_string()
            } else {
                format!("{pending_actions} input action(s) pending")
            },
        },
        SettlePhaseStatus {
            phase: SettlePhase::CommandsDrained,
            complete: pending_commands == 0,
            detail: if pending_commands == 0 {
                "no pending viewport commands".to_string()
            } else {
                format!("{pending_commands} viewport command(s) pending")
            },
        },
        SettlePhaseStatus {
            phase: SettlePhase::ActionFrameProcessed,
            complete: processed_last_action,
            detail: format!(
                "last action frame {last_action_frame}, current frame {}",
                inner.frame_count()
            ),
        },
        SettlePhaseStatus {
            phase: SettlePhase::CleanCapture,
            complete: observed_clean_action_frame || target_viewport_closed,
            detail: if target_viewport_closed {
                "target viewport closed after action drain".to_string()
            } else {
                format!(
                    "last drain frame {}, latest capture {}",
                    action_stats
                        .last_drain_frame
                        .map(|frame| frame.to_string())
                        .unwrap_or_else(|| "none".to_string()),
                    capture_frame
                        .map(|frame| frame.to_string())
                        .unwrap_or_else(|| "none".to_string())
                )
            },
        },
        SettlePhaseStatus {
            phase: SettlePhase::FreshFrame,
            complete: observed_new_capture || target_viewport_closed,
            detail: if target_viewport_closed {
                "target viewport closed during wait".to_string()
            } else {
                format!(
                    "start frame {start_frame}, current frame {}, start capture {start_capture}, latest capture {}",
                    inner.frame_count(),
                    capture_frame
                        .map(|frame| frame.to_string())
                        .unwrap_or_else(|| "none".to_string())
                )
            },
        },
    ];

    if let Some(idle) = inner.idle.status() {
        phases.push(SettlePhaseStatus {
            phase: SettlePhase::AppIdle,
            complete: idle.idle,
            detail: idle.detail,
        });
    }

    let settled = phases.iter().all(|phase| phase.complete);
    SettleReport {
        settled,
        elapsed_ms,
        phases,
    }
}

pub(super) fn settle_timeout_message(timeout_ms: u64, report: &SettleReport) -> String {
    let incomplete = report
        .phases
        .iter()
        .filter(|phase| !phase.complete)
        .map(|phase| format!("{} ({})", phase.phase.as_str(), phase.detail))
        .collect::<Vec<_>>();
    if incomplete.is_empty() {
        format!("Timed out waiting for UI to settle after {timeout_ms}ms")
    } else {
        format!(
            "Timed out waiting for UI to settle after {timeout_ms}ms; incomplete: {}",
            incomplete.join(", ")
        )
    }
}

pub(super) fn settle_timeout_details(
    elapsed_ms: u64,
    viewport: Option<&ViewportSnapshot>,
    start_capture: u64,
    end_capture: Option<u64>,
    observation: &WaitObservation,
    report: &SettleReport,
) -> Value {
    let mut details = wait_timeout_details(
        "settle",
        elapsed_ms,
        None,
        viewport,
        Some(start_capture),
        end_capture,
        observation,
    );
    if let Some(map) = details.as_object_mut() {
        map.insert("phases".to_string(), json!(report.phases));
    }
    details
}
