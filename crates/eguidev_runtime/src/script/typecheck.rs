use ruau::typecheck::{
    Checker, Config, DiagnosticCategory, Mode,
    builtins::{DefinitionModule, Environment},
    types::Arena,
};

const PUBLIC_DECLARATION: &str = include_str!("../../luau/eguidev.d.luau");
#[cfg(test)]
const PRIVATE_LIBRARY: &str = include_str!("../../luau/eguidev.luau");

/// One strict-check failure with a source-relative primary location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckFailure {
    pub(crate) error_type: &'static str,
    pub(crate) message: String,
    pub(crate) line: Option<usize>,
    pub(crate) column: Option<usize>,
    pub(crate) diagnostics: Vec<String>,
}

/// Check one tenant source against the exact checked-in public declaration.
pub fn check_source(source_name: &str, source: &str) -> Result<(), CheckFailure> {
    let mut checker =
        checker_with_definitions(&[DefinitionModule::from_static("eguidev", PUBLIC_DECLARATION)])?;
    let checked = checker.check_source_with_config(source, Config::with_source_mode(Mode::Strict));
    if !checked.has_errors() {
        return Ok(());
    }
    let records = checked.diagnostics().records().collect::<Vec<_>>();
    let first = records.first();
    let location = first.map(|record| record.primary_location.begin);
    let diagnostics = records
        .iter()
        .map(|record| {
            let begin = record.primary_location.begin;
            if begin.is_missing() {
                format!("{source_name}: {}", record.message)
            } else {
                format!(
                    "{source_name}:{}:{}: {}",
                    begin.line, begin.column, record.message
                )
            }
        })
        .collect::<Vec<_>>();
    Err(CheckFailure {
        error_type: if records
            .iter()
            .any(|record| record.category == DiagnosticCategory::Parse)
        {
            "parse"
        } else {
            "typecheck"
        },
        message: diagnostics.join("; "),
        line: location
            .filter(|location| !location.is_missing())
            .map(|location| location.line as usize),
        column: location
            .filter(|location| !location.is_missing())
            .map(|location| location.column as usize),
        diagnostics,
    })
}

fn checker_with_definitions(definitions: &[DefinitionModule]) -> Result<Checker, CheckFailure> {
    let mut arena = Arena::new();
    let builtins = Environment::standard_with_definition_modules(&mut arena, definitions)
        .map_err(|error| CheckFailure {
            error_type: "internal",
            message: error.to_string(),
            line: None,
            column: None,
            diagnostics: vec![error.to_string()],
        })?
        .without_globals(["require", "loadstring", "getfenv", "setfenv"]);
    Ok(Checker::with_builtins(arena, builtins))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    #[test]
    fn public_declaration_parses_as_the_checker_environment() {
        checker_with_definitions(&[DefinitionModule::from_static("eguidev", PUBLIC_DECLARATION)])
            .expect("public declaration");
    }

    #[test]
    fn private_library_checks_against_the_public_declaration() {
        check_source("eguidev.luau", PRIVATE_LIBRARY).expect("private library");
    }

    #[test]
    fn tenant_type_errors_keep_source_relative_locations() {
        let error = check_source(
            "probe.luau",
            "local state: WidgetState = eguidev.widget('missing'):state()\nreturn state",
        )
        .expect_err("optional state must fail");

        assert_eq!(error.line, Some(1));
        assert!(error.message.contains("probe.luau:1:"), "{}", error.message);
    }

    #[test]
    fn frozen_handle_types_compose_in_typed_helpers() {
        check_source(
            "probe.luau",
            r#"
local function viewport_for(name: string): Viewport
    if name == "root" then
        return eguidev.root
    end
    return eguidev.wait_viewport({ name = name })
end

local function scoped_widget(viewport_name: string, id: string): Widget
    local viewport = viewport_for(viewport_name)
    return viewport:widget(id)
end

return scoped_widget("root", "probe")
"#,
        )
        .expect("frozen handles should retain their public types");

        let error = check_source("probe.luau", "eguidev.root.id = 'replacement'")
            .expect_err("frozen handle fields must be read-only");
        assert!(error.message.contains("read-only"), "{}", error.message);
    }

    #[test]
    fn module_import_is_not_in_the_script_environment() {
        let error = check_source("probe.luau", "return require('private')")
            .expect_err("require must be absent");

        assert!(error.message.contains("require"), "{}", error.message);
    }

    #[test]
    fn checked_in_scripts_typecheck_against_the_exact_public_declaration() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let mut failures = Vec::new();
        for relative in ["smoketest", "docs/examples", "crates/edev/luau"] {
            let directory = workspace.join(relative);
            let mut paths = fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
                .map(|entry| entry.expect("directory entry").path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "luau")
                })
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                let source_name = path
                    .strip_prefix(workspace)
                    .expect("workspace-relative script")
                    .to_string_lossy();
                if let Err(error) = check_source(&source_name, &source) {
                    failures.push(format!("{}: {}", path.display(), error.message));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "checked-in Luau type errors:\n{}",
            failures.join("\n")
        );
    }
}
