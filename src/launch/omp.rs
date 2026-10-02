use std::path::PathBuf;

use anyhow::Context;

use super::{build_home_overlay, LaunchOptions, LaunchPlan, OverlayHome};

const MODELS_FILE: &str = "models.yml";

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
    crate::fsperm::write_private_file(&models_path, serde_yaml::to_string(&overlaid_models)?.as_bytes())
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
    plan.program = "sh".into();
    plan.args = vec![
        "-c".into(),
        "omp models refresh ollama --json >/dev/null; exec omp \"$@\"".into(),
        "ashkelon-omp".into(),
    ];
    plan.args.extend_from_slice(args);
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
        None,
    );
    set_base_url(
        providers_mapping,
        "openai-codex",
        format!("{relay_base}/chatgpt/backend-api"),
        None,
    );
    set_base_url(
        providers_mapping,
        "ollama",
        format!("{}/v1", relay_base.split("/s/").next().unwrap_or(relay_base)),
        Some("ollama"),
    );

    models
}

fn set_base_url(providers: &mut serde_yaml::Mapping, id: &str, base_url: String, discovery_type: Option<&str>) {
    let key = serde_yaml::Value::String(id.to_string());
    let entry = providers
        .entry(key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !entry.is_mapping() {
        *entry = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let entry_mapping = entry.as_mapping_mut().expect("just ensured this is a mapping");
    entry_mapping.insert(
        serde_yaml::Value::String("baseUrl".to_string()),
        serde_yaml::Value::String(base_url),
    );
    let Some(discovery_type) = discovery_type else {
        return;
    };
    for (key, value) in [("api", "openai-responses"), ("auth", "none")] {
        entry_mapping
            .entry(serde_yaml::Value::String(key.into()))
            .or_insert_with(|| serde_yaml::Value::String(value.into()));
    }
    let discovery_key = serde_yaml::Value::String("discovery".to_string());
    let discovery_entry = entry_mapping
        .entry(discovery_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !discovery_entry.is_mapping() {
        *discovery_entry = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    discovery_entry
        .as_mapping_mut()
        .expect("just ensured this is a mapping")
        .insert(
            serde_yaml::Value::String("type".to_string()),
            serde_yaml::Value::String(discovery_type.to_string()),
        );
}
