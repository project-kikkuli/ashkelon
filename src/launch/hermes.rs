use std::path::{Path, PathBuf};

use anyhow::Context;

use super::{build_home_overlay, LaunchOptions, LaunchPlan, OverlayHome};

const PROVIDER_NAME: &str = "ashkelon-relay";
const CONFIG_FILE: &str = "config.yaml";

/// Routes Hermes through the relay via its own isolated named-custom-provider mechanism
/// (`hermes_cli/runtime_provider.py::_get_named_custom_provider`, reached by `--provider
/// ashkelon-relay` selecting a `providers:` entry in config.yaml) rather than by overriding the
/// built-in `anthropic`/`openrouter` providers' `base_url` in place. That distinction is the
/// whole point: the named-provider path reads its credential straight from `key_env`/`api_key`
/// on the entry and never calls `resolve_anthropic_token()` or the OpenRouter credential pool —
/// verified from source, both of which the built-in providers DO call regardless of base_url:
///
/// - `agent.anthropic_adapter.resolve_anthropic_token()` (used whenever `provider == "anthropic"`
///   in the ordinary, non-explicit path) checks `ANTHROPIC_TOKEN` env, then
///   `CLAUDE_CODE_OAUTH_TOKEN` env, then the macOS Keychain "Claude Code-credentials" entry
///   (refreshing it if expired), and only *last* falls back to `ANTHROPIC_API_KEY`. So a machine
///   with both `ANTHROPIC_API_KEY` set AND a valid Claude Code login would still resolve the
///   keychain credential even with `model.base_url` pointed at the relay — overriding just the
///   base_url is not sufficient to avoid the keychain read.
/// - `PROVIDER_REGISTRY["anthropic"].api_key_env_vars` lists `ANTHROPIC_API_KEY` first, but that
///   ordering is never actually consulted for `provider == "anthropic"`: the bespoke branch above
///   always wins first. Registry-driven `resolve_api_key_provider_credentials` is reached only
///   for providers with no bespoke handling (zai, kimi, deepseek, ...), not anthropic.
/// - Plain `openrouter` IS just an env var in the ordinary case (`_resolve_openrouter_runtime`
///   reads `OPENROUTER_BASE_URL`/`OPENROUTER_API_KEY` and never touches a keychain), but Hermes's
///   own hosted OAuth-balanced credential "pool" is tried FIRST unless a custom endpoint env var
///   is already set — so an env-var-only reroute is fragile in exactly the same shape as the
///   anthropic case (a hidden credential source can win over the portable one), and the named
///   custom provider sidesteps it identically. One mechanism for both, verified to bypass both
///   hazards by construction rather than by enumerating every resolver's precedence.
/// - There is no plain `"openai"` provider in Hermes at all (`PROVIDER_REGISTRY`/its alias table
///   have no such entry; `OPENAI_API_KEY` is only ever consulted as a hint that the OpenRouter
///   fallback should be used). So only anthropic and openrouter are supported here.
///
/// The named-provider path is reachable only via `config.yaml` under `HERMES_HOME`
/// (`hermes_constants.get_hermes_home`, default `~/.hermes`), and Hermes has no config-only
/// override — only the whole-home env var. Redirecting that wholesale would silently start
/// Hermes with none of the user's real skills/plugins/persona/sessions, so instead this builds a
/// temporary overlay home: every entry of the real home is symlinked in unchanged except
/// `config.yaml`, which is a copy carrying one added `providers:` entry. Everything Hermes reads
/// or writes under an existing top-level name (sessions/, state.db(+wal/shm), logs/, skills/,
/// memories/, auth.json, SOUL.md, ...) therefore lands in the real home exactly as before; only a
/// brand-new top-level name Hermes might materialize is reconciled back in afterward (see
/// `launch::reconcile_home_overlay`, invoked by `main::run` once the process exits).
pub fn plan(relay_base: &str, launch: &str, args: &[String], options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
    let real_home = hermes_home();
    let overlay_dir = options.state_dir.join("launch").join(format!("{launch}-hermes-home"));

    let real_config_path = real_home.join(CONFIG_FILE);
    let real_config_text = std::fs::read_to_string(&real_config_path).unwrap_or_default();
    let real_config: serde_yaml::Value = if real_config_text.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str(&real_config_text).context("parsing the real Hermes config.yaml")?
    };

    let configured_provider = configured_provider_name(&real_config);
    let (relay_api, key_env, api_mode) = detect_relay_route(relay_base, configured_provider.as_deref(), &real_home)?;

    let overlaid_config = with_relay_provider(real_config, &relay_api, key_env, api_mode);

    build_home_overlay(&real_home, &overlay_dir, CONFIG_FILE).context("building the Hermes home overlay")?;
    let config_path = overlay_dir.join(CONFIG_FILE);
    std::fs::write(&config_path, serde_yaml::to_string(&overlaid_config)?)
        .context("writing the overlaid Hermes config.yaml")?;

    let mut plan = LaunchPlan::new("hermes", "hermes");
    plan.env
        .push(("HERMES_HOME".to_string(), overlay_dir.to_string_lossy().into_owned()));
    plan.args = vec!["--provider".to_string(), PROVIDER_NAME.to_string()];
    plan.args.extend(args.iter().cloned());
    plan.overlay_home = Some(OverlayHome {
        overlay_dir,
        real_home,
        generated_file_name: CONFIG_FILE.to_string(),
    });
    // No verified local wake channel for Hermes (never executed to check further, per the hard
    // safety rule); tmux is the only path.
    Ok(plan)
}

fn hermes_home() -> PathBuf {
    if let Ok(val) = std::env::var("HERMES_HOME") {
        if !val.trim().is_empty() {
            return PathBuf::from(val);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".hermes")
}

fn configured_provider_name(config: &serde_yaml::Value) -> Option<String> {
    config
        .get("model")?
        .get("provider")?
        .as_str()
        .map(|s| s.trim().to_lowercase())
}

fn detect_relay_route(
    relay_base: &str,
    configured: Option<&str>,
    real_home: &Path,
) -> anyhow::Result<(String, &'static str, &'static str)> {
    let normalized = configured.unwrap_or("auto");
    let is_anthropic = matches!(normalized, "anthropic" | "claude" | "claude-code");
    let is_openrouter = normalized == "openrouter";

    if is_anthropic {
        if has_env_credential(real_home, "ANTHROPIC_API_KEY") {
            return Ok((
                format!("{relay_base}/anthropic"),
                "ANTHROPIC_API_KEY",
                "anthropic_messages",
            ));
        }
        anyhow::bail!(
            "refusing to launch hermes: its configured provider is anthropic but no ANTHROPIC_API_KEY is set \
             (checked the environment and HERMES_HOME/.env) — the only remaining credential source is the \
             macOS Keychain \"Claude Code-credentials\" entry Hermes's native anthropic provider reads, which \
             ashkelon must never touch"
        );
    }

    if is_openrouter || normalized == "auto" {
        if has_env_credential(real_home, "OPENROUTER_API_KEY") {
            return Ok((
                format!("{relay_base}/openrouter/api/v1"),
                "OPENROUTER_API_KEY",
                "chat_completions",
            ));
        }
        if is_openrouter {
            anyhow::bail!(
                "refusing to launch hermes: its configured provider is openrouter but no OPENROUTER_API_KEY is set \
                 (checked the environment and HERMES_HOME/.env)"
            );
        }
    }

    anyhow::bail!(
        "hermes is not supported for provider \"{normalized}\": ashkelon only relays a configured anthropic \
         (with ANTHROPIC_API_KEY) or openrouter (with OPENROUTER_API_KEY) provider; a custom base_url provider \
         has no relay route to add at runtime"
    )
}

fn has_env_credential(real_home: &Path, name: &str) -> bool {
    if std::env::var(name).map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return true;
    }
    dotenv_value(&real_home.join(".env"), name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Minimal `.env` reader: `NAME=value` lines, `#` comments, no quoting/escaping/expansion —
/// Hermes's own loader is more capable, but this only needs to answer "is a value present".
fn dotenv_value(path: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == name {
            return Some(value.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

fn with_relay_provider(mut config: serde_yaml::Value, api: &str, key_env: &str, api_mode: &str) -> serde_yaml::Value {
    if !config.is_mapping() {
        config = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let mapping = config.as_mapping_mut().expect("just ensured this is a mapping");

    let providers_key = serde_yaml::Value::String("providers".to_string());
    let providers = mapping
        .entry(providers_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !providers.is_mapping() {
        *providers = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let providers_mapping = providers.as_mapping_mut().expect("just ensured this is a mapping");

    let mut entry = serde_yaml::Mapping::new();
    entry.insert(
        serde_yaml::Value::String("api".to_string()),
        serde_yaml::Value::String(api.to_string()),
    );
    entry.insert(
        serde_yaml::Value::String("key_env".to_string()),
        serde_yaml::Value::String(key_env.to_string()),
    );
    entry.insert(
        serde_yaml::Value::String("api_mode".to_string()),
        serde_yaml::Value::String(api_mode.to_string()),
    );
    providers_mapping.insert(
        serde_yaml::Value::String(PROVIDER_NAME.to_string()),
        serde_yaml::Value::Mapping(entry),
    );

    config
}
