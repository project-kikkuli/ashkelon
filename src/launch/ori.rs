use super::{LaunchOptions, LaunchPlan};

/// The routing report's claim that Ori appends `/api/v1` itself was wrong for any base URL
/// carrying a real path, confirmed by reading `ori`'s own base-URL resolver (`Fy`) out of the
/// installed binary via `strings`: it only fills in `/api/v1` when the URL's path is empty or
/// exactly `/api` —
/// `i.pathname = (a === "" || a === "/api") ? "/api/v1" : a` — and otherwise uses the given path
/// verbatim. ashkelon's relay path is `/s/<launch>/openrouter`, which matches neither case, so
/// `ORI_OPENROUTER_BASE_URL={relay_base}/openrouter` made every real request land at
/// `.../openrouter/<rest>` with no `/api/v1` segment at all — live-reproduced as `ori hermes`
/// fetching `GET .../openrouter/models` and getting OpenRouter's marketing HTML back (routed
/// through the relay to `https://openrouter.ai/models`, not the JSON API), then failing the
/// actual completion with a bogus "model not found". Appending `/api/v1` here, the same
/// convention every other launcher already uses for its own openrouter route (see
/// `launch::omp`), makes Ori's resolved path exactly `/api` and keeps its own logic a no-op.
pub fn plan(relay_base: &str, _launch: &str, args: &[String], _options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("ori", "ori");
    plan.env.push((
        "ORI_OPENROUTER_BASE_URL".to_string(),
        format!("{relay_base}/openrouter/api/v1"),
    ));
    plan.args = args.to_vec();
    Ok(plan)
}
