//! Demo app exercising the DevMCP scripting surface.

use std::{env, error::Error, io, path::PathBuf};

use eframe::{App, egui};
use egui::{Color32, ColorImage, TextureHandle, TextureOptions, scroll_area::ScrollBarVisibility};
#[cfg(feature = "devtools")]
use eguidev::runtime;
use eguidev::{
    ButtonOptions, CheckboxOptions, DevMcp, DevScrollAreaExt, DevUiExt, FixtureSpec, FrameGuard,
    ProgressBarOptions, ScrollAreaState, TextEditOptions, WidgetRole,
};

/// Shared result type for the demo binary entry point.
type MainResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Fixture catalog for the demo app.
fn demo_fixtures() -> Vec<FixtureSpec> {
    vec![
        FixtureSpec {
            name: "basic.default".to_string(),
            description: "Reset to the initial demo state.".to_string(),
        },
        FixtureSpec {
            name: "basic.empty".to_string(),
            description: "Clear inputs, disable toggle, and reset intensity.".to_string(),
        },
        FixtureSpec {
            name: "basic.scrolled".to_string(),
            description: "Jump the scroll area down to a later row.".to_string(),
        },
        FixtureSpec {
            name: "basic.overlay_reset_probe".to_string(),
            description: "Reset probe for overlay-local fixture input.".to_string(),
        },
        FixtureSpec {
            name: "viewports.default".to_string(),
            description: "Reset the secondary viewport to its default state.".to_string(),
        },
        FixtureSpec {
            name: "viewports.scrolled".to_string(),
            description: "Jump the secondary viewport list down to a later row.".to_string(),
        },
    ]
}

/// Build the demo's DevMCP handle, optionally attaching the embedded runtime.
fn build_devmcp(config: AppConfig) -> MainResult<DevMcp> {
    let devmcp = DevMcp::new().fixtures(demo_fixtures());
    #[cfg(feature = "devtools")]
    {
        if config.enable_mcp {
            return Ok(runtime::attach(devmcp));
        }
        Ok(devmcp)
    }
    #[cfg(not(feature = "devtools"))]
    {
        if config.enable_mcp {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--dev-mcp requires building the demo with --features devtools",
            )
            .into());
        }
        Ok(devmcp)
    }
}

/// Launch the demo app.
fn main() -> MainResult<()> {
    let config = AppConfig::from_env()?;
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 900.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Egui DevMCP Demo",
        options,
        Box::new(move |cc| Ok(Box::new(DemoApp::new(config, &cc.egui_ctx)?))),
    )
    .map_err(|error| io::Error::other(error.to_string()))?;

    Ok(())
}

#[derive(Debug, Clone, Copy)]
/// Parsed configuration for the demo app.
struct AppConfig {
    /// Whether DevMCP is enabled for this run.
    enable_mcp: bool,
}

impl AppConfig {
    /// Load configuration from process args.
    fn from_env() -> MainResult<Self> {
        let mut enable_mcp = false;
        let args = env::args_os().skip(1);

        for arg in args {
            if arg == "--dev-mcp" {
                enable_mcp = true;
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown argument: {}", PathBuf::from(arg).display()),
            )
            .into());
        }

        Ok(Self { enable_mcp })
    }

    #[cfg(test)]
    fn test(enable_mcp: bool) -> Self {
        Self { enable_mcp }
    }
}

#[derive(Debug, Clone, Copy)]
/// Snapshot of the last key event observed.
struct KeyEventSnapshot {
    /// Logical key for the event.
    key: egui::Key,
    /// Whether the key was pressed.
    pressed: bool,
    /// Modifiers active during the event.
    modifiers: egui::Modifiers,
    /// Whether the event was a key repeat.
    repeat: bool,
}

/// Demo radio-mode selection used to exercise stateful widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemoMode {
    /// Primary mode option.
    Alpha,
    /// Secondary mode option.
    Beta,
}

/// Application state for the demo app.
struct DemoApp {
    /// DevMCP integration handle.
    devmcp: DevMcp,
    /// Name field.
    name: String,
    /// Notes field.
    notes: String,
    /// Toggle state.
    enabled: bool,
    /// Slider value.
    intensity: f32,
    /// Combo-box choice.
    choice_index: usize,
    /// Ranged float drag value.
    drag_float: f32,
    /// Ranged integer drag value.
    drag_int: i32,
    /// Selected-aware toolbar button state.
    toolbar_selected: bool,
    /// Toggle widget state.
    feature_toggle: bool,
    /// Third-state checkbox value.
    mixed_value: bool,
    /// Third-state checkbox visual flag.
    mixed_indeterminate: bool,
    /// Password field contents.
    password: String,
    /// Radio-group selection.
    mode: DemoMode,
    /// Selectable-row selection.
    selected_item: usize,
    /// Demo accent color.
    accent_color: Color32,
    /// Count of submit presses.
    click_count: u32,
    /// Status text shown in the UI.
    status: String,
    /// Whether the advanced panel is expanded.
    advanced_open: bool,
    /// Whether the input diagnostics section is expanded.
    input_debug_open: bool,
    /// Number of menu actions triggered.
    menu_action_count: u32,
    /// Number of link interactions observed.
    link_click_count: u32,
    /// Probe input used by fixture reset tests for overlay-like state.
    overlay_probe_input: String,
    /// Preview texture shown in the demo.
    preview_texture: TextureHandle,
    /// Scroll state for the primary scroll demo.
    basic_scroll_state: ScrollAreaState,
    /// Whether the secondary viewport is visible.
    show_secondary: bool,
    /// Selected row index from the secondary viewport list.
    secondary_selected_row: usize,
    /// Accumulated drag offset for the secondary viewport drag region.
    secondary_drag_offset: egui::Vec2,
    /// Scroll state for the secondary viewport list.
    secondary_scroll_state: ScrollAreaState,
    /// Last observed raw scroll delta.
    last_raw_scroll: egui::Vec2,
    /// Last observed smooth scroll delta.
    last_smooth_scroll: egui::Vec2,
    /// Last observed pointer position.
    last_pointer_pos: Option<egui::Pos2>,
    /// Number of scroll events seen in the last frame.
    last_scroll_event_count: usize,
    /// Number of input events seen in the last frame.
    last_event_count: usize,
    /// Last observed key event.
    last_key_event: Option<KeyEventSnapshot>,
    /// Last observed input modifiers.
    last_modifiers: egui::Modifiers,
    /// Accumulated drag offset for the root viewport drag region.
    root_drag_offset: egui::Vec2,
    /// Whether the floating demo window is open.
    widget_window_open: bool,
}

impl DemoApp {
    /// Build a new demo app from the parsed configuration.
    fn new(config: AppConfig, ctx: &egui::Context) -> MainResult<Self> {
        let devmcp = build_devmcp(config)?;
        let preview_texture = ctx.load_texture(
            "eguidev_demo.preview",
            ColorImage::filled([16, 16], Color32::from_rgb(64, 156, 255)),
            TextureOptions::LINEAR,
        );
        Ok(Self {
            devmcp,
            name: "Sky".to_string(),
            notes: "Try typing here.".to_string(),
            enabled: true,
            intensity: 42.0,
            choice_index: 1,
            drag_float: 1.5,
            drag_int: 7,
            toolbar_selected: true,
            feature_toggle: false,
            mixed_value: true,
            mixed_indeterminate: true,
            password: "opensesame".to_string(),
            mode: DemoMode::Alpha,
            selected_item: 1,
            accent_color: Color32::from_rgba_unmultiplied(64, 156, 255, 255),
            click_count: 0,
            status: "Waiting for input.".to_string(),
            advanced_open: false,
            input_debug_open: false,
            menu_action_count: 0,
            link_click_count: 0,
            overlay_probe_input: String::new(),
            preview_texture,
            basic_scroll_state: ScrollAreaState::default(),
            show_secondary: true,
            secondary_selected_row: 0,
            secondary_drag_offset: egui::Vec2::ZERO,
            secondary_scroll_state: ScrollAreaState::default(),
            last_raw_scroll: egui::Vec2::ZERO,
            last_smooth_scroll: egui::Vec2::ZERO,
            last_pointer_pos: None,
            last_scroll_event_count: 0,
            last_event_count: 0,
            last_key_event: None,
            last_modifiers: egui::Modifiers::default(),
            root_drag_offset: egui::Vec2::ZERO,
            widget_window_open: true,
        })
    }

    /// Reset the demo state to its initial values.
    fn reset_state(&mut self) {
        self.name = "Sky".to_string();
        self.notes = "Try typing here.".to_string();
        self.enabled = true;
        self.intensity = 42.0;
        self.choice_index = 1;
        self.drag_float = 1.5;
        self.drag_int = 7;
        self.toolbar_selected = true;
        self.feature_toggle = false;
        self.mixed_value = true;
        self.mixed_indeterminate = true;
        self.password = "opensesame".to_string();
        self.mode = DemoMode::Alpha;
        self.selected_item = 1;
        self.accent_color = Color32::from_rgba_unmultiplied(64, 156, 255, 255);
        self.click_count = 0;
        self.status = "Waiting for input.".to_string();
        self.advanced_open = false;
        self.input_debug_open = false;
        self.menu_action_count = 0;
        self.link_click_count = 0;
        self.overlay_probe_input.clear();
        self.basic_scroll_state.reset();
        self.show_secondary = true;
        self.secondary_selected_row = 0;
        self.secondary_drag_offset = egui::Vec2::ZERO;
        self.secondary_scroll_state.reset();
        self.last_raw_scroll = egui::Vec2::ZERO;
        self.last_smooth_scroll = egui::Vec2::ZERO;
        self.last_pointer_pos = None;
        self.last_scroll_event_count = 0;
        self.last_event_count = 0;
        self.last_key_event = None;
        self.last_modifiers = egui::Modifiers::default();
        self.root_drag_offset = egui::Vec2::ZERO;
        self.widget_window_open = true;
    }

    /// Apply a named fixture to the demo state.
    fn apply_fixture(&mut self, name: &str) -> Result<(), String> {
        self.reset_state();
        match name {
            "basic.default" => Ok(()),
            "basic.empty" => {
                self.name.clear();
                self.notes.clear();
                self.enabled = false;
                self.intensity = 0.0;
                self.choice_index = 0;
                self.drag_float = -2.5;
                self.drag_int = -1;
                self.toolbar_selected = false;
                self.feature_toggle = true;
                self.mixed_value = false;
                self.mixed_indeterminate = false;
                self.password.clear();
                self.mode = DemoMode::Beta;
                self.selected_item = 2;
                self.accent_color = Color32::from_rgba_unmultiplied(224, 96, 96, 255);
                self.status = "Fixture: empty".to_string();
                Ok(())
            }
            "basic.scrolled" => {
                self.basic_scroll_state.jump_to(egui::vec2(0.0, 300.0));
                self.status = "Fixture: scrolled".to_string();
                Ok(())
            }
            "basic.overlay_reset_probe" => {
                self.status = "Fixture: overlay reset probe".to_string();
                Ok(())
            }
            "viewports.default" => Ok(()),
            "viewports.scrolled" => {
                self.secondary_scroll_state.jump_to(egui::vec2(0.0, 300.0));
                self.status = "Fixture: secondary viewport scrolled".to_string();
                Ok(())
            }
            _ => Err(format!("unknown fixture: {name}")),
        }
    }

    /// Render the root UI for the demo.
    fn render_root(&mut self, ui: &mut egui::Ui) {
        eguidev::container(ui, "basic.panel", |ui| {
            ui.heading("DevMCP basics");
            ui.label("Tagged widgets are available to MCP tools.");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Name");
                ui.dev_text_edit("basic.name", &mut self.name);
            });

            ui.label("Notes");
            ui.dev_text_edit_multiline("basic.notes", &mut self.notes);

            ui.dev_checkbox("basic.enabled", &mut self.enabled, "Enabled");
            ui.dev_slider("basic.intensity", &mut self.intensity, 0.0..=100.0);
            ui.horizontal(|ui| {
                ui.label("Overlay probe");
                ui.dev_text_edit("overlay.fixture_probe.input", &mut self.overlay_probe_input);
            });

            if ui.dev_button("basic.submit", "Submit").clicked() {
                self.click_count += 1;
                self.status = format!(
                    "Saved {name} with intensity {intensity:.1} (click {count}).",
                    name = self.name,
                    intensity = self.intensity,
                    count = self.click_count
                );
            }
            ui.dev_label("basic.status", &self.status);

            ui.separator();
            ui.label("Root Draggable Region:");
            let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 48.0), egui::Sense::hover());
            let drag_rect = rect.translate(self.root_drag_offset);
            let response = ui.interact(
                drag_rect,
                egui::Id::new("basic.drag.region"),
                egui::Sense::drag(),
            );
            if response.dragged() {
                self.root_drag_offset += response.drag_delta();
            }
            ui.painter()
                .rect_filled(drag_rect, 6.0, egui::Color32::from_rgb(128, 32, 96));
            ui.painter().text(
                drag_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drag me (root)",
                egui::FontId::proportional(16.0),
                egui::Color32::WHITE,
            );
            eguidev::track_response_full(
                "basic.drag",
                &response,
                eguidev::WidgetMeta {
                    role: WidgetRole::Unknown,
                    label: Some("root drag region".to_string()),
                    rect: Some(drag_rect),
                    interact_rect: Some(drag_rect),
                    visible: true,
                    ..Default::default()
                },
            );
            ui.dev_label(
                "basic.drag.detail",
                format!(
                    "Root drag offset: {:.1}, {:.1}",
                    self.root_drag_offset.x, self.root_drag_offset.y
                ),
            );

            ui.dev_separator("basic.separator.primary");
            ui.horizontal(|ui| {
                if ui.dev_link("basic.link.docs", "Open docs").clicked() {
                    self.link_click_count += 1;
                    self.status = format!("Docs link clicked {} time(s).", self.link_click_count);
                }
                if ui
                    .dev_hyperlink_to(
                        "basic.link.reference",
                        "Reference",
                        "https://example.invalid/reference",
                    )
                    .clicked()
                {
                    self.link_click_count += 1;
                    self.status =
                        format!("Reference link clicked {} time(s).", self.link_click_count);
                }
            });
            ui.horizontal(|ui| {
                ui.dev_label(
                    "basic.links.count",
                    format!("Link clicks: {}", self.link_click_count),
                );
                ui.dev_image(
                    "basic.preview.image",
                    "Preview swatch",
                    &self.preview_texture,
                );
                ui.dev_spinner("basic.spinner.loading");
            });

            let _menu = ui.dev_menu_button("basic.menu.actions", "Actions", |ui| {
                if ui
                    .dev_button("basic.menu.actions.reset_status", "Reset status")
                    .clicked()
                {
                    self.menu_action_count += 1;
                    self.status = format!("Menu reset clicked {} time(s).", self.menu_action_count);
                    ui.close();
                }
                if ui
                    .dev_button("basic.menu.actions.mark_ready", "Mark ready")
                    .clicked()
                {
                    self.menu_action_count += 1;
                    self.status = format!("Menu ready clicked {} time(s).", self.menu_action_count);
                    ui.close();
                }
            });
            ui.dev_label(
                "basic.menu.count",
                format!("Menu actions: {}", self.menu_action_count),
            );

            let _advanced = ui.dev_collapsing(
                "basic.advanced",
                &mut self.advanced_open,
                "Advanced tools",
                |ui| {
                    ui.dev_label(
                        "basic.advanced.summary",
                        "This section is visible when expanded.",
                    );
                    if ui
                        .dev_button("basic.advanced.action", "Advanced action")
                        .clicked()
                    {
                        self.status = "Advanced action clicked.".to_string();
                    }
                },
            );

            let _input_debug = ui.dev_collapsing(
                "basic.input.debug",
                &mut self.input_debug_open,
                "Input diagnostics",
                |ui| {
                    ui.dev_label(
                        "basic.input.raw_scroll",
                        format!(
                            "Raw scroll: {:.1}, {:.1}",
                            self.last_raw_scroll.x, self.last_raw_scroll.y
                        ),
                    );
                    ui.dev_label(
                        "basic.input.smooth_scroll",
                        format!(
                            "Smooth scroll: {:.1}, {:.1}",
                            self.last_smooth_scroll.x, self.last_smooth_scroll.y
                        ),
                    );
                    ui.dev_label(
                        "basic.input.pointer",
                        format!(
                            "Pointer: {}",
                            self.last_pointer_pos
                                .map(|pos| format!("{:.1}, {:.1}", pos.x, pos.y))
                                .unwrap_or_else(|| "none".to_string())
                        ),
                    );
                    ui.dev_label(
                        "basic.input.scroll_events",
                        format!("Scroll events: {}", self.last_scroll_event_count),
                    );
                    ui.dev_label(
                        "basic.input.events",
                        format!("Input events: {}", self.last_event_count),
                    );
                    let key_event = self
                        .last_key_event
                        .map(|event| {
                            format!(
                                "Key: {:?} ({}) repeat={} mods: {}",
                                event.key,
                                if event.pressed { "pressed" } else { "released" },
                                event.repeat,
                                format_modifiers(event.modifiers)
                            )
                        })
                        .unwrap_or_else(|| "Key: none".to_string());
                    ui.dev_label("basic.input.key_event", key_event);
                    ui.dev_label(
                        "basic.input.modifiers",
                        format!("Modifiers: {}", format_modifiers(self.last_modifiers)),
                    );
                },
            );

            let _root = egui::ScrollArea::vertical()
                .id_salt("basic.root_scroll")
                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                .dev_show(ui, "basic.root_scroll", |ui| {
                    ui.dev_separator("basic.separator.scroll");
                    ui.label("Scroll area");
                    ui.horizontal(|ui| {
                        if ui
                            .dev_button("basic.scroll.jump_top", "Scroll to top")
                            .clicked()
                        {
                            self.basic_scroll_state.jump_to(egui::Vec2::ZERO);
                        }
                        if ui
                            .dev_button("basic.scroll.jump_down", "Jump down")
                            .clicked()
                        {
                            self.basic_scroll_state.jump_to(egui::vec2(0.0, 300.0));
                        }
                        ui.dev_label(
                            "basic.scroll.offset",
                            format!("Scroll offset: {:.1}", self.basic_scroll_state.offset().y),
                        );
                    });
                    let _output = self.basic_scroll_state.show(
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible),
                        ui,
                        "basic.scroll",
                        |ui| {
                            for row in 0..50 {
                                ui.dev_label(
                                    format!("basic.scroll.row.{row}"),
                                    format!("Row {row}"),
                                );
                            }
                        },
                    );

                    ui.dev_separator("viewports.separator.primary");
                    ui.heading("Viewport playground");
                    ui.label(
                        "The secondary viewport stays open by default so viewport tooling is always live.",
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .dev_button("viewports.toggle", "Toggle secondary viewport")
                            .clicked()
                        {
                            self.show_secondary = !self.show_secondary;
                            self.status = if self.show_secondary {
                                "Secondary viewport opened.".to_string()
                            } else {
                                "Secondary viewport hidden.".to_string()
                            };
                        }
                        ui.dev_label(
                            "viewports.open",
                            format!("Secondary open: {}", self.show_secondary),
                        );
                    });
                    ui.dev_label(
                        "viewports.selected_row",
                        format!("Selected row: {}", self.secondary_selected_row),
                    );
                    ui.dev_label(
                        "viewports.scroll.offset",
                        format!(
                            "Secondary scroll offset: {:.1}",
                            self.secondary_scroll_state.offset().y
                        ),
                    );
                    ui.dev_label(
                        "viewports.drag.offset",
                        format!(
                            "Drag offset: {:.1}, {:.1}",
                            self.secondary_drag_offset.x, self.secondary_drag_offset.y
                        ),
                    );
                });
        });
    }

    /// Render the floating window that exercises the remaining widget roles.
    fn render_widget_surface_window(&mut self, ui: &mut egui::Ui) {
        eguidev::container(ui, "basic.window.surface.body", |ui| {
            ui.dev_label("basic.window.surface.label", "Floating window ready.");
            ui.dev_combo_box(
                "basic.choice",
                "Choice",
                &mut self.choice_index,
                &["Alpha", "Beta", "Gamma"],
            );
            ui.horizontal(|ui| {
                ui.label("Drag values");
                ui.dev_drag_value_range("basic.drag.float", &mut self.drag_float, -10.0..=10.0);
                ui.dev_drag_value_i32_range("basic.drag.int", &mut self.drag_int, -5..=20);
            });
            ui.horizontal(|ui| {
                if ui
                    .dev_button_with(
                        "basic.toolbar.sync",
                        "Toolbar sync",
                        ButtonOptions {
                            selected: self.toolbar_selected,
                        },
                    )
                    .clicked()
                {
                    self.toolbar_selected = !self.toolbar_selected;
                    self.status = format!("Toolbar sync set to {}.", self.toolbar_selected);
                }
                ui.dev_toggle_value("basic.toggle", &mut self.feature_toggle, "Feature toggle");
                ui.dev_checkbox_with(
                    "basic.mixed",
                    &mut self.mixed_value,
                    "Mixed mode",
                    CheckboxOptions {
                        indeterminate: self.mixed_indeterminate,
                    },
                );
            });
            ui.horizontal(|ui| {
                ui.label("Password");
                ui.dev_text_edit_with(
                    "basic.password",
                    &mut self.password,
                    TextEditOptions {
                        multiline: false,
                        password: true,
                    },
                );
            });
            ui.horizontal(|ui| {
                ui.dev_radio_value("basic.mode.alpha", &mut self.mode, DemoMode::Alpha, "Alpha");
                ui.dev_radio_value("basic.mode.beta", &mut self.mode, DemoMode::Beta, "Beta");
            });
            ui.horizontal(|ui| {
                ui.dev_selectable_value("basic.select.0", &mut self.selected_item, 0, "Item 0");
                ui.dev_selectable_value("basic.select.1", &mut self.selected_item, 1, "Item 1");
                ui.dev_selectable_value("basic.select.2", &mut self.selected_item, 2, "Item 2");
            });
            ui.dev_progress_bar_with(
                "basic.progress.percent",
                self.intensity / 100.0,
                ProgressBarOptions {
                    text: None,
                    show_percentage: true,
                },
            );
            ui.dev_progress_bar_with(
                "basic.progress.detail",
                ((self.drag_float + 10.0) / 20.0).clamp(0.0, 1.0),
                ProgressBarOptions {
                    text: Some(format!("Drag {:.1}", self.drag_float)),
                    show_percentage: false,
                },
            );
            ui.horizontal(|ui| {
                ui.label("Accent");
                ui.dev_color_edit("basic.accent", &mut self.accent_color);
                ui.dev_label(
                    "basic.accent.value",
                    format!("Accent: {}", format_color(self.accent_color)),
                );
            });
        });
    }

    /// Render contents inside the secondary viewport.
    fn render_secondary(&mut self, ui: &mut egui::Ui, class: egui::ViewportClass) {
        let devmcp = self.devmcp.clone();
        let ctx = ui.ctx().clone();
        if devmcp.is_enabled() {
            ctx.input_mut(|i| eguidev::raw_input_hook(&devmcp, &ctx, &mut i.raw));
        }
        let _guard = FrameGuard::new(&devmcp, &ctx);

        if ui.ctx().input(|i| i.viewport().close_requested()) {
            self.show_secondary = false;
        }

        let title = match class {
            egui::ViewportClass::EmbeddedWindow => "Secondary viewport (embedded)",
            _ => "Secondary viewport",
        };
        ui.heading(title);
        ui.label("Scroll and drag inside this viewport.");

        let _output = self.secondary_scroll_state.show(
            egui::ScrollArea::vertical()
                .id_salt("secondary_scroll")
                .max_height(240.0),
            ui,
            "viewports.scroll",
            |ui| {
                for row in 0..25 {
                    let label = format!("Row {row}");
                    let response = ui.dev_button(format!("viewports.row.{row}"), label);
                    if response.clicked() {
                        self.secondary_selected_row = row;
                        self.status = format!("Secondary row {row} selected.");
                    }
                }
            },
        );

        ui.separator();
        ui.dev_label(
            "viewports.selected_row.detail",
            format!("Selected row: {}", self.secondary_selected_row),
        );

        ui.separator();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 48.0), egui::Sense::hover());
        let drag_rect = rect.translate(self.secondary_drag_offset);
        let response = ui.interact(
            drag_rect,
            egui::Id::new("secondary_drag_region"),
            egui::Sense::drag(),
        );
        if response.dragged() {
            self.secondary_drag_offset += response.drag_delta();
        }
        ui.painter()
            .rect_filled(drag_rect, 6.0, egui::Color32::from_rgb(32, 128, 96));
        ui.painter().text(
            drag_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Drag me",
            egui::FontId::proportional(16.0),
            egui::Color32::WHITE,
        );
        eguidev::track_response_full(
            "viewports.drag",
            &response,
            eguidev::WidgetMeta {
                role: WidgetRole::Unknown,
                label: Some("drag region".to_string()),
                rect: Some(drag_rect),
                interact_rect: Some(drag_rect),
                visible: true,
                ..Default::default()
            },
        );
        ui.dev_label(
            "viewports.drag.detail",
            format!(
                "Drag offset: {:.1}, {:.1}",
                self.secondary_drag_offset.x, self.secondary_drag_offset.y
            ),
        );
    }
}

impl App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for request in self.devmcp.collect_fixture_requests() {
            let result = self.apply_fixture(&request.name);
            if !request.respond(result) {
                eprintln!("eguidev_demo: fixture response channel closed");
            }
        }
        if self.show_secondary && ctx.viewport_id() == egui::ViewportId::ROOT {
            let viewport_id = egui::ViewportId::from_hash_of("eguidev_demo.secondary");
            let builder = egui::ViewportBuilder::default()
                .with_title("DevMCP secondary viewport")
                .with_inner_size([480.0, 420.0]);
            ctx.show_viewport_immediate(viewport_id, builder, |ui, class| {
                self.render_secondary(ui, class)
            });
        }
        let (
            raw_scroll,
            smooth_scroll,
            pointer_pos,
            scroll_events,
            event_count,
            key_event,
            modifiers,
        ) = ctx.input(|i| {
            let scroll_events = i
                .events
                .iter()
                .filter(|event| matches!(event, egui::Event::MouseWheel { .. }))
                .count();
            let key_event = i.events.iter().rev().find_map(|event| {
                if let egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    repeat,
                    ..
                } = event
                {
                    Some(KeyEventSnapshot {
                        key: *key,
                        pressed: *pressed,
                        modifiers: *modifiers,
                        repeat: *repeat,
                    })
                } else {
                    None
                }
            });
            let raw_scroll = i
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel { delta, .. } => Some(*delta),
                    _ => None,
                })
                .fold(egui::Vec2::ZERO, |sum, delta| sum + delta);
            (
                raw_scroll,
                i.smooth_scroll_delta(),
                i.pointer.interact_pos(),
                scroll_events,
                i.events.len(),
                key_event,
                i.modifiers,
            )
        });
        let saw_scroll_activity = scroll_events > 0
            || raw_scroll != egui::Vec2::ZERO
            || smooth_scroll != egui::Vec2::ZERO;
        if saw_scroll_activity {
            self.last_raw_scroll = raw_scroll;
            self.last_smooth_scroll = smooth_scroll;
            self.last_scroll_event_count = scroll_events.max(1);
        }
        self.last_pointer_pos = pointer_pos;
        self.last_event_count = event_count;
        if let Some(key_event) = key_event {
            self.last_key_event = Some(key_event);
        }
        self.last_modifiers = modifiers;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let devmcp = self.devmcp.clone();
        let ctx = ui.ctx().clone();
        let _guard = FrameGuard::new(&devmcp, &ctx);
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            self.render_root(ui);
        });
        let mut open = self.widget_window_open;
        if let Some(window) = egui::Window::new("Widget Surface Window")
            .id(egui::Id::new("basic.window.surface"))
            .open(&mut open)
            .default_pos(egui::pos2(540.0, 24.0))
            .default_size(egui::vec2(220.0, 320.0))
            .show(&ctx, |ui| {
                self.render_widget_surface_window(ui);
            })
        {
            eguidev::track_response_full(
                "basic.window.surface",
                &window.response,
                eguidev::WidgetMeta {
                    role: WidgetRole::Window,
                    label: Some("Widget Surface Window".to_string()),
                    visible: true,
                    ..Default::default()
                },
            );
        }
        self.widget_window_open = open;
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        eguidev::raw_input_hook(&self.devmcp, ctx, raw_input);
    }
}

/// Render modifiers for display in the debug UI.
fn format_modifiers(modifiers: egui::Modifiers) -> String {
    format!(
        "cmd={} mac_cmd={} ctrl={} shift={} alt={}",
        modifiers.command, modifiers.mac_cmd, modifiers.ctrl, modifiers.shift, modifiers.alt
    )
}

/// Format a color as the scripting-facing `#RRGGBBAA` string.
fn format_color(color: Color32) -> String {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_reset_probe_fixture_is_idempotent_for_overlay_input() {
        let mut app =
            DemoApp::new(AppConfig::test(false), &egui::Context::default()).expect("demo app");

        app.overlay_probe_input = "first".to_string();
        app.apply_fixture("basic.overlay_reset_probe")
            .expect("apply fixture");
        assert!(app.overlay_probe_input.is_empty());

        app.overlay_probe_input = "second".to_string();
        app.apply_fixture("basic.overlay_reset_probe")
            .expect("apply fixture");
        assert!(app.overlay_probe_input.is_empty());
    }

    #[test]
    fn overlay_reset_probe_fixture_is_listed() {
        let fixtures = demo_fixtures();
        assert!(
            fixtures
                .iter()
                .find(|fixture| fixture.name == "basic.overlay_reset_probe")
                .is_some()
        );
    }

    #[test]
    fn viewport_fixture_keeps_secondary_viewport_available() {
        let mut app =
            DemoApp::new(AppConfig::test(false), &egui::Context::default()).expect("demo app");
        app.show_secondary = false;
        app.secondary_selected_row = 9;

        app.apply_fixture("viewports.default")
            .expect("apply fixture");

        assert!(app.show_secondary);
        assert_eq!(app.secondary_selected_row, 0);
    }
}
