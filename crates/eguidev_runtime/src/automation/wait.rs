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

        let (matched, _, elapsed_ms, observation) = wait_until_condition(
            &self.inner,
            timeout_ms,
            DEFAULT_POLL_INTERVAL_MS,
            Some(egui::ViewportId::ROOT),
            None,
            || async {
                self.inner.request_repaint_all();
                let current = self.inner.frame_count();
                Ok::<_, ToolError>((current >= target_frame, None::<()>))
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

    /// Wait until the UI has settled: all input actions and viewport commands are drained
    /// and at least one clean frame has been captured after the last input drain, unless
    /// the target child viewport closed while handling the action.
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
                self.inner.request_repaint_all();
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

        let mut details = wait_timeout_details(
            "widget",
            elapsed_ms,
            widget.as_ref(),
            None,
            None,
            None,
            &observation,
        );
        if widget.is_none()
            && let Err(error) = resolve_wait_widget(&self.inner, viewport_id.as_deref(), &target)
        {
            merge_missing_widget_search(&mut details, &error);
        }

        Err(ToolError::new(
            ErrorCode::Timeout,
            wait_timeout_message(
                format!("Timed out waiting for widget {condition} after {timeout_ms}ms"),
                &observation,
            ),
        )
        .with_details(details)
        .into())
    }
}
