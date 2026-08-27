//! Checked-in Luau scripting definitions.

use std::{result, sync::OnceLock};

use ruau_script_api::{
    ScriptApiAvailability, ScriptApiCatalog, ScriptApiError, ScriptApiGuide, ScriptApiLoad,
    ScriptApiQuery, ScriptApiResolution, ScriptApiResponse, ScriptApiSource, ScriptApiTask,
};
use tmcp::{ToolError, ToolResult, schema::CallToolResult};

const SCRIPT_DEFINITIONS: &str = include_str!("../luau/eguidev.d.luau");
/// Catalog built from the exact declaration bytes used by the checker.
static SCRIPT_CATALOG: OnceLock<ScriptApiCatalog> = OnceLock::new();

/// Return the checked-in Luau definitions that describe the scripting API.
pub fn script_definitions() -> &'static str {
    SCRIPT_DEFINITIONS
}

/// Return the shared discovery catalog.
pub fn script_api_catalog() -> &'static ScriptApiCatalog {
    SCRIPT_CATALOG.get_or_init(|| {
        ScriptApiCatalog::new(
            ScriptApiGuide {
                introduction: "Discover the checked Eguidev API before you automate an instrumented app. The `eguidev` table is global; imports and host I/O are unavailable.".to_owned(),
                tasks: vec![
                    ScriptApiTask {
                        name: "Find widgets".to_owned(),
                        instruction: "Request eguidev.widget, eguidev.widgets, or Viewport.widget.".to_owned(),
                    },
                    ScriptApiTask {
                        name: "Interact".to_owned(),
                        instruction: "Request Widget.click, Widget.type_text, or Widget.wait.".to_owned(),
                    },
                    ScriptApiTask {
                        name: "Prepare state".to_owned(),
                        instruction: "Request eguidev.fixtures and eguidev.fixture.".to_owned(),
                    },
                ],
            },
            vec![ScriptApiSource {
                id: "eguidev".to_owned(),
                description: "Checked globals for instrumented egui automation.".to_owned(),
                load: ScriptApiLoad::Global {
                    roots: vec!["eguidev".to_owned()],
                },
                availability: ScriptApiAvailability::Ready,
                declaration: Some(SCRIPT_DEFINITIONS.to_owned()),
                example: Some("return eguidev.fixtures()".to_owned()),
            }],
        )
        .expect("Eguidev declaration builds a discovery catalog")
    })
}

/// Resolve one static discovery query.
pub fn script_api_response(
    query: &ScriptApiQuery,
) -> result::Result<ScriptApiResponse, ScriptApiError> {
    match script_api_catalog().query(query)? {
        ScriptApiResolution::Response(response) => Ok(response),
        ScriptApiResolution::SourceRequired(_) => unreachable!("Eguidev declarations are static"),
    }
}

/// Map shared discovery to one tmcp result envelope.
pub fn script_api_tool_result(
    result: result::Result<ScriptApiResponse, ScriptApiError>,
) -> ToolResult<CallToolResult> {
    match result {
        Ok(response) => {
            let structured = serde_json::to_value(&response)
                .map_err(|error| ToolError::internal(error.to_string()))?;
            Ok(CallToolResult::new()
                .with_structured_content(structured)
                .with_text_content(response.content))
        }
        Err(error) => {
            let structured = serde_json::to_value(&error)
                .map_err(|error| ToolError::internal(error.to_string()))?;
            Ok(CallToolResult::new()
                .with_is_error(true)
                .with_structured_content(structured)
                .with_text_content(error.message))
        }
    }
}

#[cfg(test)]
mod tests {
    use ruau_script_api::{ScriptApiErrorKind, ScriptApiMode};

    use super::*;

    #[test]
    fn checked_declaration_builds_the_public_inventory() {
        let list = script_api_response(&ScriptApiQuery {
            list: true,
            filter: None,
        })
        .expect("list response");
        assert_eq!(list.mode, ScriptApiMode::List);
        for path in ["Eguidev.widget", "Eguidev.fixtures", "Widget.click"] {
            assert!(
                list.entries.iter().any(|entry| entry.path == path),
                "missing {path}"
            );
        }
    }

    #[test]
    fn common_queries_include_structured_failures() {
        let detail = script_api_response(&ScriptApiQuery {
            list: false,
            filter: Some("Widget.click".to_owned()),
        })
        .expect("detail response");
        assert!(detail.content.contains("click"));
        let missing = script_api_response(&ScriptApiQuery {
            list: false,
            filter: Some("missing-path".to_owned()),
        })
        .expect_err("missing query");
        assert_eq!(missing.kind, ScriptApiErrorKind::NotFound);
    }
}
