//! Checked-in Luau scripting definitions.

const SCRIPT_DEFINITIONS: &str = include_str!("../luau/eguidev.d.luau");

/// Return the checked-in Luau definitions that describe the scripting API.
pub fn script_definitions() -> &'static str {
    SCRIPT_DEFINITIONS
}
