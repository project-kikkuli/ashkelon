use super::LaunchPlan;

/// `codex queue --thread <id> --message <text>` wakes this harness once
/// `WakeTarget::harness_session_id` is known (see `wake::codex`); nothing here needs to know
/// that id up front.
pub fn plan(relay_base: &str, _launch: &str, args: &[String]) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("codex", "codex");
    let use_openai_api = args.iter().any(|a| a == "--openai-api");
    let passthrough: Vec<String> = args.iter().filter(|a| a.as_str() != "--openai-api").cloned().collect();

    let provider_toml = if use_openai_api {
        format!(
            r#"model_providers.ashkelon={{name="ashkelon",base_url="{relay_base}/openai/v1",wire_api="responses",env_key="OPENAI_API_KEY"}}"#
        )
    } else {
        format!(
            r#"model_providers.ashkelon={{name="ashkelon",base_url="{relay_base}/chatgpt/backend-api/codex",wire_api="responses",requires_openai_auth=true}}"#
        )
    };

    plan.args = vec!["-c".to_string(), provider_toml, "-c".to_string(), r#"model_provider="ashkelon""#.to_string()];
    plan.args.extend(passthrough);
    Ok(plan)
}
