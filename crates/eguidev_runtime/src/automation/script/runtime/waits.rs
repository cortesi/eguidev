//! Waits of the script runtime.

use super::*;

impl ScriptRuntime {
    pub(in super::super) async fn wait_for_widget_predicate<F, Fut>(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
        condition: &str,
        predicate: F,
    ) -> ScriptResult<Value>
    where
        F: Fn(Value) -> Fut + Clone,
        Fut: Future<Output = ScriptResult<bool>>,
    {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        let target_viewport = viewport_id
            .clone()
            .or_else(|| target.viewport_id.clone())
            .and_then(|viewport_id| {
                self.server
                    .inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            });
        let result = super::super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            self.deadline,
            || {
                let predicate = predicate.clone();
                let target = target.clone();
                let viewport_id = viewport_id.clone();
                async move {
                    match resolve_wait_widget(&self.server.inner, viewport_id.as_deref(), &target) {
                        Ok(widget) => match self.widget_state_json(pos, &widget) {
                            Ok(widget_json) => match predicate(widget_json).await {
                                Ok(matched) => Ok::<_, ScriptErrorInfo>((matched, Some(widget))),
                                Err(error) => Err(error),
                            },
                            Err(error) => Err(self.runtime_error(
                                pos,
                                format!("Failed to prepare widget state for predicate: {error:?}"),
                            )),
                        },
                        Err(error) if error.code() == ErrorCode::NotFound => {
                            match predicate(Value::Null).await {
                                Ok(matched) => Ok((matched, None)),
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(self.tool_error(pos, error.into())),
                    }
                }
            },
        )
        .await;

        match result {
            Ok((matched, widget, elapsed_ms, observation)) => {
                if !matched
                    && self
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    widget.as_ref().map_or(Ok(Value::Null), |widget| {
                        self.widget_state_json(pos, widget)
                    })
                } else {
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
                    if let Err(error) =
                        resolve_wait_widget(&self.server.inner, viewport_id.as_deref(), &target)
                    {
                        super::super::super::merge_missing_widget_search(&mut details, &error);
                    }
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            widget_wait_timeout_message(
                                &target.id,
                                viewport,
                                condition,
                                timeout_ms,
                                &observation,
                            ),
                        )
                        .with_details(details)
                        .into_tmcp(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(in super::super) async fn wait_for_widget_absent(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let target =
            parse_widget_ref(target).map_err(|error| self.type_error(pos, error.message))?;
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        let target_viewport = viewport_id
            .clone()
            .or_else(|| target.viewport_id.clone())
            .and_then(|viewport_id| {
                self.server
                    .inner
                    .viewports
                    .resolve_viewport_id(Some(viewport_id))
                    .ok()
            });
        let result = super::super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            target_viewport,
            self.deadline,
            || {
                let result = match resolve_wait_widget(
                    &self.server.inner,
                    viewport_id.as_deref(),
                    &target,
                ) {
                    Ok(widget) => Ok::<_, ScriptErrorInfo>((false, Some(widget))),
                    Err(error) if error.code() == ErrorCode::NotFound => Ok((true, None)),
                    Err(error) => Err(self.tool_error(pos, error.into())),
                };
                async move { result }
            },
        )
        .await;

        match result {
            Ok((matched, widget, elapsed_ms, observation)) => {
                if !matched
                    && self
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    Ok(Value::Null)
                } else {
                    let viewport = widget
                        .as_ref()
                        .map(|widget| widget.viewport_id.as_str())
                        .or(viewport_id.as_deref())
                        .or(target.viewport_id.as_deref());
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            widget_wait_timeout_message(
                                &target.id,
                                viewport,
                                "to disappear",
                                timeout_ms,
                                &observation,
                            ),
                        )
                        .with_details(with_widget_target(
                            wait_timeout_details(
                                "widget_absent",
                                elapsed_ms,
                                widget.as_ref(),
                                None,
                                None,
                                None,
                                &observation,
                            ),
                            &target.id,
                            viewport,
                        ))
                        .into_tmcp(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(in super::super) async fn wait_for_frames(
        &self,
        pos: ScriptPosition,
        count: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let count =
            parse_optional_u64_val(count).map_err(|error| self.type_error(pos, error.message))?;
        // Frame counting is app-wide, so only the timeout applies here.
        let (_viewport_id, timeout_ms, _poll_interval_ms) =
            self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.or_else(|| self.configured_timeout_ms());
        let result = self
            .await_tool(pos, self.server.wait_for_frame_count(count, timeout_ms))
            .await?;
        self.to_json(pos, result)
    }

    pub(in super::super) async fn wait_for_fresh_capture(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        self.await_tool(
            pos,
            self.server
                .wait_for_fresh_capture(viewport_id, timeout_ms, poll_interval_ms),
        )
        .await?;
        self.to_json(pos, ())
    }

    pub(in super::super) async fn wait_for_settle(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let report = self
            .await_tool(
                pos,
                self.server
                    .wait_for_settle(viewport_id, timeout_ms, poll_interval_ms),
            )
            .await?;
        self.to_json(pos, report)
    }

    pub(in super::super) async fn wait_for_viewport_predicate<F, Fut>(
        &self,
        pos: ScriptPosition,
        options: Option<&Map<String, Value>>,
        predicate: F,
    ) -> ScriptResult<Value>
    where
        F: Fn(Value) -> Fut + Clone,
        Fut: Future<Output = ScriptResult<bool>>,
    {
        let (viewport_id, timeout_ms, poll_interval_ms) = self.parse_wait_options(pos, options)?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let poll_interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
        let inner = &self.server.inner;
        let viewport_id = resolve_viewport_id(inner, viewport_id)
            .map_err(|error| self.tool_error(pos, error.into()))?;

        let result = super::super::super::utils::wait_until_condition(
            &self.server.inner,
            timeout_ms,
            poll_interval_ms,
            Some(viewport_id),
            self.deadline,
            || {
                let predicate = predicate.clone();
                async move {
                    match viewport_snapshot_for(&self.server.inner, viewport_id) {
                        Some(snapshot) => match self.viewport_state_json(pos, &snapshot) {
                            Ok(viewport_json) => match predicate(viewport_json).await {
                                Ok(matched) => Ok::<_, ScriptErrorInfo>((matched, Some(snapshot))),
                                Err(error) => Err(error),
                            },
                            Err(error) => Err(self.runtime_error(
                                pos,
                                format!(
                                    "Failed to prepare viewport state for predicate: {error:?}"
                                ),
                            )),
                        },
                        None => {
                            if self.server.inner.viewports.is_live_viewport(viewport_id) {
                                Err(self.tool_error(
                                    pos,
                                    ToolError::new(
                                        ErrorCode::NotActionable,
                                        "Viewport not ready for wait",
                                    )
                                    .into_tmcp(),
                                ))
                            } else {
                                match predicate(Value::Null).await {
                                    Ok(matched) => Ok((matched, None)),
                                    Err(error) => Err(error),
                                }
                            }
                        }
                    }
                }
            },
        )
        .await;

        match result {
            Ok((matched, viewport, elapsed_ms, observation)) => {
                if !matched
                    && self
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(self.script_timeout_error(pos));
                }
                if matched {
                    match viewport {
                        Some(viewport) => self.viewport_state_json(pos, &viewport),
                        None => Ok(Value::Null),
                    }
                } else {
                    Err(self.tool_error(
                        pos,
                        ToolError::new(
                            ErrorCode::Timeout,
                            wait_timeout_message(
                                format!(
                                    "Timed out waiting for viewport predicate after {timeout_ms}ms"
                                ),
                                &observation,
                            ),
                        )
                        .with_details(wait_timeout_details(
                            "viewport",
                            elapsed_ms,
                            None,
                            viewport.as_ref(),
                            None,
                            None,
                            &observation,
                        ))
                        .into_tmcp(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }
}
