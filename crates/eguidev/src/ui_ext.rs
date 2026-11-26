//! Helper extensions for recording common widgets with explicit ids.

use std::ops::RangeInclusive;

use egui::{collapsing_header::CollapsingResponse, scroll_area::ScrollAreaOutput};

use crate::{
    instrument::{active_inner, container, record_widget_with_layout},
    overlay::parse_css_hex,
    registry::Inner,
    types::{WidgetRange, WidgetRole, WidgetRoleMeta, WidgetValue},
};

/// Options for selected-aware buttons.
#[derive(Debug, Clone, Copy, Default)]
pub struct ButtonOptions {
    /// Whether the button is in a selected/toggled state.
    pub selected: bool,
}

/// Options for third-state checkboxes.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckboxOptions {
    /// Whether the checkbox should render as indeterminate.
    pub indeterminate: bool,
}

/// Options for text-edit widgets.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextEditOptions {
    /// Whether the edit is multiline.
    pub multiline: bool,
    /// Whether the edit masks its contents in the UI.
    ///
    /// Masking is presentation only. Unless `omit_value` is set, automation
    /// still records the cleartext value in widget state, dumps, script
    /// output, and failure bundles.
    pub password: bool,
    /// Whether automation leaves out the value of the field.
    ///
    /// The field then records no `WidgetValue`, so widget state, dumps,
    /// script output, and failure bundles never hold its text. A script can
    /// still focus the field and type into it. Set it for a field that takes
    /// a secret, such as a password or an API token.
    pub omit_value: bool,
}

impl TextEditOptions {
    /// Returns the role metadata of a text edit with these options.
    ///
    /// A custom text edit that records itself through `track_response` uses
    /// it with [`Self::recorded_value`], so it follows the same rules as
    /// [`DevUiExt::dev_text_edit_with`].
    pub fn role(self) -> WidgetRoleMeta {
        WidgetRoleMeta::TextEdit {
            multiline: self.multiline,
            password: self.password,
        }
    }

    /// Returns the value that automation records for `text`: the text, or
    /// nothing when `omit_value` is set.
    pub fn recorded_value(self, text: &str) -> Option<WidgetValue> {
        (!self.omit_value).then(|| WidgetValue::Text(text.to_owned()))
    }

    /// Build the egui text edit that these options describe.
    fn builder(self, text: &mut String) -> egui::TextEdit<'_> {
        if self.multiline {
            egui::TextEdit::multiline(text)
        } else {
            egui::TextEdit::singleline(text)
        }
        .password(self.password)
    }
}

/// Options for progress bars.
#[derive(Debug, Clone, Default)]
pub struct ProgressBarOptions {
    /// Optional overlay text rendered inside the bar.
    pub text: Option<String>,
    /// Whether to show a percentage when `text` is absent.
    pub show_percentage: bool,
}

/// Helper extensions for recording common widgets with explicit ids.
///
/// Prefer these for standard widgets; they auto-populate role/type/value/label
/// metadata.
pub trait DevUiExt {
    /// Add a button with an explicit id.
    #[must_use = "a button does nothing unless its response is handled"]
    fn dev_button(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add a selected-aware button with explicit metadata.
    #[must_use = "a button does nothing unless its response is handled"]
    fn dev_button_with(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
        options: ButtonOptions,
    ) -> egui::Response;

    /// Add a link with an explicit id.
    fn dev_link(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add a hyperlink with an explicit id, using its URL as label and value
    /// metadata.
    fn dev_hyperlink(&mut self, id: impl Into<String>, url: impl ToString) -> egui::Response;

    /// Add a hyperlink with an explicit id, label, and URL metadata.
    fn dev_hyperlink_to(
        &mut self,
        id: impl Into<String>,
        label: impl Into<egui::WidgetText>,
        url: impl ToString,
    ) -> egui::Response;

    /// Add an image with an explicit id and developer-provided description.
    fn dev_image<'a>(
        &mut self,
        id: impl Into<String>,
        description: impl Into<String>,
        source: impl Into<egui::ImageSource<'a>>,
    ) -> egui::Response;

    /// Add a label with an explicit id.
    fn dev_label(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add a checkbox with an explicit id.
    fn dev_checkbox(
        &mut self,
        id: impl Into<String>,
        value: &mut bool,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add an indeterminate-aware checkbox with explicit metadata.
    fn dev_checkbox_with(
        &mut self,
        id: impl Into<String>,
        value: &mut bool,
        text: impl Into<egui::WidgetText>,
        options: CheckboxOptions,
    ) -> egui::Response;

    /// Add a text edit with an explicit id.
    fn dev_text_edit(&mut self, id: impl Into<String>, text: &mut String) -> egui::Response;

    /// Add a text edit with explicit mode metadata.
    ///
    /// `options.password` masks the field in the UI only. The recorded value
    /// stays cleartext for scripts, dumps, and failure bundles, unless
    /// `options.omit_value` leaves it out.
    fn dev_text_edit_with(
        &mut self,
        id: impl Into<String>,
        text: &mut String,
        options: TextEditOptions,
    ) -> egui::Response;

    /// Add a slider with an explicit id.
    fn dev_slider(
        &mut self,
        id: impl Into<String>,
        value: &mut f32,
        range: RangeInclusive<f32>,
    ) -> egui::Response;

    /// Add a combo box with an explicit id.
    fn dev_combo_box<T: ToString>(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        selected: &mut usize,
        options: &[T],
    ) -> egui::Response;

    /// Add a float drag value with an explicit id.
    fn dev_drag_value(&mut self, id: impl Into<String>, value: &mut f32) -> egui::Response;

    /// Add a float drag value with an explicit constrained range.
    fn dev_drag_value_range(
        &mut self,
        id: impl Into<String>,
        value: &mut f32,
        range: RangeInclusive<f32>,
    ) -> egui::Response;

    /// Add an integer drag value with an explicit id.
    fn dev_drag_value_i32(&mut self, id: impl Into<String>, value: &mut i32) -> egui::Response;

    /// Add an integer drag value with an explicit constrained range.
    fn dev_drag_value_i32_range(
        &mut self,
        id: impl Into<String>,
        value: &mut i32,
        range: RangeInclusive<i32>,
    ) -> egui::Response;

    /// Add a multiline text edit with an explicit id.
    fn dev_text_edit_multiline(
        &mut self,
        id: impl Into<String>,
        text: &mut String,
    ) -> egui::Response;

    /// Add a toggle value with an explicit id.
    fn dev_toggle_value(
        &mut self,
        id: impl Into<String>,
        selected: &mut bool,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add a radio value with an explicit id.
    fn dev_radio_value<V: PartialEq + Clone>(
        &mut self,
        id: impl Into<String>,
        current: &mut V,
        alternative: V,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add a selectable value with an explicit id.
    fn dev_selectable_value<V: PartialEq + Clone>(
        &mut self,
        id: impl Into<String>,
        current: &mut V,
        alternative: V,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response;

    /// Add a separator with an explicit id.
    fn dev_separator(&mut self, id: impl Into<String>) -> egui::Response;

    /// Add a spinner with an explicit id.
    fn dev_spinner(&mut self, id: impl Into<String>) -> egui::Response;

    /// Add a progress bar with an explicit id.
    fn dev_progress_bar(&mut self, id: impl Into<String>, progress: f32) -> egui::Response;

    /// Add a progress bar with explicit overlay text options.
    fn dev_progress_bar_with(
        &mut self,
        id: impl Into<String>,
        progress: f32,
        options: ProgressBarOptions,
    ) -> egui::Response;

    /// Add a color-edit button with an explicit id.
    fn dev_color_edit(
        &mut self,
        id: impl Into<String>,
        color: &mut egui::Color32,
    ) -> egui::Response;

    /// Add a menu button with an explicit id.
    ///
    /// Records a `{id}.menu` container for the menu contents.
    fn dev_menu_button<R>(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<Option<R>>;

    /// Add a collapsing header with an explicit id, bound to app state.
    ///
    /// Records a `{id}.body` container for the expanded contents.
    fn dev_collapsing<R>(
        &mut self,
        id: impl Into<String>,
        open: &mut bool,
        heading: impl Into<egui::WidgetText>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> CollapsingResponse<R>;
}

impl DevUiExt for egui::Ui {
    fn dev_button(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.button(text);
        };
        let id = id.into();
        let (text, label) = widget_text_parts(text);
        let response = self.button(text);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Button { selected: None },
            Some(label),
            None,
        );
        response
    }

    fn dev_button_with(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
        options: ButtonOptions,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::Button::new(text).selected(options.selected));
        };
        let id = id.into();
        let (text, label) = widget_text_parts(text);
        let response = self.add(egui::Button::new(text).selected(options.selected));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Button {
                selected: Some(options.selected),
            },
            Some(label),
            None,
        );
        response
    }

    fn dev_link(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.link(text);
        };
        let id = id.into();
        let (text, label) = widget_text_parts(text);
        let response = self.link(text);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Link),
            Some(label),
            None,
        );
        response
    }

    fn dev_hyperlink(&mut self, id: impl Into<String>, url: impl ToString) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.hyperlink(url);
        };
        let id = id.into();
        let url = url.to_string();
        let response = self.hyperlink(url.clone());
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Link),
            Some(url.clone()),
            Some(WidgetValue::Text(url)),
        );
        response
    }

    fn dev_hyperlink_to(
        &mut self,
        id: impl Into<String>,
        label: impl Into<egui::WidgetText>,
        url: impl ToString,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.hyperlink_to(label, url);
        };
        let id = id.into();
        let (label, text) = widget_text_parts(label);
        let url = url.to_string();
        let response = self.hyperlink_to(label, url.clone());
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Link),
            Some(text),
            Some(WidgetValue::Text(url)),
        );
        response
    }

    fn dev_image<'a>(
        &mut self,
        id: impl Into<String>,
        description: impl Into<String>,
        source: impl Into<egui::ImageSource<'a>>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.image(source);
        };
        let id = id.into();
        let description = description.into();
        let response = self.image(source);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Image),
            Some(description.clone()),
            Some(WidgetValue::Text(description)),
        );
        response
    }

    fn dev_label(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.label(text);
        };
        let id = id.into();
        let (text, label) = widget_text_parts(text);
        let response = self.label(text);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Label),
            Some(label.clone()),
            Some(WidgetValue::Text(label)),
        );
        response
    }

    fn dev_checkbox(
        &mut self,
        id: impl Into<String>,
        value: &mut bool,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.checkbox(value, text);
        };
        let id = id.into();
        if let Some(updated) = take_bool_override(&inner, self, &id) {
            *value = updated;
        }
        let (text, label) = widget_text_parts(text);
        let response = self.checkbox(value, text);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Checkbox {
                indeterminate: None,
            },
            Some(label),
            Some(WidgetValue::Bool(*value)),
        );
        response
    }

    fn dev_checkbox_with(
        &mut self,
        id: impl Into<String>,
        value: &mut bool,
        text: impl Into<egui::WidgetText>,
        options: CheckboxOptions,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::Checkbox::new(value, text).indeterminate(options.indeterminate));
        };
        let id = id.into();
        if let Some(updated) = take_bool_override(&inner, self, &id) {
            *value = updated;
        }
        let (text, label) = widget_text_parts(text);
        let response =
            self.add(egui::Checkbox::new(value, text).indeterminate(options.indeterminate));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Checkbox {
                indeterminate: Some(options.indeterminate),
            },
            Some(label),
            Some(WidgetValue::Bool(*value)),
        );
        response
    }

    fn dev_text_edit(&mut self, id: impl Into<String>, text: &mut String) -> egui::Response {
        self.dev_text_edit_with(id, text, TextEditOptions::default())
    }

    fn dev_text_edit_with(
        &mut self,
        id: impl Into<String>,
        text: &mut String,
        options: TextEditOptions,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(options.builder(text));
        };
        let id = id.into();
        if let Some(updated) = take_text_override(&inner, self, &id) {
            *text = updated;
        }
        let response = self.add(options.builder(text));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            options.role(),
            None,
            options.recorded_value(text),
        );
        response
    }

    fn dev_slider(
        &mut self,
        id: impl Into<String>,
        value: &mut f32,
        range: RangeInclusive<f32>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::Slider::new(value, range));
        };
        let id = id.into();
        let range_meta = widget_range(&range);
        if let Some(updated) = take_float_override(&inner, self, &id) {
            *value = updated;
        }
        let response = self.add(egui::Slider::new(value, range));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Slider { range: range_meta },
            None,
            Some(WidgetValue::Float(f64::from(*value))),
        );
        response
    }

    fn dev_combo_box<T: ToString>(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        selected: &mut usize,
        options: &[T],
    ) -> egui::Response {
        let id = id.into();
        let label_text = label.into();
        let Some(inner) = active_inner() else {
            return show_combo_box(self, &id, &label_text, selected, options);
        };
        if let Some(updated) = take_usize_override(&inner, self, &id) {
            *selected = updated;
        }
        let option_labels = options.iter().map(ToString::to_string).collect::<Vec<_>>();
        let response = show_combo_box(self, &id, &label_text, selected, options);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::ComboBox {
                options: option_labels,
            },
            Some(label_text),
            Some(WidgetValue::Int(*selected as i64)),
        );
        response
    }

    fn dev_drag_value(&mut self, id: impl Into<String>, value: &mut f32) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::DragValue::new(value));
        };
        let id = id.into();
        if let Some(updated) = take_float_override(&inner, self, &id) {
            *value = updated;
        }
        let response = self.add(egui::DragValue::new(value));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::DragValue { range: None },
            None,
            Some(WidgetValue::Float(f64::from(*value))),
        );
        response
    }

    fn dev_drag_value_range(
        &mut self,
        id: impl Into<String>,
        value: &mut f32,
        range: RangeInclusive<f32>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::DragValue::new(value).range(range));
        };
        let id = id.into();
        let range_meta = widget_range(&range);
        if let Some(updated) = take_float_override(&inner, self, &id) {
            *value = updated;
        }
        let response = self.add(egui::DragValue::new(value).range(range));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::DragValue {
                range: Some(range_meta),
            },
            None,
            Some(WidgetValue::Float(f64::from(*value))),
        );
        response
    }

    fn dev_drag_value_i32(&mut self, id: impl Into<String>, value: &mut i32) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::DragValue::new(value));
        };
        let id = id.into();
        if let Some(updated) = take_i32_override(&inner, self, &id) {
            *value = updated;
        }
        let response = self.add(egui::DragValue::new(value));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::DragValue { range: None },
            None,
            Some(WidgetValue::Int(i64::from(*value))),
        );
        response
    }

    fn dev_drag_value_i32_range(
        &mut self,
        id: impl Into<String>,
        value: &mut i32,
        range: RangeInclusive<i32>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.add(egui::DragValue::new(value).range(range));
        };
        let id = id.into();
        let range_meta = widget_range(&range);
        if let Some(updated) = take_i32_override(&inner, self, &id) {
            *value = updated;
        }
        let response = self.add(egui::DragValue::new(value).range(range));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::DragValue {
                range: Some(range_meta),
            },
            None,
            Some(WidgetValue::Int(i64::from(*value))),
        );
        response
    }

    fn dev_text_edit_multiline(
        &mut self,
        id: impl Into<String>,
        text: &mut String,
    ) -> egui::Response {
        self.dev_text_edit_with(
            id,
            text,
            TextEditOptions {
                multiline: true,
                ..TextEditOptions::default()
            },
        )
    }

    fn dev_toggle_value(
        &mut self,
        id: impl Into<String>,
        selected: &mut bool,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.toggle_value(selected, text);
        };
        let id = id.into();
        if let Some(updated) = take_bool_override(&inner, self, &id) {
            *selected = updated;
        }
        let (text, label) = widget_text_parts(text);
        let response = self.toggle_value(selected, text);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Toggle),
            Some(label),
            Some(WidgetValue::Bool(*selected)),
        );
        response
    }

    fn dev_radio_value<V: PartialEq + Clone>(
        &mut self,
        id: impl Into<String>,
        current: &mut V,
        alternative: V,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        record_choice_widget(
            self,
            id,
            current,
            alternative,
            text,
            WidgetRole::Radio,
            Self::radio_value,
        )
    }

    fn dev_selectable_value<V: PartialEq + Clone>(
        &mut self,
        id: impl Into<String>,
        current: &mut V,
        alternative: V,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        record_choice_widget(
            self,
            id,
            current,
            alternative,
            text,
            WidgetRole::Selectable,
            Self::selectable_value,
        )
    }

    fn dev_separator(&mut self, id: impl Into<String>) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.separator();
        };
        let id = id.into();
        let response = self.separator();
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Separator),
            None,
            None,
        );
        response
    }

    fn dev_spinner(&mut self, id: impl Into<String>) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.spinner();
        };
        let id = id.into();
        let response = self.spinner();
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::Spinner),
            None,
            None,
        );
        response
    }

    fn dev_progress_bar(&mut self, id: impl Into<String>, progress: f32) -> egui::Response {
        self.dev_progress_bar_with(id, progress, ProgressBarOptions::default())
    }

    fn dev_progress_bar_with(
        &mut self,
        id: impl Into<String>,
        progress: f32,
        options: ProgressBarOptions,
    ) -> egui::Response {
        let progress = progress.clamp(0.0, 1.0);
        let mut widget = egui::ProgressBar::new(progress);
        let Some(inner) = active_inner() else {
            if let Some(text) = options.text {
                widget = widget.text(text);
            } else if options.show_percentage {
                widget = widget.show_percentage();
            }
            return self.add(widget);
        };
        let id = id.into();
        let label = if let Some(text) = options.text {
            widget = widget.text(text.clone());
            Some(text)
        } else if options.show_percentage {
            widget = widget.show_percentage();
            Some(format!("{}%", (progress * 100.0) as usize))
        } else {
            None
        };
        let response = self.add(widget);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::ProgressBar),
            label,
            Some(WidgetValue::Float(f64::from(progress))),
        );
        response
    }

    fn dev_color_edit(
        &mut self,
        id: impl Into<String>,
        color: &mut egui::Color32,
    ) -> egui::Response {
        let Some(inner) = active_inner() else {
            return self.color_edit_button_srgba(color);
        };
        let id = id.into();
        if let Some(updated) = take_color_override(&inner, self, &id) {
            *color = updated;
        }
        let response = self.color_edit_button_srgba(color);
        record_widget_with_layout(
            &inner,
            self,
            id,
            &response,
            WidgetRoleMeta::Plain(WidgetRole::ColorPicker),
            None,
            Some(WidgetValue::Text(format_color_hex(*color))),
        );
        response
    }

    fn dev_menu_button<R>(
        &mut self,
        id: impl Into<String>,
        text: impl Into<egui::WidgetText>,
        add_contents: impl FnOnce(&mut Self) -> R,
    ) -> egui::InnerResponse<Option<R>> {
        let Some(inner) = active_inner() else {
            return self.menu_button(text, |ui| container(ui, String::new(), add_contents));
        };
        let id = id.into();
        let menu_tag = format!("{id}.menu");
        let (text, label) = widget_text_parts(text);
        let output = self.menu_button(text, |ui| container(ui, menu_tag, add_contents));
        record_widget_with_layout(
            &inner,
            self,
            id,
            &output.response,
            WidgetRoleMeta::Plain(WidgetRole::MenuButton),
            Some(label),
            Some(WidgetValue::Bool(output.inner.is_some())),
        );
        output
    }

    fn dev_collapsing<R>(
        &mut self,
        id: impl Into<String>,
        open: &mut bool,
        heading: impl Into<egui::WidgetText>,
        add_contents: impl FnOnce(&mut Self) -> R,
    ) -> CollapsingResponse<R> {
        let id = id.into();
        let Some(inner) = active_inner() else {
            let output = egui::CollapsingHeader::new(heading)
                .id_salt(id.as_str())
                .open(Some(*open))
                .show(self, |ui| container(ui, String::new(), add_contents));
            if output.header_response.clicked() {
                *open = !*open;
            }
            return output;
        };
        if let Some(updated) = take_bool_override(&inner, self, &id) {
            *open = updated;
        }
        let body_tag = format!("{id}.body");
        let (heading, label) = widget_text_parts(heading);
        let output = egui::CollapsingHeader::new(heading)
            .id_salt(id.as_str())
            .open(Some(*open))
            .show(self, |ui| container(ui, body_tag, add_contents));
        if output.header_response.clicked() {
            *open = !*open;
        }
        record_widget_with_layout(
            &inner,
            self,
            id,
            &output.header_response,
            WidgetRoleMeta::Plain(WidgetRole::CollapsingHeader),
            Some(label),
            Some(WidgetValue::Bool(*open)),
        );
        output
    }
}

/// Helper extensions for recording scroll areas with explicit ids.
pub trait DevScrollAreaExt {
    /// Show a scroll area and record it with DevMCP metadata.
    fn dev_show<R>(
        self,
        ui: &mut egui::Ui,
        id: impl Into<String>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> ScrollAreaOutput<R>;

    /// Show a scroll area with viewport access and record it with DevMCP
    /// metadata.
    fn dev_show_viewport<R>(
        self,
        ui: &mut egui::Ui,
        id: impl Into<String>,
        add_contents: impl FnOnce(&mut egui::Ui, egui::Rect) -> R,
    ) -> ScrollAreaOutput<R>;
}

impl DevScrollAreaExt for egui::ScrollArea {
    fn dev_show<R>(
        self,
        ui: &mut egui::Ui,
        id: impl Into<String>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> ScrollAreaOutput<R> {
        self.dev_show_viewport(ui, id, |ui, _| add_contents(ui))
    }

    fn dev_show_viewport<R>(
        self,
        ui: &mut egui::Ui,
        id: impl Into<String>,
        add_contents: impl FnOnce(&mut egui::Ui, egui::Rect) -> R,
    ) -> ScrollAreaOutput<R> {
        if active_inner().is_none() {
            return self.show_viewport(ui, add_contents);
        }
        let id = id.into();
        let scroll_area = match super::instrument::take_scroll_area_override(ui, &id) {
            Some(offset) => self.scroll_offset(offset),
            None => self,
        };
        let output = scroll_area.show_viewport(ui, |ui, rect| {
            let _guard = super::instrument::begin_container(ui, id.clone());
            add_contents(ui, rect)
        });
        super::instrument::record_scroll_area(ui, id, &output);
        output
    }
}

/// Consume a queued widget value override for a custom instrumented widget.
///
/// Call this every frame that the custom widget is instrumented. The runtime
/// uses that call to distinguish wired custom widgets from widgets that publish
/// a settable value but never consume queued overrides.
pub fn take_widget_value_override(ui: &egui::Ui, id: &str) -> Option<WidgetValue> {
    let inner = active_inner()?;
    take_override(&inner, ui, id)
}

/// Mark `id` as a consumer of queued value overrides, and take its queued
/// override.
fn take_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<WidgetValue> {
    let viewport_id = ui.ctx().viewport_id();
    inner.mark_widget_value_consumer(viewport_id, id);
    inner.take_widget_value_update(viewport_id, id)
}

/// Show a combo box, and keep `selected` within the options.
///
/// The ID of the widget salts the box. `egui::ComboBox::from_label` would
/// salt it with the text of the label instead, and the ID of a `Ui` covers
/// every widget of a row or a panel, so two boxes of one `Ui` with one label
/// would take one ID. They would then share the state that egui keeps under
/// it, the open list among it: a click would open the list of one box and
/// close the list of the other in the same pass, and neither list would
/// appear. The ID of a widget is unique, so a box cannot collide with
/// another.
fn show_combo_box<T: ToString>(
    ui: &mut egui::Ui,
    id_salt: &str,
    label: &str,
    selected: &mut usize,
    options: &[T],
) -> egui::Response {
    let len = options.len();
    if len == 0 || *selected >= len {
        *selected = 0;
    }
    // A box without a label draws the button alone, with no empty label atom
    // beside it to widen it.
    let combo = if label.is_empty() {
        egui::ComboBox::from_id_salt(id_salt)
    } else {
        egui::ComboBox::new(id_salt, label)
    };
    if len == 0 {
        return combo.selected_text("").show_ui(ui, |_| {}).response;
    }
    let selected_text = options
        .get(*selected)
        .map(ToString::to_string)
        .unwrap_or_default();
    combo
        .selected_text(selected_text)
        .show_index(ui, selected, len, |index| options[index].to_string())
}

fn widget_text_parts(text: impl Into<egui::WidgetText>) -> (egui::WidgetText, String) {
    let text = text.into();
    let label = text.text().to_string();
    (text, label)
}

fn take_bool_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<bool> {
    match take_override(inner, ui, id) {
        Some(WidgetValue::Bool(updated)) => Some(updated),
        _ => None,
    }
}

fn take_text_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<String> {
    match take_override(inner, ui, id) {
        Some(WidgetValue::Text(updated)) => Some(updated),
        _ => None,
    }
}

fn take_color_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<egui::Color32> {
    match take_override(inner, ui, id) {
        Some(WidgetValue::Text(updated)) => parse_color_hex(&updated),
        _ => None,
    }
}

fn take_float_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<f32> {
    match take_override(inner, ui, id) {
        Some(WidgetValue::Float(updated)) => Some(updated as f32),
        Some(WidgetValue::Int(updated)) => Some(updated as f32),
        _ => None,
    }
}

fn take_i32_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<i32> {
    match take_override(inner, ui, id) {
        Some(WidgetValue::Int(updated)) => i32::try_from(updated).ok(),
        Some(WidgetValue::Float(updated)) => Some(updated as i32),
        _ => None,
    }
}

fn take_usize_override(inner: &Inner, ui: &egui::Ui, id: &str) -> Option<usize> {
    match take_override(inner, ui, id) {
        Some(WidgetValue::Int(updated)) => usize::try_from(updated).ok(),
        Some(WidgetValue::Float(updated)) => Some(updated as usize),
        _ => None,
    }
}

fn record_choice_widget<V>(
    ui: &mut egui::Ui,
    id: impl Into<String>,
    current: &mut V,
    alternative: V,
    text: impl Into<egui::WidgetText>,
    role: WidgetRole,
    add_widget: impl FnOnce(&mut egui::Ui, &mut V, V, egui::WidgetText) -> egui::Response,
) -> egui::Response
where
    V: PartialEq + Clone,
{
    let Some(inner) = active_inner() else {
        return add_widget(ui, current, alternative, text.into());
    };
    let id = id.into();
    let (text, label) = widget_text_parts(text);
    let selected_value = alternative.clone();
    if take_bool_override(&inner, ui, &id).is_some_and(|updated| updated) {
        *current = alternative.clone();
    }
    let response = add_widget(ui, current, alternative, text);
    let selected = *current == selected_value;
    record_widget_with_layout(
        &inner,
        ui,
        id,
        &response,
        WidgetRoleMeta::Plain(role),
        Some(label),
        Some(WidgetValue::Bool(selected)),
    );
    response
}

fn widget_range<T: Copy + Into<f64>>(range: &RangeInclusive<T>) -> WidgetRange {
    WidgetRange {
        min: (*range.start()).into(),
        max: (*range.end()).into(),
    }
}

/// Parse a CSS-style `#RRGGBB` or `#RRGGBBAA` color literal.
pub fn parse_color_hex(value: &str) -> Option<egui::Color32> {
    parse_css_hex(value, true)
}

pub fn format_color_hex(color: egui::Color32) -> String {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
}
