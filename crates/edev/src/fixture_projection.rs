//! Checked-in Luau projections for shared fixture records.

use eguidev::FixtureSpec;
use eguidev_runtime::ScriptEvalOutcome;

use crate::EdevError;

/// Convert public fixture references to Rust metadata records.
pub const FIXTURE_LIST_SCRIPT: &str = include_str!("../luau/fixture_list.luau");
/// Apply one fixture through `script_eval`.
pub const FIXTURE_APPLY_SCRIPT: &str = include_str!("../luau/fixture_apply.luau");

/// Decode the fixture projection into the shared Rust metadata shape.
pub fn parse_fixture_list(outcome: ScriptEvalOutcome) -> Result<Vec<FixtureSpec>, EdevError> {
    serde_json::from_value(
        outcome
            .value
            .ok_or_else(|| EdevError::FixtureFailed("fixtures() returned no value".to_string()))?,
    )
    .map_err(|error| EdevError::FixtureFailed(format!("failed to decode fixtures list: {error}")))
}
