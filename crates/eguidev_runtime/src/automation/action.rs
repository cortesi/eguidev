//! Typed action request validation and translation.

use super::*;

pub(super) fn raw_input_action(event: RawInputEvent) -> Result<InputAction, ToolError> {
    Ok(match event {
        RawInputEvent::PointerMove { position } => InputAction::PointerMove { pos: position },
        RawInputEvent::PointerButton {
            position,
            button,
            action,
            modifiers,
        } => InputAction::PointerButton {
            pos: position,
            button: egui_pointer_button(button),
            pressed: action == RawInputAction::Press,
            modifiers: modifiers.unwrap_or_default(),
        },
        RawInputEvent::Key {
            key,
            action,
            modifiers,
        } => InputAction::Key {
            key: resolve_key_name(&key).ok_or_else(|| {
                ToolError::new(ErrorCode::InvalidArgument, format!("Unknown key: {key}"))
            })?,
            pressed: action == RawInputAction::Press,
            modifiers: modifiers.unwrap_or_default(),
        },
        RawInputEvent::Text { text } => InputAction::Text { text },
        RawInputEvent::Scroll { delta, modifiers } => InputAction::Scroll {
            delta,
            modifiers: modifiers.unwrap_or_default(),
        },
    })
}

pub(super) fn resize_commands(
    options: ResizeOptions,
) -> Result<Vec<egui::ViewportCommand>, ToolError> {
    for (field, size) in [
        ("inner_size", options.inner_size),
        ("min_size", options.min_size),
        ("max_size", options.max_size),
        ("increments", options.increments),
    ] {
        if let Some(size) = size {
            ensure_positive_vec2(size, field)?;
        }
    }
    let mut commands = Vec::new();
    if let Some(size) = options.inner_size {
        commands.push(egui::ViewportCommand::InnerSize(size.into()));
    }
    if let Some(size) = options.min_size {
        commands.push(egui::ViewportCommand::MinInnerSize(size.into()));
    }
    if let Some(size) = options.max_size {
        commands.push(egui::ViewportCommand::MaxInnerSize(size.into()));
    }
    if let Some(size) = options.increments {
        commands.push(egui::ViewportCommand::ResizeIncrements(Some(size.into())));
    }
    if let Some(resizable) = options.resizable {
        commands.push(egui::ViewportCommand::Resizable(resizable));
    }
    Ok(commands)
}

impl DevMcpServer {
    pub(super) fn resolve_widget_for_pointer(
        &self,
        viewport_id: Option<&str>,
        target: &WidgetRef,
    ) -> ToolResult<(WidgetRegistryEntry, egui::ViewportId)> {
        let (widget, viewport_id) = resolve_widget_and_viewport(&self.inner, viewport_id, target)?;
        if let Some(error) = invisible_interaction_error(&self.inner, &widget, viewport_id) {
            return Err(error.into());
        }
        Ok((widget, viewport_id))
    }

    pub(super) async fn input(
        &self,
        viewport_id: Option<String>,
        event: RawInputEvent,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let action = raw_input_action(event)?;
        self.inner.queue_action(viewport_id, action);
        Ok(())
    }

    /// Press and release a key (optionally repeating), with modifiers.
    ///
    /// `key_name` is the original user-provided key name string, used to derive the text event
    /// (preserving case for single characters like `"a"` vs `"A"`).
    pub(super) async fn action_key(
        &self,
        viewport_id: Option<String>,
        key: egui::Key,
        modifiers: Modifiers,
        key_name: &str,
        repeat: Option<u32>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let repeat = repeat.unwrap_or(1);
        if repeat == 0 {
            return Err(
                ToolError::new(ErrorCode::InvalidArgument, "Repeat must be at least 1").into(),
            );
        }
        let text = if modifiers.ctrl || modifiers.command || modifiers.alt {
            None
        } else {
            printable_key_text(key_name)
        };
        for _ in 0..repeat {
            self.inner.queue_action(
                viewport_id,
                InputAction::Key {
                    key,
                    pressed: true,
                    modifiers,
                },
            );
            if let Some(text) = text.as_deref() {
                self.inner.queue_action(
                    viewport_id,
                    InputAction::Text {
                        text: text.to_string(),
                    },
                );
            }
            self.inner.queue_action(
                viewport_id,
                InputAction::Key {
                    key,
                    pressed: false,
                    modifiers,
                },
            );
        }
        Ok(())
    }

    pub(super) async fn focus_widget_for_keyboard(
        &self,
        viewport_id: Option<String>,
        target: &WidgetRef,
        timeout_ms: Option<u64>,
    ) -> Result<(WidgetRegistryEntry, egui::ViewportId), ToolError> {
        let (widget, viewport_id) = self
            .resolve_widget_for_pointer(viewport_id.as_deref(), target)
            .map_err(|error| ToolError::new(ErrorCode::NotActionable, error.message))?;
        if !widget.enabled {
            return Err(ToolError::new(
                ErrorCode::NotActionable,
                "Target widget is not focusable",
            ));
        }
        if widget.focused {
            return Ok((widget, viewport_id));
        }
        let click_pos = widget.interact_rect.center();
        queue_primary_click(&self.inner, viewport_id, click_pos);
        let Some(timeout_ms) = timeout_ms else {
            return Ok((widget, viewport_id));
        };
        let viewport_id_str = viewport_id_to_string(viewport_id);
        self.wait_for_widget_state(
            Some(viewport_id_str.clone()),
            target.clone(),
            Some(timeout_ms),
            None,
            "to take keyboard focus",
            |widget| widget.is_some_and(|widget| widget.focused),
        )
        .await
        .map_err(|error| ToolError::new(ErrorCode::NotActionable, error.message))?;
        match resolve_widget(&self.inner, Some(viewport_id_str.as_str()), target) {
            Ok(focused_widget) if focused_widget.focused => Ok((focused_widget, viewport_id)),
            Ok(_) => Err(ToolError::new(
                ErrorCode::NotActionable,
                "Widget did not retain focus",
            )),
            Err(error) if error.code() == ErrorCode::NotFound => Err(ToolError::new(
                ErrorCode::NotActionable,
                "Target widget detached while focusing",
            )),
            Err(error) => Err(error),
        }
    }

    /// Paste text into the focused widget.
    pub(super) async fn action_paste(
        &self,
        viewport_id: Option<String>,
        text: String,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        self.inner
            .queue_action(viewport_id, InputAction::Paste { text });
        Ok(())
    }

    /// Request OS-level focus for a viewport.
    ///
    /// Raises the window and steals keyboard focus from whatever the user is currently working in.
    ///
    /// **WARNING: Do not use this for general app interaction or automation.** Input injection,
    /// clicks, keyboard events, and all other automation actions work correctly without OS focus.
    /// This function exists solely for testing window focus events themselves (e.g. verifying that
    /// your app responds correctly when it gains or loses focus). Using it unnecessarily disrupts
    /// the user's workflow.
    pub(super) async fn focus_window(&self, viewport_id: String) -> ToolResult<()> {
        let viewport_id = self
            .inner
            .viewports
            .resolve_viewport_id(Some(viewport_id))
            .map_err(ToolError::from)?;
        self.inner
            .queue_command(viewport_id, egui::ViewportCommand::Focus);
        Ok(())
    }

    /// Dismiss transient egui UI state for a viewport.
    pub(super) async fn viewport_dismiss_popups(
        &self,
        viewport_id: Option<String>,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        self.inner.dismiss_transient_ui(Some(viewport_id));
        Ok(())
    }

    /// Validate and queue one atomic viewport resize request.
    pub(super) async fn viewport_resize(
        &self,
        viewport_id: Option<String>,
        options: ResizeOptions,
    ) -> ToolResult<()> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        for command in resize_commands(options)? {
            self.inner.queue_command(viewport_id, command);
        }
        Ok(())
    }

    pub(super) async fn widget_set_value(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        value: WidgetValue,
    ) -> ToolResult<()> {
        let widget = resolve_widget(&self.inner, viewport_id.as_deref(), &target)?;
        validate_widget_value(&widget, &value)?;
        let WidgetRegistryEntry {
            id: widget_id,
            viewport_id: widget_viewport_id,
            ..
        } = widget;
        let viewport_id = self
            .inner
            .viewports
            .resolve_viewport_id(Some(widget_viewport_id))
            .map_err(ToolError::from)?;
        self.inner
            .queue_widget_value_update(viewport_id, widget_id, value);
        Ok(())
    }

    /// Queue a click on a widget without verifying resulting UI state.
    pub(super) async fn action_click(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        button: Option<PointerButton>,
        modifiers: Option<Modifiers>,
        click_count: Option<u8>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        let modifiers = modifiers.unwrap_or_default();
        let button = button.unwrap_or(PointerButton::Primary);
        let click_count = click_count.unwrap_or(1);
        if !(1..=3).contains(&click_count) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgument,
                "click_count must be between 1 and 3",
            )
            .into());
        }
        let pointer_button = egui_pointer_button(button);
        queue_click(
            &self.inner,
            viewport_id,
            pos,
            pointer_button,
            modifiers,
            click_count,
        );
        Ok(())
    }

    /// Hover over a widget without clicking.
    pub(super) async fn action_hover(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        position: Option<Vec2>,
        duration_ms: Option<u64>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = if let Some(position) = position {
            resolve_relative_pos(widget.interact_rect, position)?
        } else {
            widget.interact_rect.center()
        };
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        let duration_ms = duration_ms.unwrap_or(0);
        if duration_ms > 0 {
            let frames = frames_for_duration(duration_ms);
            wait_for_frames(&self.inner, frames, Instant::now(), duration_ms).await?;
        }
        Ok(())
    }

    /// Type into a widget (optionally clearing first).
    pub(super) async fn action_type(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        text: String,
        enter: Option<bool>,
        clear: Option<bool>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        let queue_for_next_frame = !widget.focused;
        if queue_for_next_frame {
            queue_primary_click(&self.inner, viewport_id, pos);
        }
        let queue_action = |action| {
            if queue_for_next_frame {
                self.inner.queue_action_with_timing(
                    viewport_id,
                    ActionTiming::AfterOneFrame,
                    action,
                );
            } else {
                self.inner.queue_action(viewport_id, action);
            }
        };
        let queue_key_press = |key, modifiers| {
            queue_action(InputAction::Key {
                key,
                pressed: true,
                modifiers,
            });
            queue_action(InputAction::Key {
                key,
                pressed: false,
                modifiers,
            });
        };
        let clear = clear.unwrap_or(false);
        if clear {
            let modifiers = Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            };
            queue_key_press(egui::Key::A, modifiers);
            queue_key_press(egui::Key::Backspace, Modifiers::default());
        }
        queue_action(InputAction::Text { text });
        let enter = enter.unwrap_or(false);
        if enter {
            queue_key_press(egui::Key::Enter, Modifiers::default());
        }
        Ok(())
    }

    /// Focus a widget by clicking on it.
    pub(super) async fn action_focus(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        queue_primary_click(&self.inner, viewport_id, pos);
        Ok(())
    }

    /// Drag from a widget to an absolute position (points).
    pub(super) async fn action_drag(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        to: Pos2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let start = widget.interact_rect.center();
        queue_drag(
            &self.inner,
            viewport_id,
            start,
            to,
            modifiers.unwrap_or_default(),
        );
        Ok(())
    }

    /// Drag within a widget using relative coordinates (0..1).
    pub(super) async fn action_drag_relative(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        from: Option<Vec2>,
        to: Vec2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let start_relative = from.unwrap_or(Vec2 { x: 0.5, y: 0.5 });
        let start = resolve_relative_pos(widget.interact_rect, start_relative)?;
        let end = resolve_relative_pos(widget.interact_rect, to)?;
        queue_drag(
            &self.inner,
            viewport_id,
            start,
            end,
            modifiers.unwrap_or_default(),
        );
        Ok(())
    }

    /// Drag from one widget to another's center.
    pub(super) async fn action_drag_to_widget(
        &self,
        viewport_id: Option<String>,
        from: WidgetRef,
        to: WidgetRef,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let viewport_id = viewport_id.as_deref();
        let (from_widget, from_viewport) = self.resolve_widget_for_pointer(viewport_id, &from)?;
        let (to_widget, to_viewport) = self.resolve_widget_for_pointer(viewport_id, &to)?;
        if from_viewport != to_viewport {
            return Err(ToolError::new(
                ErrorCode::InvalidArgument,
                "Drag endpoints must be in the same viewport",
            )
            .into());
        }
        let viewport_id = from_viewport;
        let start = from_widget.interact_rect.center();
        let end = to_widget.interact_rect.center();
        queue_drag(
            &self.inner,
            viewport_id,
            start,
            end,
            modifiers.unwrap_or_default(),
        );
        Ok(())
    }

    /// Scroll a scroll area.
    pub(super) async fn action_scroll(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        delta: Vec2,
        modifiers: Option<Modifiers>,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            self.resolve_widget_for_pointer(viewport_id.as_deref(), &target)?;
        let pos = widget.interact_rect.center();
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        let mut applied_override = false;
        if widget.role == WidgetRole::ScrollArea {
            let current = widget
                .role_state
                .as_ref()
                .and_then(RoleState::scroll_state)
                .map(|scroll| scroll.offset.into())
                .unwrap_or(egui::Vec2::ZERO);
            let delta_vec: egui::Vec2 = delta.into();
            let mut target = current - delta_vec;
            target.x = target.x.max(0.0);
            target.y = target.y.max(0.0);
            self.inner
                .set_scroll_override(viewport_id, widget.native_id, target);
            applied_override = true;
        }
        if !applied_override {
            self.inner.queue_action(
                viewport_id,
                InputAction::Scroll {
                    delta,
                    modifiers: modifiers.unwrap_or_default(),
                },
            );
        }
        Ok(())
    }

    /// Scroll a scroll area to an absolute offset or alignment.
    pub(super) async fn action_scroll_to(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
        offset: Option<Vec2>,
        align: Option<ScrollAlign>,
    ) -> ToolResult<Vec2> {
        let (widget, viewport_id) =
            resolve_widget_and_viewport(&self.inner, viewport_id.as_deref(), &target)?;
        if widget.role != WidgetRole::ScrollArea {
            return Err(ToolError::new(
                ErrorCode::InvalidArgument,
                "Target widget is not a scroll area",
            )
            .into());
        }
        let scroll = widget
            .role_state
            .as_ref()
            .and_then(RoleState::scroll_state)
            .ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidArgument,
                    "Scroll metadata unavailable; render the scroll area before scrolling",
                )
            })?;
        if offset.is_some() && align.is_some() {
            return Err(ToolError::new(
                ErrorCode::InvalidArgument,
                "Provide either offset or align, not both",
            )
            .into());
        }
        let mut target_offset = if let Some(offset) = offset {
            offset
        } else if let Some(align) = align {
            let y = match align {
                ScrollAlign::Top => 0.0,
                ScrollAlign::Center => scroll.max_offset.y * 0.5,
                ScrollAlign::Bottom => scroll.max_offset.y,
            };
            Vec2 {
                x: scroll.offset.x,
                y,
            }
        } else {
            return Err(ToolError::new(
                ErrorCode::InvalidArgument,
                "Provide either offset or align",
            )
            .into());
        };
        target_offset.x = target_offset.x.clamp(0.0, scroll.max_offset.x);
        target_offset.y = target_offset.y.clamp(0.0, scroll.max_offset.y);
        let pos = widget.interact_rect.center();
        self.inner
            .queue_action(viewport_id, InputAction::PointerMove { pos });
        self.inner
            .set_scroll_override(viewport_id, widget.native_id, target_offset.into());
        Ok(target_offset)
    }

    /// Scroll ancestor scroll areas so the target widget becomes visible.
    pub(super) async fn action_scroll_into_view(
        &self,
        viewport_id: Option<String>,
        target: WidgetRef,
    ) -> ToolResult<()> {
        let (widget, viewport_id) =
            resolve_widget_and_viewport(&self.inner, viewport_id.as_deref(), &target)?;
        let widgets = self.inner.widgets.widget_list(viewport_id);
        let by_id: HashMap<&str, &WidgetRegistryEntry> = widgets
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let mut target_widget = widget;
        let mut parent_id = target_widget.parent_id.clone();

        while let Some(parent_key) = parent_id {
            let Some(parent) = by_id.get(parent_key.as_str()) else {
                break;
            };
            if parent.role == WidgetRole::ScrollArea
                && let Some(offset) = scroll_area_target_offset(parent, &target_widget)
            {
                self.inner
                    .set_scroll_override(viewport_id, parent.native_id, offset.into());
            }
            target_widget = (*parent).clone();
            parent_id = parent.parent_id.clone();
        }

        Ok(())
    }
}
