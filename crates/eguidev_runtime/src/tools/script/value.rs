use serde_json::Value;
use tmcp::schema::ContentBlock;

use super::{
    runtime::ScriptRuntime,
    types::{ImageReferenceCollector, ScriptArgValue, ScriptArgs, ScriptImageInfo, ScriptValue},
};

/// Convert script arguments into the JSON table shape exposed as global `args`.
pub(super) fn script_args_to_json(args: &ScriptArgs) -> Value {
    let values = args
        .iter()
        .map(|(key, value)| {
            let value = match value {
                ScriptArgValue::String(value) => Value::String(value.clone()),
                ScriptArgValue::Int(value) => Value::Number((*value).into()),
                ScriptArgValue::Float(value) => Value::Number(
                    serde_json::Number::from_f64(*value)
                        .expect("script arguments only contain finite floats"),
                ),
                ScriptArgValue::Bool(value) => Value::Bool(*value),
            };
            (key.clone(), value)
        })
        .collect();
    Value::Object(values)
}

/// Record image references nested anywhere inside a script return value.
pub(super) fn collect_image_refs(value: &Value, collector: &mut ImageReferenceCollector) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_image_refs(value, collector);
            }
        }
        Value::Object(map) => {
            let is_image_ref = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "image_ref");
            if is_image_ref && let Some(id) = map.get("id").and_then(Value::as_str) {
                collector.record(id);
            }
            for value in map.values() {
                collect_image_refs(value, collector);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Preserve eguidev's zero/single/multi-return JSON shape.
pub(super) fn script_return_value_from_json_values(values: Vec<Value>) -> Option<Value> {
    match values.len() {
        0 => None,
        1 => values.into_iter().next(),
        _ => Some(Value::Array(values)),
    }
}

/// Build the public script value payload and MCP image blocks from a JSON return value.
pub(super) fn script_value_from_json(runtime: &ScriptRuntime, value: Value) -> ScriptValue {
    let mut collector = ImageReferenceCollector::default();
    collect_image_refs(&value, &mut collector);
    let images = build_image_blocks(runtime, &collector);
    let image_infos = if images.infos.is_empty() {
        None
    } else {
        Some(images.infos)
    };
    ScriptValue {
        value: Some(value),
        images: image_infos,
        content: images.blocks,
    }
}

struct ImageBlocks {
    infos: Vec<ScriptImageInfo>,
    blocks: Vec<ContentBlock>,
}

fn build_image_blocks(runtime: &ScriptRuntime, collector: &ImageReferenceCollector) -> ImageBlocks {
    let mut infos = Vec::new();
    let mut blocks = Vec::new();
    for image in runtime.images() {
        if !collector.contains(&image.id) {
            continue;
        }
        let content_index = infos.len() + 1;
        infos.push(ScriptImageInfo {
            id: image.id.clone(),
            content_index,
            kind: image.kind.as_str().to_string(),
            viewport_id: Some(image.viewport_id.clone()),
            target: image
                .target
                .clone()
                .and_then(|target| serde_json::to_value(target).ok()),
            rect: image.rect.and_then(|rect| serde_json::to_value(rect).ok()),
            metadata: None,
        });
        blocks.push(ContentBlock::image(image.data.clone(), "image/jpeg"));
    }
    ImageBlocks { infos, blocks }
}

#[cfg(test)]
mod tests {
    use ruau::vm::{MarshaledPair, MarshaledValue, serde::marshaled_to_json};
    use serde_json::{Value, json};

    use super::{script_args_to_json, script_return_value_from_json_values};
    use crate::tools::script::types::{ScriptArgValue, ScriptArgs};

    #[test]
    fn script_args_to_json_preserves_scalar_args() {
        let args = ScriptArgs::from([
            ("enabled".to_string(), ScriptArgValue::Bool(true)),
            (
                "name".to_string(),
                ScriptArgValue::String("Ada".to_string()),
            ),
            ("ratio".to_string(), ScriptArgValue::Float(1.5)),
            ("tries".to_string(), ScriptArgValue::Int(3)),
        ]);
        assert_eq!(
            script_args_to_json(&args),
            json!({
                "enabled": true,
                "name": "Ada",
                "ratio": 1.5,
                "tries": 3,
            })
        );
    }

    fn script_return_value_from_marshaled(values: &[MarshaledValue]) -> Option<Value> {
        let json_values = values
            .iter()
            .map(marshaled_to_json)
            .collect::<Result<Vec<_>, _>>()
            .expect("marshaled values convert to JSON");
        script_return_value_from_json_values(json_values)
    }

    #[test]
    fn ruau_owned_values_keep_script_return_shape() {
        assert_eq!(script_return_value_from_marshaled(&[]), None);
        assert_eq!(
            script_return_value_from_marshaled(&[MarshaledValue::Integer(7)]),
            Some(json!(7))
        );
        assert_eq!(
            script_return_value_from_marshaled(&[
                MarshaledValue::Integer(7),
                MarshaledValue::String(b"ok".to_vec()),
            ]),
            Some(json!([7, "ok"]))
        );
    }

    #[test]
    fn ruau_owned_values_preserve_number_and_table_shape() {
        let number = script_return_value_from_marshaled(&[MarshaledValue::Number(1.0)])
            .expect("number value");
        assert_eq!(number.as_f64(), Some(1.0));
        assert_eq!(number.as_i64(), None);

        let array = MarshaledValue::Table(vec![
            MarshaledPair {
                key: MarshaledValue::Number(1.0),
                value: MarshaledValue::Integer(10),
            },
            MarshaledPair {
                key: MarshaledValue::Integer(2),
                value: MarshaledValue::String(b"two".to_vec()),
            },
        ]);
        assert_eq!(
            script_return_value_from_marshaled(&[array]),
            Some(json!([10, "two"]))
        );
        assert_eq!(
            script_return_value_from_marshaled(&[MarshaledValue::Table(Vec::new())]),
            Some(json!({}))
        );
    }
}
