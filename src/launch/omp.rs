use super::LaunchPlan;

/// `--from-claude` imports a Claude Code session into omp's own auth-broker/session store; the
/// routing report could not rule out that resolving `provider: anthropic` afterward reuses the
/// imported Claude Code login rather than the relay-routed key, so this refuses outright instead
/// of guessing. No verified local wake channel exists for omp either (its `collab` control-link
/// feature defaults to a third-party relay, `wss://my.omp.sh`) — `wake` falls through to tmux.
pub fn plan(relay_base: &str, _launch: &str, args: &[String]) -> anyhow::Result<LaunchPlan> {
    if args.iter().any(|a| a == "--from-claude") {
        anyhow::bail!(
            "refusing to launch omp with --from-claude: it imports a Claude Code session into omp's own \
             auth-broker, and whether omp then resolves the anthropic provider through that imported login \
             instead of the relay-routed key could not be verified"
        );
    }

    let mut plan = LaunchPlan::new("omp", "omp");
    plan.env.push(("ANTHROPIC_BASE_URL".to_string(), format!("{relay_base}/anthropic")));
    plan.env.push(("OPENAI_BASE_URL".to_string(), format!("{relay_base}/openai")));
    plan.env.push(("OPENROUTER_BASE_URL".to_string(), format!("{relay_base}/openrouter")));
    plan.args = args.to_vec();
    Ok(plan)
}
