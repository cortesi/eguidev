use ruau::{
    module::{self, Binding},
    vm::ModuleBinding,
};

pub const DECLARATION: &str = include_str!("../../../luau/eguidev.d.luau");
pub const SOURCE: &[u8] = include_bytes!("../../../luau/eguidev.luau");
pub const PRIVATE_INPUTS: [&str; 7] = [
    "eguidev.query",
    "eguidev.action",
    "eguidev.wait",
    "eguidev.capture",
    "eguidev.fixture",
    "eguidev.diagnostic",
    "eguidev.record",
];

pub fn register(builder: &mut module::Builder) {
    builder.source_value_with(
        "eguidev",
        Binding::declared_global(),
        SOURCE,
        PRIVATE_INPUTS,
    );
}

pub fn declared_binding(binding: ModuleBinding) -> Binding {
    match binding {
        ModuleBinding::Global => Binding::declared_global(),
        ModuleBinding::GlobalOverride => Binding::declared_global_override(),
        ModuleBinding::Library(name) => Binding::declared_library(name),
        ModuleBinding::LibraryOverride(name) => Binding::declared_library_override(name),
        ModuleBinding::Hidden(name) => Binding::hidden(name),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, str};

    use super::{DECLARATION, SOURCE};

    /// Return the library source as text.
    fn library() -> &'static str {
        str::from_utf8(SOURCE).expect("library is UTF-8")
    }

    /// Return the body of a declaration block, from the line after its header
    /// to the line before its end.
    fn block<'a>(source: &'a str, header: &str, end: &str) -> &'a str {
        let start = source
            .find(header)
            .unwrap_or_else(|| panic!("no block {header}"));
        let body = &source[start + header.len()..];
        &body[..body
            .find(end)
            .unwrap_or_else(|| panic!("no end of {header}"))]
    }

    /// Return the methods that a declared class names: each `read` field
    /// whose type is a function.
    fn declared_methods(class: &str) -> BTreeSet<String> {
        block(DECLARATION, &format!("declare class {class}\n"), "\nend\n")
            .lines()
            .filter_map(|line| line.strip_prefix("    read "))
            .filter_map(|line| line.split_once(": "))
            .filter(|(_, ty)| ty.starts_with('('))
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// Return the methods that the library defines on one method table.
    fn library_methods(table: &str) -> BTreeSet<String> {
        let prefix = format!("function {table}.");
        library()
            .lines()
            .filter_map(|line| line.strip_prefix(prefix.as_str()))
            .filter_map(|line| line.split_once('('))
            .map(|(name, _)| name.to_string())
            .collect()
    }

    #[test]
    fn each_declared_method_has_an_implementation() {
        for (class, table) in [("Widget", "widgetMethods"), ("Viewport", "viewportMethods")] {
            let declared = declared_methods(class);
            assert!(!declared.is_empty(), "{class} declares methods");
            assert_eq!(declared, library_methods(table), "{class}");
        }
    }

    #[test]
    fn the_public_table_holds_each_declared_eguidev_field() {
        let declared = block(DECLARATION, "export type Eguidev = {\n", "\n}\n")
            .lines()
            .filter_map(|line| line.strip_prefix("    "))
            .filter(|line| !line.starts_with('-') && !line.starts_with(' '))
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.to_string())
            .collect::<BTreeSet<_>>();
        let public = block(library(), "local public: Eguidev = {\n", "\n}\n")
            .lines()
            .filter_map(|line| line.strip_prefix("    "))
            .filter(|line| !line.starts_with(' '))
            .filter_map(|line| line.split_once(" ="))
            .map(|(name, _)| name.to_string())
            .collect::<BTreeSet<_>>();
        assert!(!declared.is_empty());
        assert_eq!(declared, public);
    }
}
