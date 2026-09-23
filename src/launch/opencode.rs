use super::{CompanionProcess, LaunchPlan, COMPANION_URL_PLACEHOLDER};

/// opencode's providers are AI-SDK providers whose `baseURL` (including the `/v1` segment) comes from
/// `provider.<id>.options` in config, so a temp `OPENCODE_CONFIG` overlay points each at the relay.
/// `opencode` is OpenCode Zen, its own hosted gateway.
pub fn plan(relay_base: &str, launch: &str, args: &[String]) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("opencode", "opencode");

    let config = serde_json::json!({
        "provider": {
            "anthropic": { "options": { "baseURL": format!("{relay_base}/anthropic/v1") } },
            "openai": { "options": { "baseURL": format!("{relay_base}/openai/v1") } },
            "openrouter": { "options": { "baseURL": format!("{relay_base}/openrouter/api/v1") } },
            "opencode": { "options": { "baseURL": format!("{relay_base}/opencode/zen/v1") } },
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

    // `opencode run ...` (headless) takes `--attach`; everything else is the TUI via `opencode attach`.
    plan.args = match args.split_first() {
        Some((first, rest)) if first == "run" => {
            let mut a = vec!["run".to_string(), "--attach".to_string(), COMPANION_URL_PLACEHOLDER.to_string()];
            a.extend(rest.iter().cloned());
            a
        }
        _ => {
            let mut a = vec!["attach".to_string(), COMPANION_URL_PLACEHOLDER.to_string()];
            a.extend(args.iter().cloned());
            a
        }
    };
    plan.wake.control = Some(COMPANION_URL_PLACEHOLDER.to_string());
    Ok(plan)
}
