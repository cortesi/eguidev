//! Snapshot query helpers.

use super::*;

pub(crate) fn filter_viewport_snapshots(
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

impl DevMcpServer {
    pub(super) async fn viewports_list(
        &self,
        viewport_id: Option<String>,
    ) -> ToolResult<Vec<ViewportSnapshot>> {
        filter_viewport_snapshots(&self.inner, viewport_id).map_err(Into::into)
    }
}
