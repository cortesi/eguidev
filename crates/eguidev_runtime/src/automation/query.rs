//! Snapshot query helpers.

use std::collections::HashSet;

use super::*;

pub fn filter_viewport_snapshots(
    inner: &Inner,
    viewport_id: Option<String>,
) -> Result<Vec<ViewportSnapshot>, ToolError> {
    let mut viewports = inner.viewports.viewports_snapshot();
    if let Some(filter) = viewport_id {
        let id = viewport_id_to_string(resolve_viewport_id(inner, Some(filter))?);
        viewports.retain(|entry| entry.viewport_id == id);
    }
    Ok(viewports)
}

pub(super) fn widgets_at_point(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    position: Pos2,
    all_layers: bool,
) -> Vec<WidgetRegistryEntry> {
    let mut hits = inner
        .widgets
        .widget_list(viewport_id)
        .into_iter()
        .filter(|widget| point_in_rect(position, widget.interact_rect))
        .collect::<Vec<_>>();
    hits.reverse();
    if !all_layers {
        hits.truncate(1);
    }
    hits
}

/// Return the widget that actually covers one target at a point.
///
/// Two rules keep this from naming a widget that cannot take the click. An
/// enclosing container is recorded after its contents, so it is the last hit in
/// registry order even though it never occludes its own descendants; ancestors
/// are therefore transparent. A popup also paints in a later layer than the
/// panel behind it, whatever the registry order, so paint order ranks first.
pub(super) fn covering_widget_id(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    position: Pos2,
    target_id: &str,
) -> Option<String> {
    let widgets = inner.widgets.widget_list(viewport_id);
    let ancestors = ancestor_ids(&widgets, target_id);
    let top = widgets
        .iter()
        .enumerate()
        .filter(|(_, widget)| {
            point_in_rect(position, widget.interact_rect) && !ancestors.contains(&widget.id)
        })
        .max_by_key(|(index, widget)| (widget.layer_order, *index))
        .map(|(_, widget)| widget)?;
    (top.id != target_id).then(|| top.id.clone())
}

/// Collect every recorded ancestor id of one widget, excluding the widget
/// itself.
fn ancestor_ids(widgets: &[WidgetRegistryEntry], target_id: &str) -> HashSet<String> {
    let by_id: HashMap<&str, &WidgetRegistryEntry> = widgets
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let mut ancestors = HashSet::new();
    let mut next = by_id
        .get(target_id)
        .and_then(|entry| entry.parent_id.clone());
    while let Some(parent) = next {
        if !ancestors.insert(parent.clone()) {
            break;
        }
        next = by_id
            .get(parent.as_str())
            .and_then(|entry| entry.parent_id.clone());
    }
    ancestors
}

impl DevMcpServer {
    pub(super) async fn viewports_list(
        &self,
        viewport_id: Option<String>,
    ) -> ToolResult<Vec<ViewportSnapshot>> {
        filter_viewport_snapshots(&self.inner, viewport_id).map_err(Into::into)
    }
}
