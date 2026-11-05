//! The loader of the Luau module tree of `edev smoke` and `edev eval`.

use super::*;

/// Load one deterministic Luau module tree for `edev smoke` and `edev eval`.
pub fn load_script_modules(module_dir: Option<&Path>) -> Result<ScriptModules, EdevError> {
    let Some(module_dir) = module_dir else {
        return Ok(ScriptModules::default());
    };
    if !module_dir.is_dir() {
        return Err(EdevError::InvalidArgs(format!(
            "module directory is not an existing directory: {}",
            module_dir.display()
        )));
    }
    let mut pending = vec![module_dir.to_path_buf()];
    let mut modules = ScriptModules::default();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !file_type.is_file() || path.extension().and_then(OsStr::to_str) != Some("luau") {
                continue;
            }
            let relative = path
                .strip_prefix(module_dir)
                .expect("walked module stays below its root");
            let name = relative.to_str().ok_or_else(|| {
                EdevError::InvalidArgs(format!(
                    "module path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            modules.insert(name.replace(MAIN_SEPARATOR, "/"), fs::read_to_string(path)?);
        }
    }
    Ok(modules)
}
