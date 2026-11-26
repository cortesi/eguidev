//! Child Cargo commands without inherited package identity.

use std::{env, ffi::OsStr, path::Path, process::Command};

/// Build the command of an app launcher. A launcher that can run Cargo
/// starts without the Cargo metadata of the launching package.
pub fn app_command(program: &str) -> Command {
    let runs_cargo = Path::new(program).file_stem().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("cargo" | "ncode" | "edev" | "canopyctl")
        )
    });
    if runs_cargo {
        command(program)
    } else {
        Command::new(program)
    }
}

/// Preserve build settings and remove the launching package's Cargo metadata.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in env::vars_os() {
        let Some(key) = name.to_str() else {
            continue;
        };
        if key.starts_with("CARGO_PKG_")
            || matches!(
                key,
                "CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH" | "CARGO_MANIFEST_LINKS"
            )
        {
            command.env_remove(name);
        }
    }
    command
}
