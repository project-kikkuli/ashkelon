use super::LaunchPlan;

/// No local message-injection channel exists (`--remote-control` requires Anthropic's own
/// subscription-gated bridge relay; see wake-research notes) — `wake` falls straight through to
/// tmux for this harness.
pub fn plan(relay_base: &str, _launch: &str, args: &[String]) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("claude", "claude");
    plan.env.push(("ANTHROPIC_BASE_URL".to_string(), format!("{relay_base}/anthropic")));
    plan.args = args.to_vec();
    Ok(plan)
}
