use std::path::PathBuf;

use anyhow::Context;

use super::{build_home_overlay, LaunchOptions, LaunchPlan, OverlayHome};

const MODELS_FILE: &str = "models.yml";

/// omp reads `ANTHROPIC_BASE_URL`/`OPENAI_BASE_URL` as plain env vars (both literal strings in
/// the installed binary), but does **not** read `OPENROUTER_BASE_URL` at all — the string is
/// absent from the binary, confirmed by `strings`, so setting it (as the routing report
/// originally assumed) silently does nothing. openrouter, the ChatGPT-subscription
/// `openai-codex` provider, and Ollama are instead routed by overriding `providers.<id>.baseUrl`
/// in `models.yml` — the documented per-provider config-override mechanism (`strings` shows
/// `"config override (models.yml)"` ranked as its own named config source alongside env/runtime
/// overrides, and error text pointing at `providers.${provider}` keys, e.g.
/// `compat.replayUnsignedThinking` "in your models.yml"; every built-in provider entry in the
/// binary carries the same `baseUrl` field name this reuses).
///
/// Session/model state lives under `PI_CODING_AGENT_DIR` (default `~/.omp/agent`, confirmed via
/// `--help`'s env var reference). Rather than redirect that env var wholesale — which would start
/// omp with none of the user's real sessions or model cache — this builds the same kind of temp
/// overlay as Hermes (`launch::build_home_overlay`): every entry of the real agent dir is
/// symlinked through except `models.yml`, which is a copy carrying the three added `baseUrl`
/// overrides merged with anything the user already has there. `main::run` reconciles anything
/// omp materializes fresh in the overlay back into the real agent dir on exit.
///
/// `--from-claude` imports a Claude Code session **transcript** into omp's own session store —
/// `omp --help` says so directly ("Import a Claude Code session into OMP"), and the binary has
/// no reference anywhere to the macOS Keychain "Claude Code-credentials" entry or
/// `~/.claude/.credentials.json` (both `grep -c` zero hits; the few `.claude/`-adjacent strings
/// are unrelated Claude-Code-compatibility features — loading `.claude/commands`, `.claude/mcp.json`,
/// etc. — and the few `credentials.json` hits are gcloud's `application_default_credentials.json`).
/// So unlike Hermes's `resolve_anthropic_token()`, there is no credential-import hazard here to
/// refuse; omp is launched exactly like any other harness.
pub fn plan(relay_base: &str, launch: &str, args: &[String], options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
    let real_agent_dir = agent_dir();
    let overlay_dir = options.state_dir.join("launch").join(format!("{launch}-omp-agent"));

    let real_models_path = real_agent_dir.join(MODELS_FILE);
    let real_models_text = std::fs::read_to_string(&real_models_path).unwrap_or_default();
    let real_models: serde_yaml::Value = if real_models_text.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str(&real_models_text).context("parsing the real omp models.yml")?
    };

    let overlaid_models = with_relay_overrides(real_models, relay_base);

    build_home_overlay(&real_agent_dir, &overlay_dir, MODELS_FILE).context("building the omp agent-dir overlay")?;
    let models_path = overlay_dir.join(MODELS_FILE);
    std::fs::write(&models_path, serde_yaml::to_string(&overlaid_models)?)
        .context("writing the overlaid omp models.yml")?;

    let mut plan = LaunchPlan::new("omp", "omp");
    plan.env
        .push(("ANTHROPIC_BASE_URL".to_string(), format!("{relay_base}/anthropic")));
    plan.env
        .push(("OPENAI_BASE_URL".to_string(), format!("{relay_base}/openai/v1")));
    plan.env.push((
        "PI_CODING_AGENT_DIR".to_string(),
        overlay_dir.to_string_lossy().into_owned(),
    ));
    plan.args = args.to_vec();
    plan.overlay_home = Some(OverlayHome {
        overlay_dir,
        real_home: real_agent_dir,
        generated_file_name: MODELS_FILE.to_string(),
    });
    Ok(plan)
}

fn agent_dir() -> PathBuf {
    if let Ok(val) = std::env::var("PI_CODING_AGENT_DIR") {
        if !val.trim().is_empty() {
            return PathBuf::from(val);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omp")
        .join("agent")
}

fn with_relay_overrides(mut models: serde_yaml::Value, relay_base: &str) -> serde_yaml::Value {
    if !models.is_mapping() {
        models = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let mapping = models.as_mapping_mut().expect("just ensured this is a mapping");

    let providers_key = serde_yaml::Value::String("providers".to_string());
    let providers = mapping
        .entry(providers_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !providers.is_mapping() {
        *providers = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let providers_mapping = providers.as_mapping_mut().expect("just ensured this is a mapping");

    set_base_url(
        providers_mapping,
        "openrouter",
        format!("{relay_base}/openrouter/api/v1"),
    );
    set_base_url(
        providers_mapping,
        "openai-codex",
        format!("{relay_base}/chatgpt/backend-api"),
    );
    set_base_url(providers_mapping, "ollama", format!("{relay_base}/ollama/v1"));

    models
}

fn set_base_url(providers: &mut serde_yaml::Mapping, id: &str, base_url: String) {
    let key = serde_yaml::Value::String(id.to_string());
    let entry = providers
        .entry(key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !entry.is_mapping() {
        *entry = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    entry.as_mapping_mut().expect("just ensured this is a mapping").insert(
        serde_yaml::Value::String("baseUrl".to_string()),
        serde_yaml::Value::String(base_url),
    );
}
