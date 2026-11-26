//! Widget tree helpers based on parent ids.

use std::collections::{HashSet, VecDeque};

use crate::types::{Rect, WidgetRegistryEntry};

/// Collect a widget and all descendant widgets from a flat registry snapshot.
pub fn collect_subtree(
    widgets: &[WidgetRegistryEntry],
    root: &WidgetRegistryEntry,
) -> Vec<WidgetRegistryEntry> {
    let Some(root) = widgets.iter().position(|widget| same_widget(widget, root)) else {
        return Vec::new();
    };
    // A container is recorded after the contents it wraps, so parents resolve
    // against the whole snapshot rather than the entries seen so far.
    let mut children = vec![Vec::new(); widgets.len()];
    for (index, parent) in resolve_parent_indices(widgets).into_iter().enumerate() {
        if let Some(parent) = parent {
            children[parent].push(index);
        }
    }
    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut result = vec![widgets[root].clone()];
    while let Some(parent) = queue.pop_front() {
        for &child in &children[parent] {
            // Parent ids come from instrumentation and can describe a cycle.
            // Visiting each widget once keeps the walk finite.
            if visited.insert(child) {
                queue.push_back(child);
                result.push(widgets[child].clone());
            }
        }
    }
    result
}

/// Resolve each widget's parent to a registry position.
///
/// A containing rect breaks a tie between same-id candidates, which keeps a
/// repeated container id bound to the instance that actually holds the child.
fn resolve_parent_indices(widgets: &[WidgetRegistryEntry]) -> Vec<Option<usize>> {
    widgets
        .iter()
        .enumerate()
        .map(|(index, widget)| {
            let parent_id = widget.parent_id.as_deref()?;
            let candidates = || {
                widgets
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(move |(candidate, entry)| *candidate != index && entry.id == parent_id)
            };
            candidates()
                .find(|(_, entry)| rect_contains(entry.rect, widget.rect))
                .or_else(|| candidates().next())
                .map(|(candidate, _)| candidate)
        })
        .collect()
}

fn same_widget(left: &WidgetRegistryEntry, right: &WidgetRegistryEntry) -> bool {
    left.native_id == right.native_id
        && left.id == right.id
        && left.parent_id == right.parent_id
        && left.viewport_id == right.viewport_id
}

fn rect_contains(parent: Rect, child: Rect) -> bool {
    parent.min.x <= child.min.x
        && parent.min.y <= child.min.y
        && parent.max.x >= child.max.x
        && parent.max.y >= child.max.y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Pos2, WidgetRole};

    fn rect(min_y: f32, max_y: f32) -> Rect {
        Rect {
            min: Pos2 { x: 0.0, y: min_y },
            max: Pos2 { x: 100.0, y: max_y },
        }
    }

    fn entry(id: &str, native_id: u64, parent: Option<&str>, rect: Rect) -> WidgetRegistryEntry {
        WidgetRegistryEntry {
            id: id.to_string(),
            explicit_id: true,
            native_id,
            viewport_id: "root".to_string(),
            layer_id: egui::LayerId::background(),
            layer_order: 0,
            rect,
            interact_rect: rect,
            role: WidgetRole::Unknown,
            label: None,
            value: None,
            data: None,
            layout: None,
            role_state: None,
            parent_id: parent.map(ToString::to_string),
            enabled: true,
            visible: true,
            focused: false,
            covered: false,
        }
    }

    fn ids(widgets: &[WidgetRegistryEntry]) -> Vec<&str> {
        widgets.iter().map(|widget| widget.id.as_str()).collect()
    }

    #[test]
    fn collects_children_of_a_container_recorded_after_them() {
        // A scroll area records itself after the contents it wraps.
        let widgets = vec![
            entry("row.0", 1, Some("scroll"), rect(0.0, 10.0)),
            entry("row.1", 2, Some("scroll"), rect(10.0, 20.0)),
            entry("scroll", 3, None, rect(0.0, 100.0)),
        ];
        let subtree = collect_subtree(&widgets, &widgets[2]);
        assert_eq!(ids(&subtree), vec!["scroll", "row.0", "row.1"]);
    }

    #[test]
    fn collects_a_nested_subtree_in_registry_order() {
        let widgets = vec![
            entry("panel", 1, None, rect(0.0, 100.0)),
            entry("outer", 2, Some("panel"), rect(0.0, 60.0)),
            entry("leaf", 3, Some("inner"), rect(0.0, 10.0)),
            entry("inner", 4, Some("outer"), rect(0.0, 30.0)),
        ];
        let subtree = collect_subtree(&widgets, &widgets[1]);
        assert_eq!(ids(&subtree), vec!["outer", "inner", "leaf"]);
    }

    #[test]
    fn a_parent_cycle_does_not_hang_the_walk() {
        let mut first = entry("first", 1, Some("second"), rect(0.0, 10.0));
        first.parent_id = Some("second".to_string());
        let second = entry("second", 2, Some("first"), rect(0.0, 10.0));
        let widgets = vec![first, second];
        let subtree = collect_subtree(&widgets, &widgets[0]);
        assert!(subtree.iter().any(|widget| widget.id == "first"));
    }
}
