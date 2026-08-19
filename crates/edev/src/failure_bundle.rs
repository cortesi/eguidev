//! Deterministic failure-bundle naming helpers.

/// Convert an arbitrary script or image name into a portable path component.
pub fn safe_file_component(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if safe.is_empty() {
        "image".to_string()
    } else {
        safe
    }
}

/// Choose a file extension for an MCP image MIME type.
pub fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// Compute a stable eight-hex hash for a deterministic bundle directory.
pub fn stable_hash8(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

use super::*;

/// Inputs retained by an active app session for deterministic failure collection.
#[derive(Clone)]
pub struct BundleContext {
    /// Root directory for all failure bundles in this smoke run.
    pub(super) dir: PathBuf,
    /// App launch settings to record in `meta.json`.
    pub(super) launch: LaunchConfig,
    /// Tail-capped app stderr captured since launch.
    pub(super) stderr_buffer: Arc<Mutex<Vec<u8>>>,
    /// Tail-capped app stdout captured since launch when available.
    pub(super) stdout_buffer: Arc<Mutex<Vec<u8>>>,
    /// Timeout for bundle collection script evaluation.
    pub(super) collection_timeout_ms: u64,
}

/// Payload returned by the internal bundle snapshot collection script.
#[derive(Debug, Deserialize)]
struct BundleSnapshotCollection {
    /// Full structured tree dump.
    tree: serde_json::Value,
    /// Full text tree dump.
    text: String,
    /// Viewport screenshots captured by the collection script.
    #[serde(deserialize_with = "deserialize_bundle_shots")]
    shots: Vec<BundleShot>,
    /// Non-fatal screenshot collection errors.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_bundle_errors")]
    errors: Vec<BundleCollectionError>,
}

/// One viewport screenshot entry from the collection script.
#[derive(Debug, Deserialize)]
struct BundleShot {
    /// Canonical viewport id such as `root` or `vp:<hex>`.
    viewport_id: Option<String>,
    /// Semantic viewport name when one was registered.
    name: Option<String>,
    /// Image reference returned by `Viewport:screenshot()`.
    image: BundleImageRef,
}

/// Image reference returned by the Luau script runtime.
#[derive(Debug, Deserialize)]
struct BundleImageRef {
    /// Runtime image id used to find the corresponding MCP image block.
    id: String,
}

/// Non-fatal bundle collection error returned by an internal script.
#[derive(Debug, Deserialize, Serialize)]
struct BundleCollectionError {
    /// Collection phase that failed.
    kind: String,
    /// Viewport id involved in the failure when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport_id: Option<String>,
    /// Semantic viewport name involved in the failure when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Human-readable error message.
    message: String,
}

/// Internal Luau script used to collect frame artifacts after a smoke failure.
pub const BUNDLE_COLLECTION_SCRIPT: &str = r#"
eguidev.root:wait_capture()
local shots = {}
local errors = {}
for _, viewport in ipairs(eguidev.viewports()) do
    local state = viewport:state()
    local ok, image = pcall(function()
        return viewport:screenshot()
    end)
    if ok then
        table.insert(shots, {
            viewport_id = viewport.id,
            name = state.name,
            image = image,
        })
    else
        table.insert(errors, {
            kind = "screenshot",
            viewport_id = viewport.id,
            name = state.name,
            message = tostring(image),
        })
    end
end
return {
    tree = eguidev.dump({ fields = "full" }),
    text = eguidev.dump_text({ fields = "full" }),
    shots = shots,
    errors = errors,
}
"#;

/// Internal Luau script used to collect diagnostics after frame artifacts are written.
pub const BUNDLE_DIAGNOSTICS_SCRIPT: &str = r#"
return eguidev.diagnostics()
"#;

/// Deserialize Luau's ambiguous empty table as an empty screenshot list.
fn deserialize_bundle_shots<'de, D>(deserializer: D) -> Result<Vec<BundleShot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_luau_array(deserializer, "screenshot")
}

/// Deserialize Luau's ambiguous empty table as an empty collection error list.
fn deserialize_bundle_errors<'de, D>(
    deserializer: D,
) -> Result<Vec<BundleCollectionError>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_luau_array(deserializer, "collection error")
}

/// Deserialize a Luau array while accepting an empty table encoded as `{}`.
fn deserialize_luau_array<'de, D, T>(deserializer: D, label: &str) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(SerdeDeError::custom),
        serde_json::Value::Object(map) if map.is_empty() => Ok(Vec::new()),
        other => Err(SerdeDeError::custom(format!(
            "expected {label} array, got {other}"
        ))),
    }
}

/// Write one deterministic failure bundle for a failed smoke script.
pub async fn write_failure_bundle(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    context: &BundleContext,
    script_path: &str,
    round: Option<u32>,
    args: &ScriptArgs,
    outcome: &ScriptEvalOutcome,
) -> Result<(), EdevError> {
    let bundle_key = match round {
        Some(round) => format!("{script_path}-round-{round}"),
        None => script_path.to_string(),
    };
    let bundle_dir = context.dir.join(format!(
        "{}-{}",
        safe_file_component(&bundle_key),
        stable_hash8(&bundle_key)
    ));
    replace_dir(&bundle_dir)?;
    fs::write(
        bundle_dir.join("meta.json"),
        bundle_meta(context, script_path, round, args, outcome)?,
    )?;
    fs::write(bundle_dir.join("failure.txt"), failure_text(outcome)?)?;
    fs::write(
        bundle_dir.join("app.stderr.log"),
        snapshot_output(&context.stderr_buffer),
    )?;
    fs::write(
        bundle_dir.join("app.stdout.log"),
        snapshot_output(&context.stdout_buffer),
    )?;

    let collection_result = match call_script_eval_result(
        client,
        ScriptEvalRequest {
            script: BUNDLE_COLLECTION_SCRIPT.to_string(),
            timeout_ms: Some(context.collection_timeout_ms),
            options: Some(ScriptEvalOptions {
                source_name: Some(format!("<bundle:{script_path}>")),
                args: ScriptArgs::default(),
            }),
        },
    )
    .await
    {
        Ok(result) => Some(result),
        Err(message) => {
            fs::write(
                bundle_dir.join("collection-error.txt"),
                format!("bundle collection failed: {message}\n"),
            )?;
            None
        }
    };

    let collection = match collection_result.as_ref() {
        Some(result) => match parse_script_eval_outcome(result) {
            Ok(collection_outcome) if collection_outcome.success => {
                match collection_outcome.value {
                    Some(value) => {
                        match serde_json::from_value::<BundleSnapshotCollection>(value) {
                            Ok(collection) => Some(collection),
                            Err(error) => {
                                append_collection_error(
                                    &bundle_dir,
                                    format!("invalid bundle payload: {error}\n"),
                                )?;
                                None
                            }
                        }
                    }
                    None => {
                        append_collection_error(
                            &bundle_dir,
                            "bundle collection script returned no value\n",
                        )?;
                        None
                    }
                }
            }
            Ok(collection_outcome) => {
                append_collection_error(
                    &bundle_dir,
                    format!(
                        "bundle collection script failed: {}\n",
                        script_eval_error_message(collection_outcome.error.as_ref(), "failed")
                    ),
                )?;
                None
            }
            Err(error) => {
                append_collection_error(
                    &bundle_dir,
                    format!("failed to decode bundle collection result: {error}\n"),
                )?;
                None
            }
        },
        None => None,
    };

    if let (Some(result), Some(collection)) = (collection_result.as_ref(), collection.as_ref()) {
        fs::write(bundle_dir.join("tree.json"), pretty_json(&collection.tree)?)?;
        fs::write(bundle_dir.join("tree.txt"), &collection.text)?;
        if !collection.errors.is_empty() {
            append_collection_error(
                &bundle_dir,
                format!(
                    "bundle snapshot collection errors:\n{}",
                    pretty_json(&collection.errors)?
                ),
            )?;
        }
        if let Err(error) = write_bundle_images(&bundle_dir, result, collection) {
            append_collection_error(
                &bundle_dir,
                format!("bundle image extraction failed: {error}\n"),
            )?;
        }
    } else {
        fs::write(bundle_dir.join("tree.json"), "{}\n")?;
        fs::write(bundle_dir.join("tree.txt"), "bundle collection failed\n")?;
    }
    fs::write(
        bundle_dir.join("diagnostics.json"),
        pretty_json(&collect_bundle_diagnostics(client, context, script_path).await?)?,
    )?;
    Ok(())
}

/// Collect diagnostics for a bundle without coupling them to tree/screenshot capture.
async fn collect_bundle_diagnostics(
    client: &Arc<AsyncMutex<tmcp::Client<()>>>,
    context: &BundleContext,
    script_path: &str,
) -> Result<serde_json::Value, EdevError> {
    let fallback = serde_json::json!({
        "values": {},
        "errors": {
            "_collection": {
                "code": "collection_failed",
                "message": "bundle diagnostics collection failed",
            },
        },
    });
    let result = match call_script_eval_result(
        client,
        ScriptEvalRequest {
            script: BUNDLE_DIAGNOSTICS_SCRIPT.to_string(),
            timeout_ms: Some(context.collection_timeout_ms),
            options: Some(ScriptEvalOptions {
                source_name: Some(format!("<bundle-diagnostics:{script_path}>")),
                args: ScriptArgs::default(),
            }),
        },
    )
    .await
    {
        Ok(result) => result,
        Err(message) => {
            return Ok(serde_json::json!({
                "values": {},
                "errors": {
                    "_collection": {
                        "code": "collection_failed",
                        "message": format!("bundle diagnostics failed: {message}"),
                    },
                },
            }));
        }
    };
    let outcome = match parse_script_eval_outcome(&result) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Ok(serde_json::json!({
                "values": {},
                "errors": {
                    "_collection": {
                        "code": "collection_failed",
                        "message": format!("failed to decode bundle diagnostics: {error}"),
                    },
                },
            }));
        }
    };
    if !outcome.success {
        return Ok(serde_json::json!({
            "values": {},
            "errors": {
                "_collection": {
                    "code": "collection_failed",
                    "message": script_eval_error_message(outcome.error.as_ref(), "bundle diagnostics failed"),
                },
            },
        }));
    }
    Ok(outcome.value.unwrap_or(fallback))
}

/// Append one collection warning to `collection-error.txt`.
fn append_collection_error(bundle_dir: &Path, message: impl AsRef<str>) -> Result<(), EdevError> {
    use std::io::Write;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(bundle_dir.join("collection-error.txt"))?;
    file.write_all(message.as_ref().as_bytes())?;
    Ok(())
}

/// Replace a deterministic bundle directory with an empty directory.
pub fn replace_dir(path: &Path) -> Result<(), EdevError> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

/// Build `meta.json` for a failure bundle.
pub fn bundle_meta(
    context: &BundleContext,
    script_path: &str,
    round: Option<u32>,
    args: &ScriptArgs,
    outcome: &ScriptEvalOutcome,
) -> Result<String, EdevError> {
    let script = match round {
        Some(round) => serde_json::json!({
            "path": script_path,
            "round": round,
            "args": args,
        }),
        None => serde_json::json!({
            "path": script_path,
            "args": args,
        }),
    };
    let value = serde_json::json!({
        "script": script,
        "fixtures": &outcome.fixtures,
        "app": {
            "command": &context.launch.command,
            "cwd": context.launch.cwd.display().to_string(),
        },
        "eguidev_version": env!("CARGO_PKG_VERSION"),
        "failure": {
            "message": script_eval_error_message(outcome.error.as_ref(), "script failed"),
            "details": outcome.error.as_ref().and_then(|error| error.details.clone()),
            "error": &outcome.error,
            "egui_diagnostics": &outcome.egui_diagnostics,
        },
    });
    pretty_json(&value)
}

/// Render the human-readable failure summary for `failure.txt`.
pub fn failure_text(outcome: &ScriptEvalOutcome) -> Result<String, EdevError> {
    let mut text = String::new();
    text.push_str(&format!(
        "failure: {}\n",
        script_eval_error_message(outcome.error.as_ref(), "script failed")
    ));
    if let Some(error) = &outcome.error {
        if let Some(code) = &error.code {
            text.push_str(&format!("code: {code}\n"));
        }
        if let Some(location) = &error.location {
            text.push_str(&format!(
                "location: {}:{}\n",
                location.line,
                location.column.unwrap_or(1)
            ));
        }
        if let Some(details) = &error.details {
            text.push_str("\ndetails:\n");
            text.push_str(&pretty_json(details)?);
        }
    }
    if !outcome.logs.is_empty() {
        text.push_str("\nlogs:\n");
        for log in &outcome.logs {
            text.push_str("- ");
            text.push_str(log);
            text.push('\n');
        }
    }
    if !outcome.assertions.is_empty() {
        text.push_str("\nassertions:\n");
        text.push_str(&pretty_json(&outcome.assertions)?);
    }
    if !outcome.fixtures.is_empty() {
        text.push_str("\nfixtures:\n");
        text.push_str(&pretty_json(&outcome.fixtures)?);
    }
    if !outcome.egui_diagnostics.is_empty() {
        text.push_str("\negui diagnostics:\n");
        text.push_str(&pretty_json(&outcome.egui_diagnostics)?);
    }
    Ok(text)
}

/// Write viewport screenshot image blocks referenced by the collection payload.
fn write_bundle_images(
    bundle_dir: &Path,
    result: &CallToolResult,
    collection: &BundleSnapshotCollection,
) -> Result<(), EdevError> {
    for shot in &collection.shots {
        let image = collection_image_content(result, &shot.image.id)?;
        let name = shot
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(shot.viewport_id.as_deref())
            .unwrap_or(&shot.image.id);
        let path = bundle_dir.join(format!(
            "viewport-{}.{}",
            safe_file_component(name),
            image_extension(&image.mime_type)
        ));
        let bytes = image.data_bytes().map_err(|error| {
            EdevError::EvalFailed(format!("failed to decode image {}: {error}", shot.image.id))
        })?;
        fs::write(path, bytes)?;
    }
    Ok(())
}

/// Return the MCP image content block for a collected image id.
fn collection_image_content<'a>(
    result: &'a CallToolResult,
    image_id: &str,
) -> Result<&'a ImageContent, EdevError> {
    let outcome = parse_script_eval_outcome(result).map_err(EdevError::EvalFailed)?;
    let image = outcome
        .images
        .as_ref()
        .and_then(|images| images.iter().find(|image| image.id == image_id))
        .ok_or_else(|| EdevError::EvalFailed(format!("missing bundle image {image_id}")))?;
    let block = result.content.get(image.content_index).ok_or_else(|| {
        EdevError::EvalFailed(format!(
            "image {} referenced missing content block {}",
            image.id, image.content_index
        ))
    })?;
    let ContentBlock::Image(content) = block else {
        return Err(EdevError::EvalFailed(format!(
            "image {} referenced non-image content block {}",
            image.id, image.content_index
        )));
    };
    Ok(content)
}

/// Serialize bundle JSON with a trailing newline for stable files.
pub fn pretty_json(value: &impl Serialize) -> Result<String, EdevError> {
    let mut text = serde_json::to_string_pretty(value).map_err(|error| {
        EdevError::EvalFailed(format!("failed to serialize bundle JSON: {error}"))
    })?;
    text.push('\n');
    Ok(text)
}
