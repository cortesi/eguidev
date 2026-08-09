use std::{collections::HashMap, iter};

use egui::{Color32, TextStyle};

use super::*;
use crate::{
    automation::{
        ErrorCode, ToolError,
        results::{LayoutIssue, TextMeasure, TextMeasureLine},
        types::LayoutIssueKind,
    },
    overlay::{rect_intersection, rect_size},
    types::{Pos2, Rect, RoleState, WidgetRegistryEntry, WidgetRole, WidgetValue},
};

/// Largest coordinate difference still treated as an exact match.
const RECT_EPSILON: f32 = 0.5;

pub fn widget_text(widget: &WidgetRegistryEntry) -> Option<String> {
    match widget.value.as_ref() {
        Some(WidgetValue::Text(text)) => Some(text.clone()),
        _ => widget.label.clone(),
    }
}

pub fn measure_text(
    ctx: &egui::Context,
    widget: &WidgetRegistryEntry,
) -> Result<TextMeasure, ToolError> {
    let text = widget_text(widget)
        .ok_or_else(|| ToolError::new(ErrorCode::Unsupported, "Widget does not contain text"))?;
    let style = ctx.global_style();
    let font_id = TextStyle::Body.resolve(style.as_ref());
    let desired_galley =
        ctx.fonts_mut(|fonts| fonts.layout_no_wrap(text.clone(), font_id.clone(), Color32::WHITE));
    let desired_size = desired_galley.rect.size();
    let actual_size = rect_size(widget.rect);
    let wrap_width = actual_size.x.max(1.0);
    let wrapped_galley = ctx
        .fonts_mut(|fonts| fonts.layout(text.clone(), font_id.clone(), Color32::WHITE, wrap_width));
    let line_height = wrapped_galley
        .rows
        .first()
        .map(|row| row.size.y)
        .unwrap_or(0.0);
    let lines = wrapped_galley
        .rows
        .iter()
        .map(|row| TextMeasureLine {
            text: row.glyphs.iter().map(|glyph| glyph.chr).collect(),
            width: row.size.x,
        })
        .collect::<Vec<_>>();
    Ok(TextMeasure {
        text: text.clone(),
        visible_text: text,
        desired_size: desired_size.into(),
        actual_size,
        line_height,
        lines,
        ellipsis: false,
    })
}

/// Layout analysis over one viewport's complete widget registry.
///
/// Structure always resolves against the complete registry: ancestry, the
/// effective clip rect, and the extent an ancestor scroll area declares. The
/// `scope` slice passed to each check selects only which widgets the check
/// reports on, so a check scoped to one row still sees the scroll area that
/// holds the row.
pub struct LayoutAnalysis<'a> {
    /// Every widget captured for the viewport.
    registry: &'a [WidgetRegistryEntry],
    /// Registry index by canonical widget id.
    by_id: HashMap<&'a str, &'a WidgetRegistryEntry>,
    /// Viewport rect, when the viewport reported one.
    viewport_rect: Option<Rect>,
}

impl<'a> LayoutAnalysis<'a> {
    /// Index one viewport's registry for structural queries.
    pub fn new(registry: &'a [WidgetRegistryEntry], viewport_rect: Option<Rect>) -> Self {
        let by_id = registry
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        Self {
            registry,
            by_id,
            viewport_rect,
        }
    }

    /// Report widgets whose rect has collapsed on either axis.
    pub fn zero_size(&self, scope: &[WidgetRegistryEntry]) -> Vec<LayoutIssue> {
        scope
            .iter()
            .filter(|widget| {
                let size = rect_size(widget.rect);
                size.x <= 0.0 || size.y <= 0.0
            })
            .map(|widget| LayoutIssue {
                kind: LayoutIssueKind::ZeroSize,
                widgets: vec![widget.id.clone()],
                message: "Widget has zero size".to_string(),
                rect: Some(widget.rect),
            })
            .collect()
    }

    /// Report widgets clipped by the viewport rather than by a container.
    pub fn clipping(&self, scope: &[WidgetRegistryEntry]) -> Vec<LayoutIssue> {
        scope
            .iter()
            .filter_map(|widget| {
                let layout = widget.layout.as_ref()?;
                let clipped = layout.clipped || layout.visible_fraction < 1.0;
                (clipped && !self.clipped_by_container(widget)).then(|| LayoutIssue {
                    kind: LayoutIssueKind::Clipping,
                    widgets: vec![widget.id.clone()],
                    message: "Widget is clipped".to_string(),
                    rect: Some(widget.rect),
                })
            })
            .collect()
    }

    /// Report widgets that start inside their slot and then overrun it.
    pub fn overflow(&self, scope: &[WidgetRegistryEntry]) -> Vec<LayoutIssue> {
        scope
            .iter()
            .filter_map(|widget| {
                let layout = widget.layout.as_ref()?;
                let starts_within_slot = point_in_rect(widget.rect.min, layout.available_rect);
                (layout.overflow && starts_within_slot && !self.clipped_by_container(widget)).then(
                    || LayoutIssue {
                        kind: LayoutIssueKind::Overflow,
                        widgets: vec![widget.id.clone()],
                        message: "Widget overflows its clip rect".to_string(),
                        rect: Some(widget.rect),
                    },
                )
            })
            .collect()
    }

    /// Report unrelated widget pairs in scope whose visible rects intersect.
    pub fn overlaps(&self, scope: &[WidgetRegistryEntry]) -> Vec<LayoutIssue> {
        let mut issues = Vec::new();
        for (index, first) in scope.iter().enumerate() {
            for second in scope.iter().skip(index + 1) {
                if !first.visible || !second.visible {
                    continue;
                }
                if first.role == WidgetRole::ScrollArea || second.role == WidgetRole::ScrollArea {
                    continue;
                }
                // Separate egui layers stack on purpose. A window, menu, or
                // tooltip covering the surface below it is the intent, not a
                // layout defect.
                if first.layer_id != second.layer_id {
                    continue;
                }
                if self.are_ancestor_descendant(first, second) {
                    continue;
                }
                let (Some(first_rect), Some(second_rect)) =
                    (self.visible_rect(first), self.visible_rect(second))
                else {
                    continue;
                };
                if let Some(overlap_rect) = rect_intersection(first_rect, second_rect) {
                    issues.push(LayoutIssue {
                        kind: LayoutIssueKind::Overlap,
                        widgets: vec![first.id.clone(), second.id.clone()],
                        message: "Widgets overlap".to_string(),
                        rect: Some(overlap_rect),
                    });
                }
            }
        }
        issues
    }

    /// Report widgets outside the viewport whose position no ancestor explains.
    ///
    /// An ancestor scroll area explains the position when the widget lies
    /// inside the extent that scroll area declares, which is where scrolled
    /// content sits. Any other ancestor explains it when its clip rect differs
    /// from the viewport rect, which is where clipped content sits.
    pub fn offscreen(&self, scope: &[WidgetRegistryEntry]) -> Vec<LayoutIssue> {
        let Some(viewport_rect) = self.viewport_rect else {
            return Vec::new();
        };
        scope
            .iter()
            .filter(|widget| {
                !rect_contains_rect(viewport_rect, widget.rect)
                    && !self.has_nested_clip_region(widget)
                    && !self.within_ancestor_scroll_extent(widget)
            })
            .map(|widget| LayoutIssue {
                kind: LayoutIssueKind::Offscreen,
                widgets: vec![widget.id.clone()],
                message: "Widget is outside the viewport".to_string(),
                rect: Some(widget.rect),
            })
            .collect()
    }

    /// Report text that needs more width than its widget was given.
    pub fn text_truncation(
        &self,
        ctx: &egui::Context,
        scope: &[WidgetRegistryEntry],
    ) -> Result<Vec<LayoutIssue>, ToolError> {
        let mut issues = Vec::new();
        for widget in scope {
            if widget_text(widget).is_none() {
                continue;
            }
            let measurement = measure_text(ctx, widget)?;
            if measurement.desired_size.x > measurement.actual_size.x
                && measurement.lines.len() <= 1
            {
                issues.push(LayoutIssue {
                    kind: LayoutIssueKind::TextTruncation,
                    widgets: vec![widget.id.clone()],
                    message: format!(
                        "Text truncated (needs {:.1}px, has {:.1}px)",
                        measurement.desired_size.x, measurement.actual_size.x
                    ),
                    rect: Some(widget.rect),
                });
            }
        }
        Ok(issues)
    }

    /// The part of the widget rect a viewer can actually see.
    ///
    /// A widget is bounded by its clip region and by every scroll area that
    /// holds it, so the scrolled-away remainder cannot overlap anything.
    /// Returns `None` when no part of the widget survives.
    fn visible_rect(&self, widget: &WidgetRegistryEntry) -> Option<Rect> {
        let mut rect = widget.rect;
        if let Some(clip_rect) = self.effective_clip_rect(widget) {
            rect = rect_intersection(rect, clip_rect)?;
        }
        for scroll in self
            .ancestors(widget)
            .filter(|ancestor| ancestor.role == WidgetRole::ScrollArea)
        {
            rect = rect_intersection(rect, scroll.rect)?;
        }
        Some(rect)
    }

    /// Whether a container, not the viewport edge, bounds this widget.
    ///
    /// Clipping inside a scroll area is what a scroll area is for, and a
    /// nested clip region is a deliberate boundary. Neither is a layout defect.
    fn clipped_by_container(&self, widget: &WidgetRegistryEntry) -> bool {
        self.has_nested_clip_region(widget) || self.has_ancestor_scroll_area(widget)
    }

    /// Whether any ancestor of the widget is a scroll area.
    fn has_ancestor_scroll_area(&self, widget: &WidgetRegistryEntry) -> bool {
        self.ancestors(widget)
            .any(|ancestor| ancestor.role == WidgetRole::ScrollArea)
    }

    /// Whether the widget sits inside a clip region narrower than the viewport.
    fn has_nested_clip_region(&self, widget: &WidgetRegistryEntry) -> bool {
        let (Some(viewport_rect), Some(clip_rect)) =
            (self.viewport_rect, self.effective_clip_rect(widget))
        else {
            return false;
        };
        !rect_approx_eq(clip_rect, viewport_rect)
    }

    /// The widget's own clip rect, or the nearest ancestor's when it has none.
    ///
    /// A rect published without an `egui::Response` carries no layout, so it
    /// inherits the clip region of the container that published it.
    fn effective_clip_rect(&self, widget: &WidgetRegistryEntry) -> Option<Rect> {
        if let Some(layout) = widget.layout.as_ref() {
            return Some(layout.clip_rect);
        }
        self.ancestors(widget)
            .find_map(|ancestor| ancestor.layout.as_ref())
            .map(|layout| layout.clip_rect)
    }

    /// Whether an ancestor scroll area declares an extent holding this widget.
    fn within_ancestor_scroll_extent(&self, widget: &WidgetRegistryEntry) -> bool {
        self.ancestors(widget)
            .filter_map(scroll_content_rect)
            .any(|content_rect| rect_contains_rect(content_rect, widget.rect))
    }

    fn are_ancestor_descendant(
        &self,
        first: &WidgetRegistryEntry,
        second: &WidgetRegistryEntry,
    ) -> bool {
        self.has_ancestor(first, &second.id) || self.has_ancestor(second, &first.id)
    }

    fn has_ancestor(&self, descendant: &WidgetRegistryEntry, ancestor_id: &str) -> bool {
        self.ancestors(descendant)
            .any(|ancestor| ancestor.id == ancestor_id)
    }

    /// Walk parents from the widget upward, stopping on a missing id or a cycle.
    fn ancestors(
        &self,
        widget: &WidgetRegistryEntry,
    ) -> impl Iterator<Item = &'a WidgetRegistryEntry> {
        let mut parent_id = widget.parent_id.clone();
        let mut remaining = self.registry.len();
        iter::from_fn(move || {
            let id = parent_id.take()?;
            if remaining == 0 {
                return None;
            }
            remaining -= 1;
            let ancestor = *self.by_id.get(id.as_str())?;
            parent_id = ancestor.parent_id.clone();
            Some(ancestor)
        })
    }
}

/// The extent a scroll area declares, in viewport coordinates.
///
/// The extent starts at the scroll area rect offset backwards by the current
/// scroll position and spans the full content size, so it covers the rows that
/// have scrolled out of view on either side.
fn scroll_content_rect(widget: &WidgetRegistryEntry) -> Option<Rect> {
    if widget.role != WidgetRole::ScrollArea {
        return None;
    }
    let scroll = widget
        .role_state
        .as_ref()
        .and_then(RoleState::scroll_state)?;
    let min = Pos2 {
        x: widget.rect.min.x - scroll.offset.x,
        y: widget.rect.min.y - scroll.offset.y,
    };
    Some(Rect {
        min,
        max: Pos2 {
            x: min.x + scroll.content_size.x,
            y: min.y + scroll.content_size.y,
        },
    })
}

fn rect_approx_eq(left: Rect, right: Rect) -> bool {
    approx_eq(left.min.x, right.min.x)
        && approx_eq(left.min.y, right.min.y)
        && approx_eq(left.max.x, right.max.x)
        && approx_eq(left.max.y, right.max.y)
}

fn approx_eq(left: f32, right: f32) -> bool {
    (left - right).abs() <= RECT_EPSILON
}

pub fn point_in_rect(pos: Pos2, rect: Rect) -> bool {
    pos.x >= rect.min.x && pos.x <= rect.max.x && pos.y >= rect.min.y && pos.y <= rect.max.y
}

/// Whether `outer` contains `inner`, tolerating sub-pixel rounding.
pub fn rect_contains_rect(outer: Rect, inner: Rect) -> bool {
    inner.min.x >= outer.min.x - RECT_EPSILON
        && inner.min.y >= outer.min.y - RECT_EPSILON
        && inner.max.x <= outer.max.x + RECT_EPSILON
        && inner.max.y <= outer.max.y + RECT_EPSILON
}

impl DevMcpServer {
    pub(super) async fn check_layout(
        &self,
        viewport_id: Option<String>,
        root: Option<WidgetRef>,
    ) -> ToolResult<Vec<LayoutIssue>> {
        let viewport_id = self.resolve_scope_viewport(viewport_id, root.as_ref())?;
        let registry = self.inner.widgets.widget_list(viewport_id);
        let viewport_id_str = viewport_id_to_string(viewport_id);
        // The scope selects what the result reports; structure always resolves
        // against the complete viewport registry.
        let scope = match root.as_ref() {
            Some(root) => {
                let root = resolve_widget(&self.inner, Some(viewport_id_str.as_str()), root)?;
                collect_subtree(&registry, &root)
            }
            None => registry.clone(),
        };

        let analysis = LayoutAnalysis::new(&registry, viewport_rect(&self.inner, viewport_id));
        let mut issues = analysis.zero_size(&scope);
        issues.extend(analysis.clipping(&scope));
        issues.extend(analysis.overflow(&scope));
        issues.extend(analysis.overlaps(&scope));
        issues.extend(analysis.offscreen(&scope));
        if let Some(ctx) = self.inner.context_for(viewport_id) {
            issues.extend(analysis.text_truncation(&ctx, &scope)?);
        }
        Ok(issues)
    }

    /// Measure text for a text-containing widget.
    pub(super) async fn text_measure(&self, target: WidgetRef) -> ToolResult<TextMeasure> {
        let (widget, viewport_id) = resolve_widget_and_viewport(&self.inner, None, &target)?;
        let ctx = self.inner.context_for(viewport_id).ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidArgument,
                "Context not available for text measurement",
            )
        })?;
        Ok(measure_text(&ctx, &widget)?)
    }

    pub(super) fn widget_at_point_result(
        &self,
        pos: Pos2,
        all_layers: Option<bool>,
        viewport_id: Option<&str>,
    ) -> ToolResult<WidgetAtPointResult> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id.map(str::to_string))?;
        let hits = widgets_at_point(&self.inner, viewport_id, pos, all_layers.unwrap_or(false));
        Ok(WidgetAtPointResult { widgets: hits })
    }

    pub(super) fn resolve_scope_viewport(
        &self,
        viewport_id: Option<String>,
        scope: Option<&WidgetRef>,
    ) -> Result<egui::ViewportId, ToolError> {
        if let Some(scope) = scope {
            let widget = resolve_widget(&self.inner, viewport_id.as_deref(), scope)?;
            return resolve_viewport_id(&self.inner, Some(widget.viewport_id));
        }
        resolve_viewport_id(&self.inner, viewport_id)
    }
}

#[cfg(test)]
mod tests {
    use std::slice;

    use super::*;
    use crate::types::{Vec2, WidgetLayout, WidgetRole};

    fn rect(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Rect {
        Rect {
            min: Pos2 { x: min_x, y: min_y },
            max: Pos2 { x: max_x, y: max_y },
        }
    }

    fn entry(id: &str, role: WidgetRole, rect: Rect, visible: bool) -> WidgetRegistryEntry {
        WidgetRegistryEntry {
            id: id.to_string(),
            explicit_id: true,
            native_id: 1,
            viewport_id: "root".to_string(),
            layer_id: "layer".to_string(),
            rect,
            interact_rect: rect,
            role,
            label: None,
            value: None,
            data: None,
            layout: None,
            role_state: None,
            parent_id: Some("panel".to_string()),
            enabled: true,
            visible,
            focused: false,
        }
    }

    fn layout(clip_rect: Rect, available_rect: Rect) -> WidgetLayout {
        WidgetLayout {
            desired_size: egui::vec2(100.0, 20.0).into(),
            actual_size: egui::vec2(100.0, 20.0).into(),
            clip_rect,
            clipped: false,
            overflow: false,
            available_rect,
            visible_fraction: 1.0,
        }
    }

    /// A scroll area that fills the viewport, holding rows with the same clip rect.
    fn viewport_filling_scroll(
        viewport_rect: Rect,
        offset_y: f32,
        content_height: f32,
    ) -> WidgetRegistryEntry {
        let mut scroll = entry("scroll", WidgetRole::ScrollArea, viewport_rect, true);
        scroll.parent_id = None;
        scroll.layout = Some(layout(viewport_rect, viewport_rect));
        scroll.role_state = Some(RoleState::ScrollArea {
            offset: Vec2 {
                x: 0.0,
                y: offset_y,
            },
            viewport_size: Vec2 {
                x: viewport_rect.max.x - viewport_rect.min.x,
                y: viewport_rect.max.y - viewport_rect.min.y,
            },
            content_size: Vec2 {
                x: viewport_rect.max.x - viewport_rect.min.x,
                y: content_height,
            },
        });
        scroll
    }

    #[test]
    fn layout_checks_ignore_nested_scroll_clipping() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let scroll_rect = rect(0.0, 40.0, 100.0, 80.0);

        let mut row = entry(
            "row",
            WidgetRole::Label,
            rect(0.0, 70.0, 100.0, 120.0),
            false,
        );
        row.layout = Some(WidgetLayout {
            clipped: true,
            overflow: true,
            visible_fraction: 0.2,
            ..layout(scroll_rect, viewport_rect)
        });

        let registry = vec![row];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.clipping(&registry).is_empty());
        assert!(analysis.overflow(&registry).is_empty());
        assert!(analysis.offscreen(&registry).is_empty());
    }

    #[test]
    fn scrolled_row_in_viewport_filling_scroll_area_is_not_offscreen() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let scroll = viewport_filling_scroll(viewport_rect, 200.0, 500.0);
        // The row scrolled above the viewport but stays inside the content extent.
        let mut row = entry(
            "row",
            WidgetRole::Label,
            rect(0.0, -180.0, 100.0, -160.0),
            false,
        );
        row.parent_id = Some("scroll".to_string());
        row.layout = Some(layout(viewport_rect, viewport_rect));

        let registry = vec![scroll, row.clone()];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.offscreen(slice::from_ref(&row)).is_empty());
    }

    #[test]
    fn published_rect_inside_a_scroll_area_is_not_offscreen() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let scroll = viewport_filling_scroll(viewport_rect, 200.0, 500.0);
        // A published rect carries no layout, so it inherits the scroll clip rect.
        let mut marker = entry(
            "marker",
            WidgetRole::Unknown,
            rect(0.0, -180.0, 20.0, -160.0),
            true,
        );
        marker.parent_id = Some("scroll".to_string());

        let registry = vec![scroll, marker.clone()];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.offscreen(slice::from_ref(&marker)).is_empty());
    }

    #[test]
    fn widget_beyond_the_declared_content_extent_is_offscreen() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let scroll = viewport_filling_scroll(viewport_rect, 0.0, 300.0);
        let mut stray = entry(
            "stray",
            WidgetRole::Label,
            rect(0.0, 320.0, 100.0, 340.0),
            true,
        );
        stray.parent_id = Some("scroll".to_string());
        stray.layout = Some(layout(viewport_rect, viewport_rect));

        let registry = vec![scroll, stray.clone()];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        let issues = analysis.offscreen(slice::from_ref(&stray));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].widgets, vec!["stray"]);
    }

    #[test]
    fn widget_off_a_non_scrollable_axis_is_offscreen() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        // The content is only as wide as the viewport, so horizontal escape is a defect.
        let scroll = viewport_filling_scroll(viewport_rect, 0.0, 500.0);
        let mut wide = entry(
            "wide",
            WidgetRole::Label,
            rect(160.0, 10.0, 220.0, 30.0),
            true,
        );
        wide.parent_id = Some("scroll".to_string());
        wide.layout = Some(layout(viewport_rect, viewport_rect));

        let registry = vec![scroll, wide.clone()];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        let issues = analysis.offscreen(slice::from_ref(&wide));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].widgets, vec!["wide"]);
    }

    #[test]
    fn scoped_analysis_still_resolves_the_ancestor_scroll_area() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let scroll = viewport_filling_scroll(viewport_rect, 200.0, 500.0);
        let mut row = entry(
            "row",
            WidgetRole::Label,
            rect(0.0, -180.0, 100.0, -160.0),
            false,
        );
        row.parent_id = Some("scroll".to_string());
        row.layout = Some(layout(viewport_rect, viewport_rect));

        let registry = vec![scroll, row.clone()];
        let scope = vec![row];
        // The scope holds the row alone; the analysis still sees the scroll area.
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.offscreen(&scope).is_empty());

        // Without the surrounding registry the same scope reports the row.
        let blind = LayoutAnalysis::new(&scope, Some(viewport_rect));
        assert_eq!(blind.offscreen(&scope).len(), 1);
    }

    #[test]
    fn overlap_checks_ignore_scroll_areas_and_invisible_widgets() {
        let scroll = entry(
            "scroll",
            WidgetRole::ScrollArea,
            rect(0.0, 0.0, 100.0, 40.0),
            true,
        );
        let row = entry("row", WidgetRole::Label, rect(0.0, 0.0, 100.0, 20.0), true);
        let hidden = entry(
            "hidden",
            WidgetRole::Label,
            rect(0.0, 0.0, 100.0, 20.0),
            false,
        );

        let registry = vec![scroll, row, hidden];
        let analysis = LayoutAnalysis::new(&registry, None);
        assert!(analysis.overlaps(&registry).is_empty());
    }

    #[test]
    fn overlap_checks_ignore_ancestor_descendant_widgets() {
        let mut panel = entry(
            "panel",
            WidgetRole::Unknown,
            rect(0.0, 0.0, 100.0, 100.0),
            true,
        );
        panel.parent_id = None;
        let child = entry(
            "child",
            WidgetRole::Button,
            rect(10.0, 10.0, 40.0, 30.0),
            true,
        );
        let sibling = entry(
            "sibling",
            WidgetRole::Button,
            rect(20.0, 10.0, 50.0, 30.0),
            true,
        );

        let registry = vec![panel, child, sibling];
        let analysis = LayoutAnalysis::new(&registry, None);
        let issues = analysis.overlaps(&registry);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].widgets, vec!["child", "sibling"]);
    }

    #[test]
    fn overlap_ancestry_resolves_through_widgets_outside_the_scope() {
        let mut canvas = entry(
            "canvas",
            WidgetRole::Unknown,
            rect(0.0, 0.0, 100.0, 100.0),
            true,
        );
        canvas.parent_id = None;
        let mut square = entry(
            "square",
            WidgetRole::Unknown,
            rect(0.0, 0.0, 20.0, 20.0),
            true,
        );
        square.parent_id = Some("canvas".to_string());

        let registry = vec![canvas, square];
        let analysis = LayoutAnalysis::new(&registry, None);
        // The scope excludes nothing here, and containment is intentional.
        assert!(analysis.overlaps(&registry).is_empty());
    }

    #[test]
    fn overlap_checks_ignore_widgets_in_separate_layers() {
        let panel = entry(
            "panel",
            WidgetRole::Label,
            rect(0.0, 0.0, 200.0, 200.0),
            true,
        );
        let mut window = entry(
            "window.label",
            WidgetRole::Label,
            rect(20.0, 20.0, 120.0, 60.0),
            true,
        );
        window.layer_id = "window_layer".to_string();
        window.parent_id = None;

        let registry = vec![panel, window];
        let analysis = LayoutAnalysis::new(&registry, None);
        assert!(analysis.overlaps(&registry).is_empty());
    }

    #[test]
    fn overlap_checks_report_siblings_in_the_same_layer() {
        let first = entry(
            "first",
            WidgetRole::Button,
            rect(0.0, 0.0, 60.0, 20.0),
            true,
        );
        let second = entry(
            "second",
            WidgetRole::Button,
            rect(30.0, 0.0, 90.0, 20.0),
            true,
        );

        let registry = vec![first, second];
        let analysis = LayoutAnalysis::new(&registry, None);
        assert_eq!(analysis.overlaps(&registry).len(), 1);
    }

    #[test]
    fn clipping_and_overflow_ignore_content_inside_a_scroll_area() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let scroll = viewport_filling_scroll(viewport_rect, 40.0, 500.0);
        let mut row = entry(
            "row",
            WidgetRole::Label,
            rect(0.0, 80.0, 100.0, 130.0),
            true,
        );
        row.parent_id = Some("scroll".to_string());
        row.layout = Some(WidgetLayout {
            clipped: true,
            overflow: true,
            visible_fraction: 0.4,
            ..layout(viewport_rect, viewport_rect)
        });

        let registry = vec![scroll, row.clone()];
        let scope = slice::from_ref(&row);
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.clipping(scope).is_empty());
        assert!(analysis.overflow(scope).is_empty());
    }

    #[test]
    fn overlap_checks_use_visible_rect_for_scroll_content() {
        let viewport_rect = rect(0.0, 0.0, 200.0, 200.0);
        let clip_rect = rect(100.0, 100.0, 180.0, 160.0);
        let toolbar = entry(
            "toolbar",
            WidgetRole::Button,
            rect(0.0, 0.0, 200.0, 30.0),
            true,
        );
        let mut scroll_text = entry(
            "scroll_text",
            WidgetRole::Label,
            rect(0.0, 0.0, 180.0, 140.0),
            true,
        );
        scroll_text.layout = Some(WidgetLayout {
            clipped: true,
            visible_fraction: 0.3,
            ..layout(clip_rect, viewport_rect)
        });

        let registry = vec![toolbar, scroll_text];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.overlaps(&registry).is_empty());
    }

    #[test]
    fn overflow_checks_ignore_post_layout_available_rects() {
        let viewport_rect = rect(0.0, 0.0, 100.0, 100.0);
        let mut button = entry(
            "button",
            WidgetRole::Button,
            rect(0.0, 0.0, 40.0, 20.0),
            true,
        );
        button.layout = Some(WidgetLayout {
            overflow: true,
            ..layout(viewport_rect, rect(50.0, 0.0, 100.0, 100.0))
        });

        let registry = vec![button];
        let analysis = LayoutAnalysis::new(&registry, Some(viewport_rect));
        assert!(analysis.overflow(&registry).is_empty());
    }

    #[test]
    fn ancestor_walk_terminates_on_a_parent_cycle() {
        let mut first = entry("first", WidgetRole::Label, rect(0.0, 0.0, 10.0, 10.0), true);
        first.parent_id = Some("second".to_string());
        let mut second = entry(
            "second",
            WidgetRole::Label,
            rect(0.0, 0.0, 10.0, 10.0),
            true,
        );
        second.parent_id = Some("first".to_string());

        let registry = vec![first.clone(), second];
        let analysis = LayoutAnalysis::new(&registry, None);
        assert!(!analysis.has_ancestor(&first, "missing"));
    }
}
