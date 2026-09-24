use anyhow::Context;
use base64::Engine;

use super::{CompanionProcess, LaunchOptions, LaunchPlan, COMPANION_URL_PLACEHOLDER};

/// opencode's providers are AI-SDK providers whose `baseURL` (including the `/v1` segment) comes from
/// `provider.<id>.options` in config, so a temp `OPENCODE_CONFIG` overlay points each at the relay.
/// `opencode` is OpenCode Zen, its own hosted gateway.
///
/// Two live-run findings this also works around:
/// - `opencode serve --port 0` does not pick a random port; it binds the default 4096 regardless,
///   so two launches collide. ashkelon picks a free loopback port itself instead (bind
///   `127.0.0.1:0`, read the port, drop the listener) and passes that as `--port`.
/// - The server otherwise warns `OPENCODE_SERVER_PASSWORD is not set; server is unsecured`. A
///   random per-launch password is generated and passed as that env var to both the `serve`
///   companion and the `attach`/`run` client (whose own `--password` defaults to reading the same
///   var, per `opencode attach --help`); `wake`'s HTTP calls carry it as Basic auth (username
///   `opencode`, `attach --help`'s documented default).
pub fn plan(relay_base: &str, launch: &str, args: &[String], _options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
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
    crate::fsperm::write_private_file(&config_path, &serde_json::to_vec_pretty(&config)?)?;
    plan.temp_files.push(config_path.clone());

    let port = free_local_port().context("picking a free local port for opencode serve")?;
    let password = random_token();

    let base_env = vec![
        (
            "OPENCODE_CONFIG".to_string(),
            config_path.to_string_lossy().into_owned(),
        ),
        ("ANTHROPIC_BASE_URL".to_string(), format!("{relay_base}/anthropic")),
        ("OPENCODE_SERVER_PASSWORD".to_string(), password.clone()),
    ];
    plan.env = base_env.clone();

    // The banner (`opencode server listening on http://host:port`, confirmed via `strings`) is a
    // live-run-confirmed stdout line, not stderr as the help text's `--print-logs` framing would
    // suggest — `main::run` reads and keeps draining both streams for exactly this reason.
    plan.companion = Some(CompanionProcess {
        program: "opencode".to_string(),
        args: vec![
            "serve".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--hostname".to_string(),
            "127.0.0.1".to_string(),
            "--print-logs".to_string(),
        ],
        env: base_env,
        ready_pattern: regex::Regex::new(r"listening on http://(?P<host>[^:\s]+):(?P<port>\d+)").expect("static regex"),
    });

    // `opencode run ...` (headless) takes `--attach`; everything else is the TUI via `opencode attach`.
    plan.args = match args.split_first() {
        Some((first, rest)) if first == "run" => {
            let mut a = vec![
                "run".to_string(),
                "--attach".to_string(),
                COMPANION_URL_PLACEHOLDER.to_string(),
            ];
            a.extend(rest.iter().cloned());
            a
        }
        _ => {
            let mut a = vec!["attach".to_string(), COMPANION_URL_PLACEHOLDER.to_string()];
            a.extend(args.iter().cloned());
            a
        }
    };
    plan.env
        .push(("OPENCODE_SERVER_PASSWORD".to_string(), password.clone()));
    plan.wake.control = Some(COMPANION_URL_PLACEHOLDER.to_string());
    plan.wake.control_auth = Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
    ));
    Ok(plan)
}

fn free_local_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
    // `listener` drops here, freeing the port back to the OS; there is an inherent (and here
    // accepted) race between that and opencode's own bind a moment later.
}

fn random_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
