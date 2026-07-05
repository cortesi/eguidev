//! Checked-in Luau scripting definitions.

use eguidev::ScriptPrelude;

const SCRIPT_DEFINITIONS: &str = include_str!("../luau/eguidev.d.luau");

/// Return the checked-in Luau definitions that describe the scripting API.
pub fn script_definitions() -> &'static str {
    SCRIPT_DEFINITIONS
}

/// Return the checked-in Luau definitions plus app prelude declarations.
pub fn script_definitions_with_preludes(preludes: &[ScriptPrelude]) -> String {
    let mut definitions = SCRIPT_DEFINITIONS.trim_end().to_string();
    for prelude in preludes {
        if prelude.declarations.trim().is_empty() {
            continue;
        }
        definitions.push_str("\n\n-- App prelude: ");
        definitions.push_str(&prelude.namespace);
        definitions.push('\n');
        definitions.push_str(prelude.declarations.trim());
    }
    definitions.push('\n');
    definitions
}

/// Render the checked-in Luau definitions in a markdown code fence.
pub fn render_script_docs_markdown() -> String {
    format!("```luau\n{}\n```\n", SCRIPT_DEFINITIONS.trim())
}
