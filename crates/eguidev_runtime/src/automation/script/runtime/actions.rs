//! Actions of the script runtime: pointer and key input, scrolls, and viewport
//! commands.

use super::*;

impl ScriptRuntime {
    pub(in super::super) async fn action_click(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let button = match map_value(options, "button") {
            None => None,
            Some(value) => Some(
                parse_pointer_button(value).map_err(|error| self.type_error(pos, error.message))?,
            ),
        };
        let modifiers =
            parse_modifiers(options).map_err(|error| self.type_error(pos, error.message))?;
        let click_count = parse_optional_u8(options, "click_count")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_click(
                Some(action_viewport_id.clone()),
                target.clone(),
                button,
                modifiers,
                click_count,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_hover(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let position = parse_optional_vec2(options, "position")
            .map_err(|error| self.type_error(pos, error.message))?;
        let duration_ms = parse_optional_u64(options, "duration_ms")
            .map_err(|error| self.type_error(pos, error.message))?;
        let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
        let confirm = self
            .action_settle_enabled(pos, options)?
            .then_some(HoverConfirm {
                timeout_ms,
                poll_interval_ms,
            });
        self.await_tool(
            pos,
            self.server.action_hover(
                Some(action_viewport_id.clone()),
                target,
                position,
                duration_ms,
                confirm,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_type(
        &self,
        pos: ScriptPosition,
        target: &Value,
        text: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let clear = parse_optional_bool(options, "clear")
            .map_err(|error| self.type_error(pos, error.message))?;
        let enter = parse_optional_bool(options, "enter")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.focus_for_keyboard(pos, Some(action_viewport_id.clone()), &target, options)
            .await?;
        self.await_tool(
            pos,
            self.server.action_type(
                Some(action_viewport_id.clone()),
                target.clone(),
                text,
                enter,
                clear,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    /// Give a widget the keyboard focus, and confirm it within the action
    /// timeout, before a keyboard action types into it.
    pub(super) async fn focus_for_keyboard(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
        target: &WidgetRef,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<()> {
        let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
        self.await_tool(pos, async {
            self.server
                .focus_widget_for_keyboard(
                    viewport_id,
                    target,
                    timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS),
                    poll_interval_ms,
                )
                .await
                .map(|_| ())
                .map_err(tmcp::ToolError::from)
        })
        .await
    }

    pub(in super::super) async fn action_focus(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        self.await_tool(
            pos,
            self.server
                .action_focus(Some(action_viewport_id.clone()), target),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_drag(
        &self,
        pos: ScriptPosition,
        target: &Value,
        to: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let to = parse_pos2(to).map_err(|error| self.type_error(pos, error.message))?;
        let from = parse_optional_vec2(options, "from")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers =
            parse_modifiers(options).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_drag(
                Some(action_viewport_id.clone()),
                target,
                from,
                to,
                modifiers,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_drag_relative(
        &self,
        pos: ScriptPosition,
        target: &Value,
        to: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let to = parse_vec2(to).map_err(|error| self.type_error(pos, error.message))?;
        let from = parse_optional_vec2(options, "from")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers =
            parse_modifiers(options).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_drag_relative(
                Some(action_viewport_id.clone()),
                target,
                from,
                to,
                modifiers,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_drag_to_widget(
        &self,
        pos: ScriptPosition,
        from: &Value,
        to: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (from, action_viewport_id) = self.parse_action_target(pos, from, options)?;
        let to = parse_widget_ref(to).map_err(|error| self.type_error(pos, error.message))?;
        let start = parse_optional_vec2(options, "from")
            .map_err(|error| self.type_error(pos, error.message))?;
        let modifiers =
            parse_modifiers(options).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server.action_drag_to_widget(
                Some(action_viewport_id.clone()),
                from,
                start,
                to,
                modifiers,
            ),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_scroll(
        &self,
        pos: ScriptPosition,
        target: &Value,
        delta: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let delta = parse_vec2(delta).map_err(|error| self.type_error(pos, error.message))?;
        let modifiers =
            parse_modifiers(options).map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(
            pos,
            self.server
                .action_scroll(Some(action_viewport_id.clone()), target, delta, modifiers),
        )
        .await?;
        self.finish_action(pos, options, Some(action_viewport_id))
            .await
    }

    pub(in super::super) async fn action_scroll_to(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let offset = parse_optional_vec2(options, "offset")
            .map_err(|error| self.type_error(pos, error.message))?;
        let mut align = match map_value(options, "align") {
            None => None,
            Some(value) => Some(
                parse_scroll_align(value).map_err(|error| self.type_error(pos, error.message))?,
            ),
        };
        if offset.is_some() {
            align = None;
        }
        if offset.is_none() && align.is_none() {
            return Err(self.type_error(pos, "scroll_to requires either offset or align"));
        }
        let target_offset = self
            .await_tool(
                pos,
                self.server.action_scroll_to(
                    Some(action_viewport_id.clone()),
                    target.clone(),
                    offset,
                    align,
                ),
            )
            .await?;
        let settle_enabled = self.action_settle_enabled(pos, options)?;
        self.settle_after_action(pos, options, Some(action_viewport_id.clone()))
            .await?;
        if settle_enabled {
            let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    "to reach the requested scroll offset",
                    |widget| {
                        widget
                            .and_then(|widget| widget.role_state.as_ref())
                            .and_then(|state| state.scroll_state())
                            .is_some_and(|scroll| {
                                scroll_offsets_match(scroll.offset, target_offset)
                            })
                    },
                ),
            )
            .await?;
        }
        self.to_json(pos, ())
    }

    pub(in super::super) async fn action_scroll_into_view(
        &self,
        pos: ScriptPosition,
        target: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let applied = self
            .await_tool(
                pos,
                self.server
                    .action_scroll_into_view(Some(action_viewport_id.clone()), target.clone()),
            )
            .await?;
        let settle_enabled = self.action_settle_enabled(pos, options)?;
        self.settle_after_action(pos, options, Some(action_viewport_id.clone()))
            .await?;
        if settle_enabled {
            // Requesting the offsets only queues them. Each scrolled area is
            // polled until it reports the requested offset on two consecutive
            // captures, then the target is polled until its rect stops moving
            // and it is interaction-ready. Otherwise the next click fails on
            // the call that just returned.
            let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
            for scroll in applied {
                let requested = scroll.offset;
                let mut previous: Option<Vec2> = None;
                self.await_tool(
                    pos,
                    self.server.wait_for_widget_state(
                        Some(action_viewport_id.clone()),
                        WidgetRef {
                            id: scroll.widget_id,
                            viewport_id: None,
                        },
                        timeout_ms,
                        poll_interval_ms,
                        "to reach the requested offset after scroll_into_view",
                        move |widget| {
                            let Some(current) = widget
                                .and_then(|widget| widget.role_state.as_ref())
                                .and_then(RoleState::scroll_state)
                                .map(|scroll| scroll.offset)
                            else {
                                previous = None;
                                return false;
                            };
                            let reached = (current.x - requested.x).abs()
                                <= SCROLL_STABILITY_TOLERANCE
                                && (current.y - requested.y).abs() <= SCROLL_STABILITY_TOLERANCE;
                            let stable = previous
                                .is_some_and(|prior| prior.x == current.x && prior.y == current.y);
                            previous = Some(current);
                            reached && stable
                        },
                    ),
                )
                .await?;
            }
            let mut previous_rect: Option<Rect> = None;
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    "to become interaction-ready after scroll_into_view",
                    move |widget| {
                        let Some(widget) = widget else {
                            previous_rect = None;
                            return false;
                        };
                        let stable = previous_rect == Some(widget.interact_rect);
                        previous_rect = Some(widget.interact_rect);
                        stable && interaction_ready(widget)
                    },
                ),
            )
            .await?;
        }
        self.to_json(pos, ())
    }

    pub(in super::super) async fn action_key(
        &self,
        pos: ScriptPosition,
        combo: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (key, modifiers, key_name) =
            parse_key_combo(&combo).map_err(|msg| self.type_error(pos, msg))?;
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        let repeat = parse_optional_u32(options, "repeat_count")
            .map_err(|error| self.type_error(pos, error.message))?;
        let target = match map_value(options, "target") {
            Some(value) => {
                Some(parse_widget_ref(value).map_err(|error| self.type_error(pos, error.message))?)
            }
            None => None,
        };
        let action_viewport_id = if let Some(target) = &target {
            Some(self.resolve_target_viewport(pos, viewport_id.as_deref(), target)?)
        } else {
            viewport_id
        };
        if let Some(target) = &target {
            self.focus_for_keyboard(pos, action_viewport_id.clone(), target, options)
                .await?;
        }
        self.await_tool(
            pos,
            self.server.action_key(
                action_viewport_id.clone(),
                key,
                modifiers,
                &key_name,
                repeat,
            ),
        )
        .await?;
        self.finish_action(pos, options, action_viewport_id).await
    }

    pub(in super::super) async fn action_paste(
        &self,
        pos: ScriptPosition,
        text: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let viewport_id = parse_optional_string(options, "viewport_id")
            .map_err(|error| self.type_error(pos, error.message))?;
        self.await_tool(pos, self.server.action_paste(viewport_id.clone(), text))
            .await?;
        self.finish_action(pos, options, viewport_id).await
    }

    pub(in super::super) async fn viewport_input(
        &self,
        pos: ScriptPosition,
        event: &Value,
        viewport_id: String,
    ) -> ScriptResult<Value> {
        let event = serde_json::from_value::<RawInputEvent>(event.clone())
            .map_err(|error| self.type_error(pos, format!("invalid raw input event: {error}")))?;
        self.record_targeted_viewport(viewport_id.clone());
        self.await_tool(pos, self.server.input(Some(viewport_id), event))
            .await?;
        self.to_json(pos, ())
    }

    pub(in super::super) async fn viewport_resize(
        &self,
        pos: ScriptPosition,
        options: &Value,
        viewport_id: String,
    ) -> ScriptResult<Value> {
        let map = options.as_object();
        let vec2 = |key: &str| {
            parse_optional_vec2(map, key).map_err(|error| self.type_error(pos, error.message))
        };
        // `finish_action` reads the shared action controls.
        let resize = ResizeOptions {
            action: ActionOptions::default(),
            inner_size: vec2("inner_size")?,
            min_size: vec2("min_size")?,
            max_size: vec2("max_size")?,
            increments: vec2("increments")?,
            resizable: parse_optional_bool(map, "resizable")
                .map_err(|error| self.type_error(pos, error.message))?,
        };
        self.record_targeted_viewport(viewport_id.clone());
        self.await_tool(
            pos,
            self.server
                .viewport_resize(Some(viewport_id.clone()), resize),
        )
        .await?;
        self.finish_action(pos, options.as_object(), Some(viewport_id))
            .await
    }

    pub(in super::super) async fn focus_window(
        &self,
        pos: ScriptPosition,
        viewport: String,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        self.await_tool(pos, self.server.focus_window(viewport.clone()))
            .await?;
        self.finish_action(pos, options, Some(viewport)).await
    }

    pub(in super::super) async fn viewport_dismiss_popups(
        &self,
        pos: ScriptPosition,
        viewport_id: Option<String>,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        self.await_tool(
            pos,
            self.server.viewport_dismiss_popups(viewport_id.clone()),
        )
        .await?;
        self.finish_action(pos, options, viewport_id).await
    }

    pub(in super::super) async fn widget_set_value(
        &self,
        pos: ScriptPosition,
        target: &Value,
        value: &Value,
        options: Option<&Map<String, Value>>,
    ) -> ScriptResult<Value> {
        let (target, action_viewport_id) = self.parse_action_target(pos, target, options)?;
        let value = widget_value_from_dynamic(value)
            .map_err(|error| self.type_error(pos, error.message))?;
        let settle_enabled = self.action_settle_enabled(pos, options)?;
        self.await_tool(
            pos,
            self.server.widget_set_value(
                Some(action_viewport_id.clone()),
                target.clone(),
                value.clone(),
            ),
        )
        .await?;
        self.settle_after_action(pos, options, Some(action_viewport_id.clone()))
            .await?;
        if settle_enabled {
            let (timeout_ms, poll_interval_ms) = self.action_timeouts(pos, options)?;
            self.await_tool(
                pos,
                self.server.wait_for_widget_state(
                    Some(action_viewport_id),
                    target,
                    timeout_ms,
                    poll_interval_ms,
                    "to accept the requested value",
                    |widget| {
                        widget
                            .and_then(|widget| widget.value.as_ref())
                            .is_some_and(|current| widget_values_match(current, &value))
                    },
                ),
            )
            .await?;
        }
        self.to_json(pos, ())
    }
}

pub(super) fn scroll_offsets_match(current: Vec2, expected: Vec2) -> bool {
    (current.x - expected.x).abs() <= SCROLL_STABILITY_TOLERANCE
        && (current.y - expected.y).abs() <= SCROLL_STABILITY_TOLERANCE
}
