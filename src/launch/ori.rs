use super::{LaunchOptions, LaunchPlan};

/// Live-verified (routing report): Ori appends `/api/v1` itself to `ORI_OPENROUTER_BASE_URL`
/// (`ORI_OPENROUTER_BASE_URL=http://127.0.0.1:PORT` resolved to endpoint
/// `http://127.0.0.1:PORT/api/v1`), so the relay route is passed without that suffix here.
pub fn plan(relay_base: &str, _launch: &str, args: &[String], _options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("ori", "ori");
    plan.env.push(("ORI_OPENROUTER_BASE_URL".to_string(), format!("{relay_base}/openrouter")));
    plan.args = args.to_vec();
    Ok(plan)
}
