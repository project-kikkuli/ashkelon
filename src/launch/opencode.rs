use super::{CompanionProcess, LaunchPlan, COMPANION_URL_PLACEHOLDER};

/// opencode's built-in Anthropic provider reads `ANTHROPIC_BASE_URL` directly; OpenAI/OpenRouter
/// (AI-SDK providers) only take a `baseURL` from `provider.<id>.options` in config, hence the
/// temp `OPENCODE_CONFIG` overlay alongside the env var.
pub fn plan(relay_base: &str, launch: &str, args: &[String]) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("opencode", "opencode");

    let config = serde_json::json!({
        "provider": {
            "openai": { "options": { "baseURL": format!("{relay_base}/openai") } },
            "openrouter": { "options": { "baseURL": format!("{relay_base}/openrouter") } },
        }
    });
    let config_path = std::env::temp_dir().join(format!("ashkelon-{launch}-opencode-config.json"));
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;
    plan.temp_files.push(config_path.clone());

    let base_env = vec![
        ("OPENCODE_CONFIG".to_string(), config_path.to_string_lossy().into_owned()),
        ("ANTHROPIC_BASE_URL".to_string(), format!("{relay_base}/anthropic")),
    ];
    plan.env = base_env.clone();

    // The server binds an ephemeral port (`--port 0`); the launcher discovers it from this
    // banner line on stderr (`--print-logs`, since plain-stdout logging goes to a log file
    // instead) and substitutes it for `COMPANION_URL_PLACEHOLDER` below.
    plan.companion = Some(CompanionProcess {
        program: "opencode".to_string(),
        args: vec![
            "serve".to_string(),
            "--port".to_string(),
            "0".to_string(),
            "--hostname".to_string(),
            "127.0.0.1".to_string(),
            "--print-logs".to_string(),
        ],
        env: base_env,
        ready_pattern: regex::Regex::new(r"listening on http://(?P<host>[^:\s]+):(?P<port>\d+)")
            .expect("static regex"),
    });

    plan.args = vec!["attach".to_string(), COMPANION_URL_PLACEHOLDER.to_string()];
    plan.args.extend(args.iter().cloned());
    plan.wake.control = Some(COMPANION_URL_PLACEHOLDER.to_string());
    Ok(plan)
}
