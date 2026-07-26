//! Reader for the checked-in Luau declaration surface.
//!
//! `eguidev.d.luau` stays hand-written because it is the documentation a script
//! author reads, and one file shows the complete interface at a glance. This
//! reader recovers the method signatures from that file so a test can hold them
//! against the runtime binding tables, which Ruau cannot audit because the
//! method tables are hidden bindings.

use std::collections::BTreeMap;

/// One method declared on a script-facing type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredMethod {
    /// Method name as scripts call it.
    pub name: String,
    /// Declared parameters after `self`, as `name: Type` text.
    pub parameters: Vec<String>,
}

impl DeclaredMethod {
    /// Type of the final declared parameter, without its name.
    pub fn last_parameter_type(&self) -> Option<&str> {
        let last = self.parameters.last()?;
        Some(
            last.split_once(':')
                .map_or(last.as_str(), |(_, ty)| ty.trim()),
        )
    }

    /// Whether the final parameter is an optional options table.
    pub fn takes_options(&self) -> bool {
        self.last_parameter_type()
            .is_some_and(|ty| ty.ends_with("Options?"))
    }
}

/// Read the methods declared on `export type <type_name> = { ... }`.
///
/// A field whose type is not a function is a data field, not a method, and is
/// skipped.
pub fn declared_methods(source: &str, type_name: &str) -> BTreeMap<String, DeclaredMethod> {
    let Some(body) = type_body(source, type_name) else {
        return BTreeMap::new();
    };
    let mut methods = BTreeMap::new();
    for (name, signature) in top_level_fields(&body) {
        let Some(parameters) = function_parameters(&signature) else {
            continue;
        };
        methods.insert(name.clone(), DeclaredMethod { name, parameters });
    }
    methods
}

/// Text between the braces of `export type <type_name> = { ... }`.
fn type_body(source: &str, type_name: &str) -> Option<String> {
    let header = format!("export type {type_name} = {{");
    let start = source.find(&header)? + header.len();
    let mut depth = 1usize;
    for (offset, character) in source[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(source[start..start + offset].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a type body into `(field name, declared type)` pairs.
///
/// Comment lines are dropped, and a field's type continues until the comma that
/// closes it at brace and parenthesis depth zero.
fn top_level_fields(body: &str) -> Vec<(String, String)> {
    let stripped = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut fields = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for character in stripped.chars() {
        match character {
            '{' | '(' | '[' => {
                depth += 1;
                current.push(character);
            }
            '}' | ')' | ']' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            ',' if depth == 0 => {
                push_field(&mut fields, &current);
                current.clear();
            }
            _ => current.push(character),
        }
    }
    push_field(&mut fields, &current);
    fields
}

fn push_field(fields: &mut Vec<(String, String)>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if let Some((name, declared)) = text.split_once(':') {
        fields.push((name.trim().to_string(), declared.trim().to_string()));
    }
}

/// Parameters of a `(self: T, ...) -> R` declaration, excluding `self`.
///
/// Returns `None` when the declared type is not a method.
fn function_parameters(declared: &str) -> Option<Vec<String>> {
    let inner = declared.strip_prefix('(')?;
    let mut depth = 1usize;
    let mut end = None;
    for (offset, character) in inner.char_indices() {
        match character {
            '(' | '{' => depth += 1,
            ')' | '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let parameters = &inner[..end?];
    if !inner[end? + 1..].trim_start().starts_with("->") {
        return None;
    }
    let mut split = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for character in parameters.chars() {
        match character {
            '(' | '{' => {
                depth += 1;
                current.push(character);
            }
            ')' | '}' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            ',' if depth == 0 => {
                split.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(character),
        }
    }
    let last = current.trim();
    if !last.is_empty() {
        split.push(last.to_string());
    }
    let mut parameters = split.into_iter();
    let receiver = parameters.next()?;
    if !receiver.starts_with("self:") && !receiver.starts_with("self :") {
        return None;
    }
    Some(parameters.collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r"
export type Widget = {
    id: string,
    --- Docs are ignored.
    wait_for_visible: (self: Widget, options: WaitOptions?) -> WidgetState,
    drag_relative: (self: Widget, relative: Vec, from: Vec?, options: ActionOptions?) -> (),
    state: (self: Widget) -> WidgetState,
    wait_for_widget: (
        self: Widget,
        id: string,
        predicate: WidgetWaitPredicate
    ) -> WidgetState,
    callback: (value: number) -> (),
}
";

    #[test]
    fn reads_methods_and_skips_data_fields_and_free_functions() {
        let methods = declared_methods(SOURCE, "Widget");
        let mut names = methods.keys().cloned().collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            vec![
                "drag_relative".to_string(),
                "state".to_string(),
                "wait_for_visible".to_string(),
                "wait_for_widget".to_string(),
            ]
        );
    }

    #[test]
    fn reads_parameters_across_wrapped_declarations() {
        let methods = declared_methods(SOURCE, "Widget");
        assert_eq!(
            methods["wait_for_widget"].parameters,
            vec![
                "id: string".to_string(),
                "predicate: WidgetWaitPredicate".to_string()
            ]
        );
        assert!(methods["wait_for_visible"].takes_options());
        assert!(methods["drag_relative"].takes_options());
        assert!(!methods["state"].takes_options());
        assert!(!methods["wait_for_widget"].takes_options());
    }

    #[test]
    fn returns_nothing_for_a_missing_type() {
        assert!(declared_methods(SOURCE, "Missing").is_empty());
    }
}
