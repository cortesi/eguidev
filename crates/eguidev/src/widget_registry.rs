//! Widget registry for tracking egui widgets across frames.
#![allow(missing_docs)]

use std::{
    collections::{HashMap, HashSet},
    mem,
    sync::Mutex,
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    error::{ErrorCode, ToolError},
    registry::{lock, viewport_id_to_string},
    types::{WidgetLayout, WidgetRef, WidgetRegistryEntry, WidgetRoleMeta, WidgetValue},
    viewports::{ViewportSnapshot, ViewportState},
};

/// Metadata for a widget used during tracking and layout analysis.
#[derive(Debug, Clone, Default)]
pub struct WidgetMeta {
    /// Role taxonomy entry with the metadata that the role requires.
    pub role: WidgetRoleMeta,
    /// Optional label.
    pub label: Option<String>,
    /// Optional widget value for stateful controls.
    pub value: Option<WidgetValue>,
    /// Structured app-domain metadata attached to this widget.
    pub data: Option<Value>,
    /// Optional layout metadata.
    pub layout: Option<WidgetLayout>,
    /// Whether the widget is visible to automation.
    ///
    /// This defaults to `false`, which hides the widget from every default
    /// lookup, so set it on every recorded widget. Response-backed helpers pass
    /// `ui.is_visible() && ui.is_rect_visible(rect)`; a painter-drawn rect has
    /// no other visibility signal, so it must state its own.
    pub visible: bool,
    /// Whether the widget accepts interaction.
    ///
    /// `None` takes the value from the `egui::Response` when there is one, and
    /// treats a response-free published rect as enabled.
    pub enabled: Option<bool>,
    /// Optional explicit rect override.
    pub rect: Option<egui::Rect>,
    /// Optional explicit interaction rect override.
    pub interact_rect: Option<egui::Rect>,
}

impl WidgetMeta {
    /// Attach structured app-domain data from any serializable value.
    pub fn with_data<T: Serialize>(self, data: T) -> Self {
        let data =
            serde_json::to_value(data).unwrap_or_else(|error| widget_data_error_value(&error));
        Self {
            data: normalize_widget_data(Some(data)),
            ..self
        }
    }
}

#[derive(Debug, Clone)]
struct DuplicateExplicitIdFault {
    duplicate_ids: Vec<String>,
}

const MAX_CANDIDATE_SUMMARIES: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateScore {
    rank: usize,
    distance: usize,
    length_delta: usize,
}

pub struct WidgetRegistry {
    registry_current: Mutex<HashMap<egui::ViewportId, Vec<WidgetRegistryEntry>>>,
    registry_snapshot: Mutex<HashMap<egui::ViewportId, Vec<WidgetRegistryEntry>>>,
    duplicate_explicit_id_fault: Mutex<Option<DuplicateExplicitIdFault>>,
    container_stack: Mutex<HashMap<egui::ViewportId, Vec<String>>>,
}

impl Default for WidgetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetRegistry {
    pub fn new() -> Self {
        Self {
            registry_current: Mutex::new(HashMap::new()),
            registry_snapshot: Mutex::new(HashMap::new()),
            duplicate_explicit_id_fault: Mutex::new(None),
            container_stack: Mutex::new(HashMap::new()),
        }
    }

    pub fn clear_registry(&self, viewport_id: egui::ViewportId) {
        let mut registry = lock(&self.registry_current, "registry lock");
        registry.insert(viewport_id, Vec::new());
        let mut containers = lock(&self.container_stack, "container stack lock");
        containers.insert(viewport_id, Vec::new());
    }

    pub fn finalize_registry(&self, viewport_id: egui::ViewportId) {
        let mut current = lock(&self.registry_current, "registry lock");
        let mut snapshot = lock(&self.registry_snapshot, "registry snapshot lock");
        if let Some(entries) = current.remove(&viewport_id) {
            snapshot.insert(viewport_id, entries);
        }
        let fault = build_duplicate_explicit_id_fault(&snapshot);
        *lock(
            &self.duplicate_explicit_id_fault,
            "duplicate explicit id fault lock",
        ) = fault;
    }

    pub fn record_widget(&self, viewport_id: egui::ViewportId, entry: WidgetRegistryEntry) {
        let mut registry = lock(&self.registry_current, "registry lock");
        registry.entry(viewport_id).or_default().push(entry);
    }

    pub fn push_container(&self, viewport_id: egui::ViewportId, id: String) {
        let mut stack = lock(&self.container_stack, "container stack lock");
        stack.entry(viewport_id).or_default().push(id);
    }

    pub fn pop_container(&self, viewport_id: egui::ViewportId) {
        let mut stack = lock(&self.container_stack, "container stack lock");
        if let Some(stack) = stack.get_mut(&viewport_id) {
            stack.pop();
        }
    }

    pub fn current_container(&self, viewport_id: egui::ViewportId) -> Option<String> {
        let stack = lock(&self.container_stack, "container stack lock");
        stack
            .get(&viewport_id)
            .and_then(|stack| stack.last().cloned())
    }

    pub fn widget_list(&self, viewport_id: egui::ViewportId) -> Vec<WidgetRegistryEntry> {
        lock(&self.registry_snapshot, "registry snapshot lock")
            .get(&viewport_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn child_ids_of(&self, viewport_id: egui::ViewportId, parent_id: &str) -> Vec<String> {
        lock(&self.registry_snapshot, "registry snapshot lock")
            .get(&viewport_id)
            .into_iter()
            .flatten()
            .filter(|entry| entry.parent_id.as_deref() == Some(parent_id))
            .map(|entry| entry.id.clone())
            .collect()
    }

    pub fn duplicate_explicit_id_error(&self, viewports: &ViewportState) -> Option<ToolError> {
        let fault = lock(
            &self.duplicate_explicit_id_fault,
            "duplicate explicit id fault lock",
        )
        .clone()?;
        let snapshot = lock(&self.registry_snapshot, "registry snapshot lock");
        Some(fault.into_tool_error(viewports, &snapshot))
    }

    pub fn resolve_widget(
        &self,
        viewports: &ViewportState,
        viewport_id: Option<&str>,
        target: &WidgetRef,
    ) -> Result<WidgetRegistryEntry, ToolError> {
        if let Some(error) = self.duplicate_explicit_id_error(viewports) {
            return Err(error);
        }
        let tool_viewport = viewport_id;
        let registry = lock(&self.registry_snapshot, "registry snapshot lock");

        let (matches, resolved_viewport) =
            match resolve_viewport_selector(viewports, tool_viewport, target) {
                Ok((viewport_id, resolved_viewport)) => {
                    let matches = registry
                        .get(&viewport_id)
                        .into_iter()
                        .flatten()
                        .filter(|entry| entry.id == target.id)
                        .cloned()
                        .collect::<Vec<_>>();
                    (matches, resolved_viewport)
                }
                Err(error) => return Err(error),
            };

        if matches.is_empty() {
            return Err(not_found_error(
                "Widget not found for id",
                target,
                tool_viewport,
                &resolved_viewport,
                &registry,
                viewports,
            ));
        }
        if matches.len() > 1 {
            return Err(ambiguous_error(
                "ambiguous",
                "Widget reference is ambiguous",
                target,
                tool_viewport,
                &resolved_viewport,
                &matches,
            ));
        }
        Ok(matches.into_iter().next().expect("single id match"))
    }

    pub fn resolve_widget_global(
        &self,
        viewports: &ViewportState,
        target: &WidgetRef,
    ) -> Result<WidgetRegistryEntry, ToolError> {
        if target.viewport_id.is_some() {
            return self.resolve_widget(viewports, None, target);
        }
        if let Some(error) = self.duplicate_explicit_id_error(viewports) {
            return Err(error);
        }
        let registry = lock(&self.registry_snapshot, "registry snapshot lock");

        let matches = registry
            .values()
            .flatten()
            .filter(|entry| entry.id == target.id)
            .cloned()
            .collect::<Vec<_>>();

        if matches.is_empty() {
            return Err(not_found_error(
                "Widget not found for id",
                target,
                None,
                "all",
                &registry,
                viewports,
            ));
        }
        if matches.len() > 1 {
            return Err(ambiguous_error(
                "ambiguous",
                "Widget reference is ambiguous",
                target,
                None,
                "all",
                &matches,
            ));
        }
        Ok(matches.into_iter().next().expect("single id match"))
    }
}

pub fn record_widget(
    widgets: &WidgetRegistry,
    id: String,
    response: &egui::Response,
    meta: WidgetMeta,
) {
    let viewport_id = response.ctx.viewport_id();
    let parent_id = widgets.current_container(viewport_id);
    record_widget_entry(
        widgets,
        WidgetEntryInput {
            id,
            ctx: &response.ctx,
            viewport_id,
            layer_id: response.layer_id,
            native_id: response.id.value(),
            rect: meta.rect.unwrap_or(response.rect),
            interact_rect: meta.interact_rect.unwrap_or(response.interact_rect),
            enabled: meta.enabled.unwrap_or_else(|| response.enabled()),
            meta,
            parent_id,
            focused: response.ctx.memory(|mem| mem.has_focus(response.id)),
        },
    );
}

pub fn record_rect_meta(
    widgets: &WidgetRegistry,
    id: String,
    ui: &egui::Ui,
    rect: egui::Rect,
    meta: WidgetMeta,
) {
    let viewport_id = ui.ctx().viewport_id();
    let parent_id = widgets.current_container(viewport_id);
    let native_id = egui::Id::new(id.as_str()).value();
    record_widget_entry(
        widgets,
        WidgetEntryInput {
            id,
            ctx: ui.ctx(),
            viewport_id,
            layer_id: ui.layer_id(),
            native_id,
            rect: meta.rect.unwrap_or(rect),
            interact_rect: meta.interact_rect.unwrap_or(rect),
            enabled: meta.enabled.unwrap_or(true),
            meta,
            parent_id,
            focused: false,
        },
    );
}

struct WidgetEntryInput<'a> {
    id: String,
    ctx: &'a egui::Context,
    viewport_id: egui::ViewportId,
    layer_id: egui::LayerId,
    native_id: u64,
    rect: egui::Rect,
    interact_rect: egui::Rect,
    meta: WidgetMeta,
    parent_id: Option<String>,
    enabled: bool,
    focused: bool,
}

/// Map one egui layer order onto a comparable paint rank, where a larger value paints later.
fn layer_paint_order(order: egui::Order) -> u8 {
    match order {
        egui::Order::Background => 0,
        egui::Order::Middle => 1,
        egui::Order::Foreground => 2,
        egui::Order::Tooltip => 3,
        egui::Order::Debug => 4,
    }
}

fn record_widget_entry(widgets: &WidgetRegistry, input: WidgetEntryInput<'_>) {
    let WidgetEntryInput {
        id,
        ctx,
        viewport_id,
        layer_id,
        native_id,
        rect,
        interact_rect,
        meta,
        parent_id,
        enabled,
        focused,
    } = input;
    let value = normalize_widget_value(meta.value);
    let data = normalize_widget_data(meta.data);
    let (rect, interact_rect) = if let Some(to_global) = ctx.layer_transform_to_global(layer_id) {
        (to_global.mul_rect(rect), to_global.mul_rect(interact_rect))
    } else {
        (rect, interact_rect)
    };
    let entry = WidgetRegistryEntry {
        id,
        explicit_id: true,
        native_id,
        viewport_id: viewport_id_to_string(viewport_id),
        layer_id: format!("{layer_id:?}"),
        layer_order: layer_paint_order(layer_id.order),
        rect: rect.into(),
        interact_rect: interact_rect.into(),
        role: meta.role.role(),
        role_state: meta.role.state(),
        label: meta.label,
        value,
        data,
        layout: meta.layout,
        parent_id,
        enabled,
        visible: meta.visible,
        focused,
    };
    widgets.record_widget(viewport_id, entry);
}

fn normalize_widget_data(data: Option<Value>) -> Option<Value> {
    const MAX_WIDGET_DATA_BYTES: usize = 16 * 1024;

    let data = data?;
    let byte_len = serde_json::to_vec(&data)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_WIDGET_DATA_BYTES + 1);
    if byte_len <= MAX_WIDGET_DATA_BYTES {
        return Some(data);
    }
    Some(json!({
        "_eguidev_truncated": true,
        "bytes": byte_len,
    }))
}

fn widget_data_error_value(error: &serde_json::Error) -> Value {
    json!({
        "_eguidev_error": "serialize",
        "message": error.to_string(),
    })
}

fn normalize_widget_value(value: Option<WidgetValue>) -> Option<WidgetValue> {
    const MAX_TEXT_CHARS: usize = 10_000;
    const TRUNCATION_SUFFIX: &str = "...";

    match value {
        Some(WidgetValue::Text(text)) => {
            let mut chars = text.chars();
            if chars.clone().count() <= MAX_TEXT_CHARS {
                return Some(WidgetValue::Text(text));
            }
            let keep = MAX_TEXT_CHARS.saturating_sub(TRUNCATION_SUFFIX.chars().count());
            let mut truncated = chars.by_ref().take(keep).collect::<String>();
            truncated.push_str(TRUNCATION_SUFFIX);
            Some(WidgetValue::Text(truncated))
        }
        Some(WidgetValue::Float(v)) if !v.is_finite() => None,
        other => other,
    }
}

fn resolve_viewport_selector(
    viewports: &ViewportState,
    tool_viewport: Option<&str>,
    target: &WidgetRef,
) -> Result<(egui::ViewportId, String), ToolError> {
    let tool_resolved = resolve_viewport_id(viewports, tool_viewport)?;
    let target_resolved = resolve_viewport_id(viewports, target.viewport_id.as_deref())?;

    match (tool_resolved, target_resolved) {
        (Some(tool_id), Some(target_id)) => {
            if tool_id != target_id {
                let details = selector_details(target, tool_viewport, None);
                return Err(ToolError::new(
                    ErrorCode::NotFound,
                    format!(
                        "Conflicting viewport selectors (tool={tool_viewport:?}, target={:?})",
                        target.viewport_id.as_deref()
                    ),
                )
                .with_details(json!({
                    "reason": "conflict",
                    "selectors": selectors_value(&details),
                })));
            }
            Ok((tool_id, viewport_id_to_string(tool_id)))
        }
        (Some(tool_id), None) => Ok((tool_id, viewport_id_to_string(tool_id))),
        (None, Some(target_id)) => Ok((target_id, viewport_id_to_string(target_id))),
        (None, None) => Ok((egui::ViewportId::ROOT, "root".to_string())),
    }
}

fn selector_details(
    target: &WidgetRef,
    tool_viewport: Option<&str>,
    resolved_viewport: Option<&str>,
) -> serde_json::Value {
    json!({
        "selectors": {
            "id": target.id,
            "viewport_id": target.viewport_id.as_deref(),
            "tool_viewport_id": tool_viewport,
            "resolved_viewport_id": resolved_viewport,
        }
    })
}

fn ambiguous_error(
    reason: &str,
    message: &str,
    target: &WidgetRef,
    tool_viewport: Option<&str>,
    resolved_viewport: &str,
    candidates: &[WidgetRegistryEntry],
) -> ToolError {
    let (summaries, truncated) = summarize_candidates(candidates);
    let details = selector_details(target, tool_viewport, Some(resolved_viewport));
    let message = format!(
        "{message} (id={:?}, viewport={resolved_viewport})",
        target.id
    );
    ToolError::new(ErrorCode::NotFound, message).with_details(json!({
        "reason": reason,
        "selectors": selectors_value(&details),
        "candidates": summaries,
        "candidates_truncated": truncated,
    }))
}

fn not_found_error(
    message: &str,
    target: &WidgetRef,
    tool_viewport: Option<&str>,
    resolved_viewport: &str,
    registry: &HashMap<egui::ViewportId, Vec<WidgetRegistryEntry>>,
    viewports: &ViewportState,
) -> ToolError {
    let message = format!(
        "{message} (id={:?}, viewport={resolved_viewport})",
        target.id
    );
    let details = selector_details(target, tool_viewport, Some(resolved_viewport));
    ToolError::new(ErrorCode::NotFound, message).with_details(json!({
        "selectors": selectors_value(&details),
        "search": missing_widget_search(target, resolved_viewport, registry, viewports),
    }))
}

fn summarize_candidates(candidates: &[WidgetRegistryEntry]) -> (Vec<serde_json::Value>, bool) {
    let truncated = candidates.len() > MAX_CANDIDATE_SUMMARIES;
    let summaries = candidates
        .iter()
        .take(MAX_CANDIDATE_SUMMARIES)
        .map(|entry| {
            json!({
                "id": entry.id,
                "viewport_id": entry.viewport_id,
                "role": entry.role,
            })
        })
        .collect();
    (summaries, truncated)
}

fn build_duplicate_explicit_id_fault(
    snapshot: &HashMap<egui::ViewportId, Vec<WidgetRegistryEntry>>,
) -> Option<DuplicateExplicitIdFault> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for entry in snapshot.values().flatten() {
        if entry.explicit_id {
            *counts.entry(entry.id.as_str()).or_insert(0) += 1;
        }
    }
    let mut duplicate_ids = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| id.to_string())
        .collect::<Vec<_>>();
    duplicate_ids.sort();
    (!duplicate_ids.is_empty()).then_some(DuplicateExplicitIdFault { duplicate_ids })
}

impl DuplicateExplicitIdFault {
    fn into_tool_error(
        self,
        viewports: &ViewportState,
        snapshot: &HashMap<egui::ViewportId, Vec<WidgetRegistryEntry>>,
    ) -> ToolError {
        let Self { duplicate_ids } = self;
        let summary = duplicate_ids
            .iter()
            .take(5)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if duplicate_ids.len() > 5 {
            format!(", +{} more", duplicate_ids.len() - 5)
        } else {
            String::new()
        };
        let message = format!(
            "Duplicate explicit widget ids detected: {summary}{suffix}; make ids unique or scope one side with container() before continuing automation"
        );
        let flat: Vec<WidgetRegistryEntry> = snapshot.values().flatten().cloned().collect();
        let duplicate_payload = duplicate_ids
            .into_iter()
            .map(|id| {
                let candidates = flat
                    .iter()
                    .filter(|entry| entry.explicit_id && entry.id == id)
                    .map(|candidate| {
                        json!({
                            "viewport": viewport_summary(viewports, &candidate.viewport_id),
                            "role": candidate.role,
                            "label": candidate.label,
                            "rect": candidate.rect,
                            "parent_chain": parent_chain_for(candidate, &flat),
                        })
                    })
                    .collect::<Vec<_>>();
                json!({ "id": id, "candidates": candidates })
            })
            .collect::<Vec<_>>();
        ToolError::new(ErrorCode::InstrumentationFault, message).with_details(json!({
            "reason": "duplicate_explicit_widget_ids",
            "duplicate_ids": duplicate_payload,
        }))
    }
}

fn missing_widget_search(
    target: &WidgetRef,
    resolved_viewport: &str,
    registry: &HashMap<egui::ViewportId, Vec<WidgetRegistryEntry>>,
    viewports: &ViewportState,
) -> serde_json::Value {
    let target_id = target.id.as_str();
    let restrict_to =
        (resolved_viewport != "root" && resolved_viewport != "all").then_some(resolved_viewport);
    let mut all_candidates: Vec<(CandidateScore, String)> = Vec::new();
    let mut exact_matches = Vec::new();
    let mut summaries = registry
        .iter()
        .filter_map(|(viewport_id, entries)| {
            let viewport_id = viewport_id_to_string(*viewport_id);
            if restrict_to.is_some_and(|resolved| resolved != viewport_id) {
                return None;
            }
            let mut candidates = entries
                .iter()
                .filter_map(|entry| {
                    scored_candidate(target_id, &entry.id).map(|score| (score, entry))
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|(left_score, left), (right_score, right)| {
                left_score
                    .cmp(right_score)
                    .then_with(|| left.id.cmp(&right.id))
            });
            for (score, entry) in &candidates {
                all_candidates.push((*score, entry.id.clone()));
            }
            let near_misses = candidates
                .into_iter()
                .take(MAX_CANDIDATE_SUMMARIES)
                .map(|(score, entry)| {
                    if score.rank == 0 {
                        exact_matches.push(json!({
                            "viewport": viewport_summary(viewports, &viewport_id),
                            "id": entry.id,
                            "role": entry.role,
                            "label": entry.label,
                        }));
                    }
                    json!({
                        "id": entry.id,
                        "role": entry.role,
                        "label": entry.label,
                        "match": candidate_match_kind(score),
                    })
                })
                .collect::<Vec<_>>();
            Some(json!({
                "viewport": viewport_summary(viewports, &viewport_id),
                "widget_count": entries.len(),
                "near_misses": near_misses,
            }))
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        left["viewport"]["id"]
            .as_str()
            .cmp(&right["viewport"]["id"].as_str())
    });
    exact_matches.sort_by(|left, right| {
        left["viewport"]["id"]
            .as_str()
            .cmp(&right["viewport"]["id"].as_str())
            .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
    });
    all_candidates.sort_by(|(left_score, left), (right_score, right)| {
        left_score.cmp(right_score).then_with(|| left.cmp(right))
    });
    let mut suggestions = Vec::new();
    for (_, id) in all_candidates {
        if !suggestions.contains(&id) {
            suggestions.push(id);
        }
        if suggestions.len() >= MAX_CANDIDATE_SUMMARIES {
            break;
        }
    }
    json!({
        "viewports": summaries,
        "exact_matches": exact_matches.into_iter().take(MAX_CANDIDATE_SUMMARIES).collect::<Vec<_>>(),
        "suggestions": suggestions,
    })
}

fn scored_candidate(target: &str, candidate: &str) -> Option<CandidateScore> {
    if target == candidate {
        return Some(CandidateScore {
            rank: 0,
            distance: 0,
            length_delta: 0,
        });
    }
    if let Some(distance) = edit_distance_at_most(target, candidate, 2) {
        return Some(CandidateScore {
            rank: 1,
            distance,
            length_delta: target.len().abs_diff(candidate.len()),
        });
    }
    if target.starts_with(candidate) || candidate.starts_with(target) {
        return Some(CandidateScore {
            rank: 2,
            distance: usize::MAX,
            length_delta: target.len().abs_diff(candidate.len()),
        });
    }
    None
}

fn candidate_match_kind(score: CandidateScore) -> &'static str {
    match score.rank {
        0 => "exact",
        1 => "edit_distance",
        _ => "prefix",
    }
}

fn edit_distance_at_most(left: &str, right: &str, limit: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > limit {
        return None;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (i, left_byte) in left.bytes().enumerate() {
        current[0] = i + 1;
        let mut row_min = current[0];
        for (j, right_byte) in right.bytes().enumerate() {
            let substitution = previous[j] + usize::from(left_byte != right_byte);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
            row_min = row_min.min(current[j + 1]);
        }
        if row_min > limit {
            return None;
        }
        mem::swap(&mut previous, &mut current);
    }
    let distance = previous[right.len()];
    (distance <= limit).then_some(distance)
}

fn parent_chain_for(
    candidate: &WidgetRegistryEntry,
    snapshot: &[WidgetRegistryEntry],
) -> Vec<String> {
    let by_id = snapshot
        .iter()
        .filter(|entry| entry.viewport_id == candidate.viewport_id)
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut parent_id = candidate.parent_id.as_deref();
    while let Some(id) = parent_id {
        if !seen.insert(id) {
            break;
        }
        chain.push(id.to_string());
        parent_id = by_id.get(id).and_then(|entry| entry.parent_id.as_deref());
    }
    chain
}

fn viewport_summary(viewports: &ViewportState, viewport_id: &str) -> serde_json::Value {
    let snapshot = viewports
        .viewports_snapshot()
        .into_iter()
        .find(|snapshot| snapshot.viewport_id == viewport_id);
    viewport_summary_value(viewport_id, snapshot.as_ref())
}

fn viewport_summary_value(
    viewport_id: &str,
    snapshot: Option<&ViewportSnapshot>,
) -> serde_json::Value {
    json!({
        "id": viewport_id,
        "name": snapshot.and_then(|snapshot| snapshot.name.clone()),
        "title": snapshot.and_then(|snapshot| snapshot.title.clone()),
    })
}

fn resolve_viewport_id(
    viewports: &ViewportState,
    selector: Option<&str>,
) -> Result<Option<egui::ViewportId>, ToolError> {
    selector
        .map(|value| viewports.resolve_viewport_id(Some(value.to_string())))
        .transpose()
}

fn selectors_value(details: &serde_json::Value) -> serde_json::Value {
    details
        .get("selectors")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

#[cfg(test)]
mod tests {
    use serde::{
        Serialize, Serializer,
        ser::{Error as SerError, SerializeMap},
    };
    use serde_json::{Value, json};

    use super::{WidgetMeta, WidgetRegistry, normalize_widget_data};

    struct FailingData;

    impl Serialize for FailingData {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("intentional data failure"))
        }
    }

    struct NonFiniteData;

    impl Serialize for NonFiniteData {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry("nan", &f64::NAN)?;
            map.end()
        }
    }

    #[test]
    fn widget_data_is_truncated_when_compact_json_is_too_large() {
        let value = json!({
            "payload": "x".repeat(17 * 1024),
        });

        assert_eq!(
            normalize_widget_data(Some(value)),
            Some(json!({
                "_eguidev_truncated": true,
                "bytes": 17422,
            }))
        );
    }

    #[test]
    fn with_data_marks_serialization_failures() {
        let meta = WidgetMeta::default().with_data(FailingData);
        let data = meta.data.expect("error marker");

        assert_eq!(data["_eguidev_error"], "serialize");
        assert!(
            data["message"]
                .as_str()
                .expect("message")
                .contains("intentional data failure")
        );
    }

    #[test]
    fn with_data_sanitizes_non_finite_numbers_to_null() {
        let meta = WidgetMeta::default().with_data(NonFiniteData);

        assert_eq!(meta.data, Some(json!({ "nan": Value::Null })));
    }

    #[test]
    fn duplicate_explicit_id_error_stays_finite_when_parent_ids_cycle() {
        use crate::{
            error::ErrorCode,
            types::{Pos2, Rect, WidgetRef, WidgetRegistryEntry, WidgetRole},
            viewports::ViewportState,
        };

        fn entry(id: &str, parent_id: Option<&str>) -> WidgetRegistryEntry {
            let rect = Rect {
                min: Pos2 { x: 0.0, y: 0.0 },
                max: Pos2 { x: 1.0, y: 1.0 },
            };
            WidgetRegistryEntry {
                id: id.to_string(),
                explicit_id: true,
                native_id: 1,
                viewport_id: "root".to_string(),
                layer_id: "background".to_string(),
                layer_order: 0,
                rect,
                interact_rect: rect,
                role: WidgetRole::Label,
                label: None,
                value: None,
                data: None,
                layout: None,
                role_state: None,
                parent_id: parent_id.map(str::to_string),
                enabled: true,
                visible: true,
                focused: false,
            }
        }

        let registry = WidgetRegistry::new();
        registry.record_widget(egui::ViewportId::ROOT, entry("dup", None));
        registry.record_widget(egui::ViewportId::ROOT, entry("dup", Some("dup")));
        registry.finalize_registry(egui::ViewportId::ROOT);
        let viewports = ViewportState::new();
        let error = registry
            .duplicate_explicit_id_error(&viewports)
            .expect("duplicate id fault");
        assert_eq!(error.code(), ErrorCode::InstrumentationFault);
        let chain =
            error.details().expect("details")["duplicate_ids"][0]["candidates"][1]["parent_chain"]
                .as_array()
                .expect("parent_chain");
        assert!(!chain.is_empty());
        let resolved = registry.resolve_widget(
            &viewports,
            None,
            &WidgetRef {
                id: "dup".to_string(),
                viewport_id: None,
            },
        );
        assert_eq!(
            resolved.expect_err("duplicate id").code(),
            ErrorCode::InstrumentationFault
        );
    }
}
