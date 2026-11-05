//! The smoke run of the MCP server, which evaluates each script of the suite.

use super::*;

/// Execute the resolved smoke suite by calling `script_eval` for each
/// discovered script.
pub async fn run_smoke_suite(
    client: Arc<AsyncMutex<tmcp::Client<()>>>,
    config: &SmokeConfig,
    bundle_context: Option<BundleContext>,
) -> Result<SuiteResult, EdevError> {
    let modules = load_script_modules(config.module_dir.as_deref())?;
    if let Some(context) = bundle_context.as_ref()
        && let Err(error) = prune_previous_bundles(&context.dir)
    {
        eprintln!("edev: failed to remove the failure bundles of an earlier run: {error}");
    }
    Ok(run_suite_with(
        &config.suite,
        |request: ScriptRunRequest| {
            let script_path = request.path.clone();
            let script_args = request.args.clone();
            let script_source = request.source.clone();
            let payload = script_eval_request_value(ScriptEvalRequest {
                script: request.source,
                timeout_ms: request.timeout_ms,
                options: Some(ScriptEvalOptions {
                    source_name: Some(script_path.clone()),
                    max_instructions: request.max_instructions,
                    args: request.args,
                    modules: modules.clone(),
                }),
            });
            let result = block_in_place(|| {
                Handle::current().block_on(async {
                    let client = client.lock().await;
                    client
                        .call_tool("script_eval".to_string(), payload)
                        .await
                        .map_err(|error| error.to_string())
                })
            })?;
            let mut outcome = parse_script_eval_outcome(&result)?;
            config.suite.apply_egui_diagnostic_policy(&mut outcome);
            if !outcome.success
                && let Some(context) = bundle_context.as_ref()
            {
                let bundle_round = if config.suite.round_limit() > 1 {
                    Some(request.round)
                } else {
                    None
                };
                let bundle_result = block_in_place(|| {
                    Handle::current().block_on(write_failure_bundle(
                        &client,
                        context,
                        FailureBundleScript {
                            path: &script_path,
                            round: bundle_round,
                            args: &script_args,
                            source: &script_source,
                            modules: &modules,
                        },
                        &outcome,
                    ))
                });
                if let Err(error) = bundle_result {
                    eprintln!("edev: failed to write failure bundle for {script_path}: {error}");
                }
            }
            Ok(outcome)
        },
    ))
}
