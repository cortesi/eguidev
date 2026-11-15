//! Registration of the hidden native module of the kernel.

use ruau::module::Builder;

use super::*;

pub(super) struct EguidevModule {
    pub(super) args: Value,
    pub(super) runtime: Arc<ScriptRuntime>,
    pub(super) declaration: String,
}

/// Names the module registers, grouped by the table that receives them.
///
/// Ruau audits declared globals against the declaration source, but a hidden
/// method table has no declaration to audit. This record is what the
/// declaration parity test compares `eguidev.d.luau` against.
#[derive(Debug, Default)]
pub(super) struct ModuleSurface {
    /// Global function names.
    pub(super) globals: BTreeSet<String>,
    /// Method names keyed by hidden table name.
    pub(super) methods: BTreeMap<String, BTreeSet<String>>,
}

impl ModuleSurface {
    pub(super) fn record(&mut self, name: &str, binding: &ModuleBinding) {
        match binding {
            ModuleBinding::Global | ModuleBinding::GlobalOverride => {
                self.globals.insert(name.to_string());
            }
            ModuleBinding::Hidden(table) => {
                self.methods
                    .entry(table.to_string())
                    .or_default()
                    .insert(name.to_string());
            }
            ModuleBinding::Library(_) | ModuleBinding::LibraryOverride(_) => {}
        }
    }
}

pub(super) struct DeclaredModuleBuilder<'a> {
    pub(super) builder: &'a mut Builder,
    pub(super) surface: ModuleSurface,
}

impl DeclaredModuleBuilder<'_> {
    /// Register an async call of the hidden table `table` that returns the JSON
    /// value of one runtime call.
    pub(super) fn json_call<A, F, Fut>(
        &mut self,
        name: &str,
        table: &'static str,
        runtime: &Arc<ScriptRuntime>,
        call: F,
    ) where
        A: for<'s> FromLuaMulti<'s> + Send + 'static,
        F: Fn(Arc<ScriptRuntime>, ScriptPosition, A) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ScriptResult<Value>> + Send + 'static,
    {
        let runtime = Arc::clone(runtime);
        self.async_function(
            name,
            ModuleBinding::hidden(table),
            async_host_fn(move |ctx: AsyncHostContext, args: A| {
                let runtime = Arc::clone(&runtime);
                let call = call.clone();
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let value = call(runtime, pos, args).await.or_raise(&ctx).await?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
    }

    /// Register an async call of the hidden table `table` that returns
    /// nothing, whatever the runtime call returns.
    pub(super) fn unit_call<A, F, Fut, T>(
        &mut self,
        name: &str,
        table: &'static str,
        runtime: &Arc<ScriptRuntime>,
        call: F,
    ) where
        A: for<'s> FromLuaMulti<'s> + Send + 'static,
        F: Fn(Arc<ScriptRuntime>, ScriptPosition, A) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ScriptResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        let runtime = Arc::clone(runtime);
        self.async_function(
            name,
            ModuleBinding::hidden(table),
            async_host_fn(move |ctx: AsyncHostContext, args: A| {
                let runtime = Arc::clone(&runtime);
                let call = call.clone();
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    call(runtime, pos, args).await.or_raise(&ctx).await?;
                    Ok(HostReturn::default())
                }
            }),
        );
    }

    pub(super) fn borrowed_function<F>(&mut self, name: &str, binding: ModuleBinding, function: F)
    where
        F: for<'s> Fn(&Scope<'s>, MultiValue<'s>) -> Result<MultiValue<'s>, RuntimeError>
            + Send
            + Sync
            + 'static,
    {
        self.surface.record(name, &binding);
        self.builder
            .borrowed_function(name, library::declared_binding(binding), function);
    }

    pub(super) fn async_function(
        &mut self,
        name: &str,
        binding: ModuleBinding,
        function: Box<dyn AsyncHostFunction>,
    ) {
        self.surface.record(name, &binding);
        self.builder.async_function(
            name,
            library::declared_binding(binding),
            Arc::from(function),
        );
    }
}

impl EguidevModule {
    /// Build the native module and report the surface it registered.
    pub(super) fn build(self) -> (Arc<dyn NativeModule>, ModuleSurface) {
        let mut native = Builder::from_declaration(
            "eguidev_initial",
            DeclarationSource::Text(&self.declaration),
        );
        let mut builder = DeclaredModuleBuilder {
            builder: &mut native,
            surface: ModuleSurface::default(),
        };
        self.register_core_globals(&mut builder);
        self.register_viewport_methods(&mut builder);
        self.register_widget_methods(&mut builder);
        self.register_capture_methods(&mut builder);
        self.register_script_utility_globals(&mut builder);
        let surface = builder.surface;
        library::register(&mut native);
        let module = native
            .build()
            .expect("Eguidev declaration matches its runtime bindings");
        (module, surface)
    }

    pub(super) fn register_core_globals(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "assertion",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, args| assert_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "configure",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, args| configure_host(&runtime, scope, args),
        );
        builder.json_call(
            "apply",
            "eguidev.fixture",
            &self.runtime,
            |runtime, pos, args: FixtureArgs| async move {
                runtime.fixture_apply(pos, args.name, args.params).await
            },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "list",
            ModuleBinding::hidden("eguidev.fixture"),
            move |scope, args| fixtures_host(&runtime, scope, &args),
        );
        builder.json_call(
            "get",
            "eguidev.diagnostic",
            &self.runtime,
            |runtime, pos, name: String| async move { runtime.diagnostic(pos, name).await },
        );
        builder.json_call(
            "all",
            "eguidev.diagnostic",
            &self.runtime,
            |runtime, pos, (): ()| async move { runtime.diagnostics(pos).await },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "dump",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| dump_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "dump_text",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| dump_text_host(&runtime, scope, args),
        );
        builder.json_call(
            "viewport_list",
            "eguidev.query",
            &self.runtime,
            |runtime, pos, (): ()| async move { runtime.viewports_list(pos).await },
        );
    }

    pub(super) fn register_capture_methods(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "diff",
            ModuleBinding::hidden("eguidev.capture"),
            move |scope, args| capture_diff_host(&runtime, scope, args),
        );
    }

    pub(super) fn register_viewport_methods(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "viewport_state",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| viewport_state_host(&runtime, scope, args),
        );
        builder.json_call(
            "egui",
            "eguidev.diagnostic",
            &self.runtime,
            |runtime, pos, viewport: ViewportReceiver| async move {
                runtime.egui_diagnostics(pos, viewport.id).await
            },
        );
        builder.unit_call(
            "clear_egui",
            "eguidev.diagnostic",
            &self.runtime,
            |runtime, pos, viewport: ViewportReceiver| async move {
                runtime.clear_egui_diagnostics(pos, viewport.id).await
            },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "viewport_widget_list",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| viewport_widget_list_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "viewport_widget_at_point",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| viewport_widget_at_point_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_wait",
            ModuleBinding::hidden("eguidev.wait"),
            async_host_fn(move |ctx: AsyncHostContext, args: ViewportPredicateArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let predicate = args.predicate.clone();
                    let predicate_ctx = ctx.clone();
                    let value = runtime
                        .wait_for_viewport_predicate(
                            pos,
                            options.as_ref().and_then(Value::as_object),
                            move |viewport| {
                                let predicate = predicate.clone();
                                let predicate_ctx = predicate_ctx.clone();
                                async move {
                                    predicate_matches(&predicate_ctx, &predicate, viewport).await
                                }
                            },
                        )
                        .await
                        .or_raise(&ctx)
                        .await?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        builder.json_call(
            "viewport_focus",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .focus_window(
                        pos,
                        args.receiver.id,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "viewport_settle",
            "eguidev.wait",
            &self.runtime,
            |runtime, pos, args: ViewportOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .wait_for_settle(pos, options.as_ref().and_then(Value::as_object))
                    .await
            },
        );
        builder.json_call(
            "viewport_dismiss_popups",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .viewport_dismiss_popups(
                        pos,
                        Some(args.receiver.id),
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "viewport_key",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportStringOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_key(pos, args.value, options.as_ref().and_then(Value::as_object))
                    .await
            },
        );
        builder.json_call(
            "viewport_paste",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportStringOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_paste(pos, args.value, options.as_ref().and_then(Value::as_object))
                    .await
            },
        );
        builder.json_call(
            "viewport_input",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportValueArgs| async move {
                runtime
                    .viewport_input(pos, &args.value, args.receiver.id)
                    .await
            },
        );
        builder.json_call(
            "viewport_resize",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportValueArgs| async move {
                runtime
                    .viewport_resize(pos, &args.value, args.receiver.id)
                    .await
            },
        );
        builder.json_call(
            "viewport_screenshot",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, args: ViewportValueArgs| async move {
                let target = serde_json::json!({ "viewport_id": args.receiver.id });
                runtime
                    .screenshot(pos, Some(&target), Some(&args.value))
                    .await
            },
        );
        builder.json_call(
            "viewport_native_screenshot",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, args: ViewportValueArgs| async move {
                runtime
                    .native_screenshot(pos, args.receiver.id, Some(&args.value))
                    .await
            },
        );
        builder.json_call(
            "viewport_sample_pixels",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, args: ViewportValueArgs| async move {
                runtime
                    .sample_pixels(pos, &args.value, Some(args.receiver.id))
                    .await
            },
        );
        builder.json_call(
            "viewport_layout_issues",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, viewport: ViewportReceiver| async move {
                runtime.check_layout(pos, Some(viewport.id)).await
            },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "viewport_show_highlight",
            ModuleBinding::hidden("eguidev.action"),
            async_host_fn(
                move |ctx: AsyncHostContext, args: ViewportValueStringArgs| {
                    let runtime = Arc::clone(&runtime);
                    async move {
                        let pos = script_position_from_context(&ctx).await?;
                        let rect = super::super::parse::parse_rect(&args.value)
                            .map_err(|error| type_error(error.message))
                            .or_raise(&ctx)
                            .await?;
                        let value = runtime
                            .show_highlight_rect(pos, Some(args.receiver.id), rect, args.text)
                            .await
                            .or_raise(&ctx)
                            .await?;
                        ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                            .await
                    }
                },
            ),
        );
        builder.json_call(
            "viewport_clear_highlights",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, viewport: ViewportReceiver| async move {
                runtime.clear_highlights(pos, Some(viewport.id)).await
            },
        );
        builder.json_call(
            "viewport_show_debug_overlay",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: ViewportOptionsArgs| async move {
                runtime
                    .show_debug_overlay(
                        pos,
                        Some(args.receiver.id),
                        args.options.as_ref().and_then(Value::as_object),
                        None,
                    )
                    .await
            },
        );
        builder.json_call(
            "viewport_clear_debug_overlay",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, viewport: ViewportReceiver| async move {
                runtime.clear_debug_overlay(pos, Some(viewport.id)).await
            },
        );
    }

    pub(super) fn register_widget_methods(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_viewport",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_viewport_host(&runtime, scope, args),
        );
        builder.json_call(
            "widget_click",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_click(
                        pos,
                        &args.receiver.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_hover",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_hover(
                        pos,
                        &args.receiver.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_type_text",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetTextOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_type(
                        pos,
                        &args.receiver.value,
                        args.text,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_focus",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_focus(
                        pos,
                        &args.receiver.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_set_value",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetValueOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .widget_set_value(
                        pos,
                        &args.receiver.value,
                        &args.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_drag_position",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetValueOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_drag(
                        pos,
                        &args.receiver.value,
                        &args.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_drag_relative",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetValueOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_drag_relative(
                        pos,
                        &args.receiver.value,
                        &args.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_drag_to",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetDragToArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_drag_to_widget(
                        pos,
                        &args.receiver.value,
                        &args.target.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_scroll",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetValueOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_scroll(
                        pos,
                        &args.receiver.value,
                        &args.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_scroll_to",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_scroll_to(
                        pos,
                        &args.receiver.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_scroll_into_view",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .action_scroll_into_view(
                        pos,
                        &args.receiver.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_text_measure",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, receiver: WidgetReceiver| async move {
                runtime.text_measure(pos, &receiver.value).await
            },
        );
        builder.json_call(
            "widget_layout_issues",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, receiver: WidgetReceiver| async move {
                runtime
                    .check_layout_widget(pos, &receiver.value, receiver.viewport_id)
                    .await
            },
        );
        builder.json_call(
            "widget_screenshot",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, args: WidgetValueArgs| async move {
                runtime
                    .screenshot(pos, Some(&args.receiver.value), Some(&args.value))
                    .await
            },
        );
        builder.json_call(
            "widget_sample_pixels",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, args: WidgetValueArgs| async move {
                runtime
                    .widget_sample_pixels(
                        pos,
                        &args.receiver.value,
                        args.receiver.viewport_id,
                        &args.value,
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_sample_grid",
            "eguidev.capture",
            &self.runtime,
            |runtime, pos, args: WidgetGridArgs| async move {
                runtime
                    .widget_sample_grid(
                        pos,
                        &args.receiver.value,
                        args.receiver.viewport_id,
                        &args.nx,
                        &args.ny,
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_show_highlight",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetStringArgs| async move {
                runtime
                    .show_highlight_widget(
                        pos,
                        &args.receiver.value,
                        args.receiver.viewport_id,
                        args.value,
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_clear_highlight",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, receiver: WidgetReceiver| async move {
                runtime
                    .clear_widget_highlight(pos, &receiver.value, receiver.viewport_id)
                    .await
            },
        );
        builder.json_call(
            "widget_show_debug_overlay",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                runtime
                    .show_debug_overlay(
                        pos,
                        None,
                        args.options.as_ref().and_then(Value::as_object),
                        Some(args.receiver.widget_ref()),
                    )
                    .await
            },
        );
        builder.json_call(
            "widget_clear_debug_overlay",
            "eguidev.action",
            &self.runtime,
            |runtime, pos, widget: WidgetReceiver| async move {
                runtime
                    .clear_widget_debug_overlay(pos, widget.widget_ref())
                    .await
            },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.async_function(
            "widget_wait",
            ModuleBinding::hidden("eguidev.wait"),
            async_host_fn(move |ctx: AsyncHostContext, args: WidgetPredicateArgs| {
                let runtime = Arc::clone(&runtime);
                async move {
                    let pos = script_position_from_context(&ctx).await?;
                    let options = args.options_with_viewport();
                    let predicate = args.predicate.clone();
                    let predicate_ctx = ctx.clone();
                    let value = runtime
                        .wait_for_widget_predicate(
                            pos,
                            &args.receiver.value,
                            options.as_ref().and_then(Value::as_object),
                            &args.condition,
                            move |widget| {
                                let predicate = predicate.clone();
                                let predicate_ctx = predicate_ctx.clone();
                                async move {
                                    predicate_matches(&predicate_ctx, &predicate, widget).await
                                }
                            },
                        )
                        .await
                        .or_raise(&ctx)
                        .await?;
                    ctx.json_host_return_with_options(value, JsonDecodeOptions::typed())
                        .await
                }
            }),
        );
        builder.json_call(
            "widget_wait_absent",
            "eguidev.wait",
            &self.runtime,
            |runtime, pos, args: WidgetOptionsArgs| async move {
                let options = args.options_with_viewport();
                runtime
                    .wait_for_widget_absent(
                        pos,
                        &args.receiver.value,
                        options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_state",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_state_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_parent",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_parent_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "widget_children",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| widget_children_host(&runtime, scope, args),
        );
    }

    pub(super) fn register_script_utility_globals(&self, builder: &mut DeclaredModuleBuilder<'_>) {
        builder.unit_call(
            "capture",
            "eguidev.wait",
            &self.runtime,
            |runtime, pos, options: OptionalJsonArg| async move {
                runtime
                    .wait_for_fresh_capture(pos, options.0.as_ref().and_then(Value::as_object))
                    .await
            },
        );
        builder.json_call(
            "frames",
            "eguidev.wait",
            &self.runtime,
            |runtime, pos, args: FrameCountOptionsArgs| async move {
                runtime
                    .wait_for_frames(
                        pos,
                        args.count,
                        args.options.as_ref().and_then(Value::as_object),
                    )
                    .await
            },
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "log",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, args| log_host(&runtime, scope, args),
        );
        let runtime = Arc::clone(&self.runtime);
        builder.borrowed_function(
            "capture",
            ModuleBinding::hidden("eguidev.query"),
            move |scope, args| capture_host(&runtime, scope, &args),
        );
        let args = self.args.clone();
        builder.borrowed_function(
            "args",
            ModuleBinding::hidden("eguidev.record"),
            move |scope, values| args_host(&args, scope, &values),
        );
        builder.borrowed_function(
            "array",
            ModuleBinding::hidden("eguidev.record"),
            frozen_array_host,
        );
    }
}
