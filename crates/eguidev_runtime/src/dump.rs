//! Canonical widget tree dumps for script, CLI, and failure artifacts.

use std::{cmp::Ordering, collections::HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::ToolError,
    registry::{Inner, viewport_id_to_string},
    types::{
        Rect, RoleState, Vec2, WidgetLayout, WidgetRange, WidgetRegistryEntry, WidgetRole,
        WidgetValue,
    },
    viewports::ViewportSnapshot,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DumpOptions {
    #[serde(default)]
    pub(crate) viewport: Option<String>,
    #[serde(default)]
    pub(crate) root: Option<String>,
    #[serde(default = "default_include_invisible")]
    pub(crate) include_invisible: bool,
    #[serde(default)]
    pub(crate) fields: DumpFields,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            viewport: None,
            root: None,
            include_invisible: default_include_invisible(),
            fields: DumpFields::Core,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DumpFields {
    #[default]
    Core,
    Full,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeDump {
    pub(crate) viewports: Vec<ViewportDump>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ViewportDump {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    pub(crate) focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) minimized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) occluded: Option<bool>,
    pub(crate) inner_size: Vec2,
    pub(crate) frame_count: u64,
    pub(crate) widgets: Vec<WidgetDump>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WidgetDump {
    pub(crate) id: String,
    pub(crate) role: WidgetRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<WidgetValue>,
    pub(crate) value_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
    pub(crate) rect: Rect,
    pub(crate) enabled: bool,
    pub(crate) visible: bool,
    pub(crate) focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) interact_rect: Option<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) layout: Option<WidgetLayout>,
    #[serde(rename = "scroll_state", skip_serializing_if = "Option::is_none")]
    pub(crate) scroll: Option<eguidev::ScrollAreaMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) range: Option<WidgetRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) layer: Option<String>,
    pub(crate) children: Vec<Self>,
}

#[derive(Clone)]
struct IndexedWidget {
    key: String,
    parent_key: Option<String>,
    widget: WidgetRegistryEntry,
}

fn default_include_invisible() -> bool {
    true
}

pub fn build_tree_dump(inner: &Inner, options: &DumpOptions) -> Result<TreeDump, ToolError> {
    ensure_dump_ready(inner)?;
    let snapshots = resolve_viewport_snapshots(inner, options.viewport.as_deref())?;
    let viewports = snapshots
        .into_iter()
        .map(|snapshot| build_viewport_dump(inner, snapshot, options))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TreeDump { viewports })
}

fn ensure_dump_ready(inner: &Inner) -> Result<(), ToolError> {
    if let Some(error) = inner.widgets.duplicate_explicit_id_error(&inner.viewports) {
        return Err(error.into());
    }
    if let Some(error) = inner.viewports.viewport_name_error() {
        return Err(error.into());
    }
    Ok(())
}

pub fn dump_text(dump: &TreeDump) -> String {
    let mut out = String::new();
    for viewport in &dump.viewports {
        render_viewport(viewport, &mut out);
        for widget in &viewport.widgets {
            render_widget(widget, 1, &mut out);
        }
    }
    out.trim_end().to_string()
}

fn resolve_viewport_snapshots(
    inner: &Inner,
    viewport: Option<&str>,
) -> Result<Vec<ViewportSnapshot>, ToolError> {
    let snapshots = inner.viewports.viewports_snapshot();
    let Some(viewport) = viewport else {
        return Ok(snapshots);
    };
    let resolved = inner
        .viewports
        .resolve_viewport_id(Some(viewport.to_string()))?;
    let selector = viewport_id_to_string(resolved);
    Ok(snapshots
        .into_iter()
        .filter(|snapshot| snapshot.viewport_id == selector)
        .collect())
}

fn build_viewport_dump(
    inner: &Inner,
    snapshot: ViewportSnapshot,
    options: &DumpOptions,
) -> Result<ViewportDump, ToolError> {
    let viewport_id = inner
        .viewports
        .resolve_viewport_id(Some(snapshot.viewport_id.clone()))?;
    let mut widgets = inner.widgets.widget_list(viewport_id);
    if !options.include_invisible {
        widgets.retain(|widget| widget.visible);
    }
    let widgets = match &options.root {
        Some(root) => build_root_subtrees(&widgets, root, options.fields),
        None => build_widget_tree(&widgets, options.fields),
    };
    let frame_count = inner
        .viewports
        .capture_snapshot(viewport_id)
        .map(|snapshot| snapshot.frame_count)
        .or_else(|| {
            inner
                .frame_health(viewport_id)
                .map(|health| health.frame_count)
        })
        .unwrap_or_default();
    Ok(ViewportDump {
        id: snapshot.viewport_id,
        name: snapshot.name,
        title: snapshot.title,
        focused: snapshot.focused,
        minimized: snapshot.minimized,
        occluded: snapshot.occluded,
        inner_size: snapshot.inner_size,
        frame_count,
        widgets,
    })
}

fn build_root_subtrees(
    widgets: &[WidgetRegistryEntry],
    root: &str,
    fields: DumpFields,
) -> Vec<WidgetDump> {
    let indexed = index_widgets(widgets);
    let children = children_map(&indexed);
    indexed
        .iter()
        .filter(|entry| entry.widget.id == root)
        .map(|entry| build_widget_dump(entry, &children, fields))
        .collect()
}

fn build_widget_tree(widgets: &[WidgetRegistryEntry], fields: DumpFields) -> Vec<WidgetDump> {
    let indexed = index_widgets(widgets);
    let children = children_map(&indexed);
    indexed
        .iter()
        .filter(|entry| entry.parent_key.is_none())
        .map(|entry| build_widget_dump(entry, &children, fields))
        .collect()
}

fn build_widget_dump(
    entry: &IndexedWidget,
    children: &HashMap<Option<String>, Vec<IndexedWidget>>,
    fields: DumpFields,
) -> WidgetDump {
    let widget = &entry.widget;
    let value_text = widget
        .value
        .as_ref()
        .map(WidgetValue::to_text)
        .unwrap_or_default();
    let selected = widget.role_state.as_ref().and_then(RoleState::selected);
    let scroll = (fields == DumpFields::Full)
        .then(|| widget.role_state.as_ref().and_then(RoleState::scroll_state))
        .flatten();
    let range = (fields == DumpFields::Full)
        .then(|| widget.role_state.as_ref().and_then(RoleState::range))
        .flatten();
    WidgetDump {
        id: widget.id.clone(),
        role: widget.role.clone(),
        label: widget.label.clone(),
        value: widget.value.clone(),
        value_text,
        data: widget.data.clone(),
        rect: widget.rect,
        enabled: widget.enabled,
        visible: widget.visible,
        focused: widget.focused,
        selected,
        interact_rect: (fields == DumpFields::Full).then_some(widget.interact_rect),
        layout: (fields == DumpFields::Full)
            .then(|| widget.layout.clone())
            .flatten(),
        scroll,
        range,
        layer: (fields == DumpFields::Full).then(|| widget.layer_id.clone()),
        children: children
            .get(&Some(entry.key.clone()))
            .into_iter()
            .flatten()
            .map(|child| build_widget_dump(child, children, fields))
            .collect(),
    }
}

fn index_widgets(widgets: &[WidgetRegistryEntry]) -> Vec<IndexedWidget> {
    let mut indexed = widgets
        .iter()
        .enumerate()
        .map(|(index, widget)| IndexedWidget {
            key: format!("{index}:{}", widget.id),
            parent_key: None,
            widget: widget.clone(),
        })
        .collect::<Vec<_>>();
    let parent_keys = indexed
        .iter()
        .enumerate()
        .map(|(index, _)| resolve_parent_key(&indexed, index))
        .collect::<Vec<_>>();
    for (entry, parent_key) in indexed.iter_mut().zip(parent_keys) {
        entry.parent_key = parent_key;
    }
    indexed
}

fn children_map(indexed: &[IndexedWidget]) -> HashMap<Option<String>, Vec<IndexedWidget>> {
    let mut map: HashMap<Option<String>, Vec<IndexedWidget>> = HashMap::new();
    for widget in indexed {
        map.entry(widget.parent_key.clone())
            .or_default()
            .push(widget.clone());
    }
    map
}

fn resolve_parent_key(indexed: &[IndexedWidget], index: usize) -> Option<String> {
    let widget = &indexed[index].widget;
    let parent_id = widget.parent_id.as_deref()?;
    indexed
        .iter()
        .enumerate()
        .filter(|(candidate_index, candidate)| {
            *candidate_index != index
                && candidate.widget.id == parent_id
                && rect_contains(candidate.widget.rect, widget.rect)
        })
        .min_by(|(_, left), (_, right)| {
            rect_area(left.widget.rect)
                .partial_cmp(&rect_area(right.widget.rect))
                .unwrap_or(Ordering::Equal)
        })
        .or_else(|| {
            indexed
                .iter()
                .enumerate()
                .rev()
                .find(|(candidate_index, candidate)| {
                    *candidate_index != index && candidate.widget.id == parent_id
                })
        })
        .map(|(_, candidate)| candidate.key.clone())
}

fn rect_area(rect: Rect) -> f32 {
    (rect.max.x - rect.min.x).max(0.0) * (rect.max.y - rect.min.y).max(0.0)
}

fn rect_contains(parent: Rect, child: Rect) -> bool {
    parent.min.x <= child.min.x
        && parent.min.y <= child.min.y
        && parent.max.x >= child.max.x
        && parent.max.y >= child.max.y
}
fn render_viewport(viewport: &ViewportDump, out: &mut String) {
    let selector = viewport.name.as_deref().unwrap_or(&viewport.id);
    out.push_str("viewport ");
    out.push_str(selector);
    if let Some(title) = &viewport.title {
        out.push(' ');
        out.push_str(&quoted(title, usize::MAX));
    }
    out.push(' ');
    out.push_str(&format_size(viewport.inner_size));
    if viewport.focused {
        out.push_str(" focused");
    }
    if viewport.minimized == Some(true) {
        out.push_str(" minimized");
    }
    if viewport.occluded == Some(true) {
        out.push_str(" occluded");
    }
    out.push_str(" frame=");
    out.push_str(&viewport.frame_count.to_string());
    out.push('\n');
}

fn render_widget(widget: &WidgetDump, depth: usize, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(&widget.id);
    out.push(' ');
    out.push_str(&role_name(&widget.role));
    if let Some(label) = &widget.label {
        out.push(' ');
        out.push_str(&quoted(label, 60));
    }
    if widget.value.is_some() {
        out.push_str(" value=");
        out.push_str(&truncate_chars(&compact_value_json(&widget.value), 60));
    } else if !widget.value_text.is_empty() {
        out.push_str(" value_text=");
        out.push_str(&quoted(&widget.value_text, 60));
    }
    out.push(' ');
    out.push_str(&format_rect(widget.rect));
    render_flags(widget, out);
    if let Some(data) = &widget.data {
        out.push_str(" data=");
        out.push_str(&truncate_chars(&compact_json(data), 80));
    }
    out.push('\n');
    for child in &widget.children {
        render_widget(child, depth + 1, out);
    }
}

fn render_flags(widget: &WidgetDump, out: &mut String) {
    if widget.focused {
        out.push_str(" focused");
    }
    if widget.selected == Some(true) {
        out.push_str(" selected");
    }
    if !widget.visible {
        out.push_str(" !visible");
    }
    if !widget.enabled {
        out.push_str(" !enabled");
    }
    if let Some(layout) = &widget.layout {
        if layout.clipped {
            let clipped = ((1.0 - layout.visible_fraction).max(0.0) * 100.0).round();
            out.push_str(&format!(" !clipped({clipped:.0}%)"));
        }
        if layout.overflow {
            out.push_str(" !overflow");
        }
    }
}

fn role_name(role: &WidgetRole) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn compact_value_json(value: &Option<WidgetValue>) -> String {
    compact_json(&serde_json::to_value(value).unwrap_or(Value::Null))
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn quoted(value: &str, max_chars: usize) -> String {
    compact_json(&Value::String(truncate_chars(value, max_chars)))
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let prefix = max_chars.saturating_sub(3);
    let mut truncated = value.chars().take(prefix).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn format_size(size: Vec2) -> String {
    format!("{}x{}", format_num(size.x), format_num(size.y))
}

fn format_rect(rect: Rect) -> String {
    format!(
        "[{},{} {}x{}]",
        format_num(rect.min.x),
        format_num(rect.min.y),
        format_num(rect.max.x - rect.min.x),
        format_num(rect.max.y - rect.min.y)
    )
}

fn format_num(value: f32) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let rounded = value.round();
    if (value - rounded).abs() < 0.01 {
        return format!("{rounded:.0}");
    }
    let value = format!("{value:.1}");
    value
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::{Pos2, WidgetLayout};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            min: Pos2 { x, y },
            max: Pos2 { x: x + w, y: y + h },
        }
    }

    fn widget(id: &str, role: WidgetRole, rect: Rect) -> WidgetDump {
        WidgetDump {
            id: id.to_string(),
            role,
            label: None,
            value: None,
            value_text: String::new(),
            data: None,
            rect,
            enabled: true,
            visible: true,
            focused: false,
            selected: None,
            interact_rect: None,
            layout: None,
            scroll: None,
            range: None,
            layer: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn dump_text_renders_viewports_and_widget_hierarchy() {
        let mut parent = widget(
            "app.root",
            WidgetRole::Unknown,
            rect(0.0, 0.0, 200.0, 120.0),
        );
        let mut child = widget(
            "basic.name",
            WidgetRole::TextEdit,
            rect(10.0, 20.0, 80.0, 24.0),
        );
        child.label = Some("Name".to_string());
        child.value = Some(WidgetValue::Text("Luau".to_string()));
        child.value_text = "Luau".to_string();
        child.focused = true;
        parent.children.push(child);
        let dump = TreeDump {
            viewports: vec![ViewportDump {
                id: "root".to_string(),
                name: None,
                title: Some("Demo".to_string()),
                focused: true,
                minimized: None,
                occluded: None,
                inner_size: Vec2 { x: 800.0, y: 600.0 },
                frame_count: 42,
                widgets: vec![parent],
            }],
        };

        assert_eq!(
            dump_text(&dump),
            "viewport root \"Demo\" 800x600 focused frame=42\n  app.root unknown [0,0 200x120]\n    basic.name text_edit \"Name\" value=\"Luau\" [10,20 80x24] focused"
        );
    }

    #[test]
    fn dump_text_truncates_value_json() {
        let mut item = widget("source", WidgetRole::TextEdit, rect(0.0, 0.0, 120.0, 24.0));
        item.value = Some(WidgetValue::Text("x".repeat(120)));
        item.value_text = "x".repeat(120);
        let dump = TreeDump {
            viewports: vec![ViewportDump {
                id: "root".to_string(),
                name: None,
                title: None,
                focused: false,
                minimized: None,
                occluded: None,
                inner_size: Vec2 { x: 800.0, y: 600.0 },
                frame_count: 1,
                widgets: vec![item],
            }],
        };
        let text = dump_text(&dump);
        let line = text.lines().nth(1).expect("widget line");

        assert!(line.contains("value=\"xxxxxxxx"));
        assert!(line.contains("... [0,0 120x24]"));
        assert!(
            !line.contains(&"x".repeat(80)),
            "value JSON should be bounded in text dumps"
        );
    }

    #[test]
    fn tree_dump_json_omits_absent_optional_fields() {
        let dump = TreeDump {
            viewports: vec![ViewportDump {
                id: "root".to_string(),
                name: None,
                title: None,
                focused: false,
                minimized: None,
                occluded: None,
                inner_size: Vec2 { x: 800.0, y: 600.0 },
                frame_count: 1,
                widgets: vec![widget(
                    "status",
                    WidgetRole::Label,
                    rect(0.0, 0.0, 50.0, 20.0),
                )],
            }],
        };
        let value = serde_json::to_value(&dump).expect("serialize dump");
        let viewport = value["viewports"][0].as_object().expect("viewport object");
        let widget = viewport["widgets"][0].as_object().expect("widget object");

        for key in ["name", "title", "minimized", "occluded"] {
            assert!(
                !viewport.contains_key(key),
                "viewport optional field {key} should be omitted"
            );
        }
        for key in [
            "label",
            "value",
            "data",
            "selected",
            "interact_rect",
            "layout",
            "scroll_state",
            "range",
            "layer",
        ] {
            assert!(
                !widget.contains_key(key),
                "widget optional field {key} should be omitted"
            );
        }
        assert_eq!(widget["children"], json!([]));
    }

    #[test]
    fn dump_text_renders_data_and_full_field_flags() {
        let mut item = widget("status", WidgetRole::Label, rect(0.0, 0.0, 50.0, 20.0));
        item.data = Some(json!({ "payload": "x".repeat(100) }));
        item.enabled = false;
        item.layout = Some(WidgetLayout {
            desired_size: Vec2 { x: 80.0, y: 20.0 },
            actual_size: Vec2 { x: 50.0, y: 20.0 },
            clip_rect: rect(0.0, 0.0, 25.0, 20.0),
            clipped: true,
            overflow: true,
            available_rect: rect(0.0, 0.0, 50.0, 20.0),
            visible_fraction: 0.5,
        });
        let dump = TreeDump {
            viewports: vec![ViewportDump {
                id: "vp:1".to_string(),
                name: Some("hud".to_string()),
                title: None,
                focused: false,
                minimized: None,
                occluded: Some(true),
                inner_size: Vec2 { x: 300.0, y: 200.0 },
                frame_count: 7,
                widgets: vec![item],
            }],
        };
        let text = dump_text(&dump);

        assert!(text.starts_with("viewport hud 300x200 occluded frame=7\n"));
        assert!(text.contains("status label [0,0 50x20] !enabled !clipped(50%) !overflow"));
        assert!(text.contains("data={\"payload\":\"xxxxxxxx"));
        assert!(text.contains("..."));
    }
}
