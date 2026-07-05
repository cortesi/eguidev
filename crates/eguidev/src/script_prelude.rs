//! App-owned Luau prelude registration for the script runtime.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use crate::{diagnostics::DevMcpConfigError, registry::lock};

const RESERVED_SCRIPT_GLOBALS: &[&str] = &[
    "_G",
    "args",
    "assert",
    "assert_widget_exists",
    "bit32",
    "buffer",
    "capture",
    "configure",
    "coroutine",
    "debug",
    "diagnostic",
    "diagnostics",
    "dump",
    "dump_text",
    "expect",
    "expect_absent",
    "expect_above",
    "expect_left_of",
    "expect_no_overlap",
    "expect_text_fits",
    "expect_tree",
    "expect_within",
    "fixture",
    "fixture_raw",
    "fixtures",
    "ipairs",
    "log",
    "math",
    "next",
    "os",
    "pairs",
    "root",
    "select",
    "string",
    "table",
    "tonumber",
    "tostring",
    "try_widget",
    "type",
    "utf8",
    "viewport",
    "viewports",
    "wait_for_capture",
    "wait_for_frames",
    "wait_until",
    "widget",
];

/// App Luau helpers evaluated before every script under an app namespace.
///
/// The namespace is exposed as a global table while the source is evaluated.
/// After setup the script VM is sandboxed, so user scripts can call helpers but
/// cannot replace the namespace table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptPrelude {
    /// Required app namespace, exposed as a global table such as `gonsh`.
    pub namespace: String,
    /// Luau source evaluated after eguidev's built-in prelude.
    pub source: String,
    /// Strict-mode declaration text appended to `script_api`.
    pub declarations: String,
}

impl ScriptPrelude {
    pub(crate) fn validate(&self) -> Result<(), DevMcpConfigError> {
        validate_namespace(&self.namespace)
    }
}

/// Registered app preludes staged on `DevMcp` and copied into active runtimes.
#[derive(Clone, Default)]
pub struct ScriptPreludeRegistry {
    preludes: Arc<Mutex<Vec<ScriptPrelude>>>,
}

impl fmt::Debug for ScriptPreludeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptPreludeRegistry")
            .field("preludes", &self.preludes().len())
            .finish()
    }
}

impl ScriptPreludeRegistry {
    /// Create an empty app prelude registry.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_from(&self, other: &Self) {
        *lock(&self.preludes, "script prelude registry lock") = other.preludes();
    }

    pub(crate) fn insert(&self, prelude: ScriptPrelude) -> Result<(), DevMcpConfigError> {
        prelude.validate()?;
        let mut preludes = lock(&self.preludes, "script prelude registry lock");
        if preludes
            .iter()
            .any(|existing| existing.namespace == prelude.namespace)
        {
            return Err(DevMcpConfigError::duplicate_script_prelude_namespace(
                &prelude.namespace,
            ));
        }
        preludes.push(prelude);
        Ok(())
    }

    /// Return registered preludes in registration order.
    pub fn preludes(&self) -> Vec<ScriptPrelude> {
        lock(&self.preludes, "script prelude registry lock").clone()
    }
}

fn validate_namespace(namespace: &str) -> Result<(), DevMcpConfigError> {
    if namespace.is_empty() {
        return Err(DevMcpConfigError::empty_script_prelude_namespace());
    }
    if !is_luau_identifier(namespace) {
        return Err(DevMcpConfigError::invalid_script_prelude_namespace(
            namespace,
        ));
    }
    if RESERVED_SCRIPT_GLOBALS.contains(&namespace) {
        return Err(DevMcpConfigError::reserved_script_prelude_namespace(
            namespace,
        ));
    }
    Ok(())
}

fn is_luau_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{ScriptPrelude, ScriptPreludeRegistry};

    fn prelude(namespace: &str) -> ScriptPrelude {
        ScriptPrelude {
            namespace: namespace.to_string(),
            source: String::new(),
            declarations: String::new(),
        }
    }

    #[test]
    fn registry_rejects_reserved_namespaces() {
        let registry = ScriptPreludeRegistry::new();
        let error = registry.insert(prelude("widget")).expect_err("reserved");
        assert_eq!(error.code, "reserved_script_prelude_namespace");
    }

    #[test]
    fn registry_rejects_duplicate_namespaces() {
        let registry = ScriptPreludeRegistry::new();
        registry.insert(prelude("app")).expect("first");
        let error = registry.insert(prelude("app")).expect_err("duplicate");
        assert_eq!(error.code, "duplicate_script_prelude_namespace");
    }
}
