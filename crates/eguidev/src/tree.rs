//! Widget tree helpers based on parent ids.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::types::{Rect, WidgetRegistryEntry};

#[derive(Clone)]
struct IndexedWidget {
    key: String,
    parent_key: Option<String>,
    widget: WidgetRegistryEntry,
}

/// Collect a widget and all descendant widgets from a flat registry snapshot.
pub fn collect_subtree(
    widgets: &[WidgetRegistryEntry],
    root: &WidgetRegistryEntry,
) -> Vec<WidgetRegistryEntry> {
    let indexed = index_widgets(widgets);
    let children_map = build_children_map(&indexed);
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    let Some(root) = find_indexed_widget(&indexed, root) else {
        return result;
    };
    let mut visited = HashSet::new();
    visited.insert(root.key.clone());
    result.push(root.widget.clone());
    queue.push_back(root.key.clone());
    while let Some(parent_key) = queue.pop_front() {
        let children = children_map
            .get(&Some(parent_key))
            .cloned()
            .unwrap_or_default();
        for child in children {
            // Parent ids come from instrumentation and can describe a cycle.
            // Visiting each key once keeps the walk finite.
            if !visited.insert(child.key.clone()) {
                continue;
            }
            queue.push_back(child.key.clone());
            result.push(child.widget);
        }
    }
    result
}

fn build_children_map(indexed: &[IndexedWidget]) -> HashMap<Option<String>, Vec<IndexedWidget>> {
    let mut map: HashMap<Option<String>, Vec<IndexedWidget>> = HashMap::new();
    for widget in indexed {
        map.entry(widget.parent_key.clone())
            .or_default()
            .push(widget.clone());
    }
    map
}

fn index_widgets(widgets: &[WidgetRegistryEntry]) -> Vec<IndexedWidget> {
    // A container is recorded after the contents it wraps, so parents resolve
    // against the whole snapshot rather than the entries seen so far.
    let parent_indices = resolve_parent_indices(widgets);
    let mut keys: Vec<Option<String>> = vec![None; widgets.len()];
    let mut sibling_counts: HashMap<(Option<String>, String), usize> = HashMap::new();
    for index in 0..widgets.len() {
        assign_key(
            index,
            widgets,
            &parent_indices,
            &mut keys,
            &mut sibling_counts,
        );
    }
    widgets
        .iter()
        .enumerate()
        .map(|(index, widget)| IndexedWidget {
            key: keys[index].clone().unwrap_or_else(|| widget.id.clone()),
            parent_key: parent_indices[index].and_then(|parent| keys[parent].clone()),
            widget: widget.clone(),
        })
        .collect()
}

/// Assign scoped keys along the widget's parent chain, outermost first.
fn assign_key(
    index: usize,
    widgets: &[WidgetRegistryEntry],
    parent_indices: &[Option<usize>],
    keys: &mut [Option<String>],
    sibling_counts: &mut HashMap<(Option<String>, String), usize>,
) {
    let mut chain = Vec::new();
    let mut current = Some(index);
    while let Some(entry) = current {
        if keys[entry].is_some() || chain.contains(&entry) {
            break;
        }
        chain.push(entry);
        current = parent_indices[entry];
    }
    for &entry in chain.iter().rev() {
        let parent_key = parent_indices[entry].and_then(|parent| keys[parent].clone());
        let ordinal = sibling_counts
            .entry((parent_key.clone(), widgets[entry].id.clone()))
            .or_default();
        *ordinal += 1;
        keys[entry] = Some(scoped_key(
            parent_key.as_deref(),
            &widgets[entry].id,
            *ordinal,
        ));
    }
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

fn scoped_key(parent_key: Option<&str>, id: &str, ordinal: usize) -> String {
    match parent_key {
        Some(parent_key) => format!("{parent_key}/{id}#{ordinal}"),
        None => format!("{id}#{ordinal}"),
    }
}

fn find_indexed_widget<'a>(
    indexed: &'a [IndexedWidget],
    root: &WidgetRegistryEntry,
) -> Option<&'a IndexedWidget> {
    indexed
        .iter()
        .find(|entry| same_widget(&entry.widget, root))
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
            layer_id: "layer".to_string(),
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
