//! The crate root names each type that its exported types use.

#[cfg(test)]
mod tests {
    use eguidev::{
        DataCondition, Modifiers, ScrollAtCondition, Vec2, WidgetCondition, WidgetRegistryEntry,
    };

    #[test]
    fn a_condition_names_its_parts_from_the_crate_root() {
        let condition = WidgetCondition {
            scroll_at: Some(ScrollAtCondition {
                offset: Vec2 { x: 0.0, y: 40.0 },
                tolerance: 0.5,
            }),
            data: Some(DataCondition {
                pointer: "/state".to_string(),
                equals: serde_json::json!("ready"),
            }),
            ..WidgetCondition::default()
        };
        assert!(condition.scroll_at.is_some() && condition.data.is_some());
        let modifiers = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(modifiers.shift && !modifiers.ctrl);
        let entry: Option<WidgetRegistryEntry> = None;
        assert!(entry.is_none());
    }
}
