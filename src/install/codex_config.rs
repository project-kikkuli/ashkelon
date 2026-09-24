use std::path::Path;

use anyhow::Context;
use toml_edit::{value, DocumentMut, InlineTable, Item, Value};

use super::state::InstallState;

/// Points Codex at the relay persistently, the same shape `launch::codex::plan` builds for one
/// run: a ChatGPT-login provider and an API-key provider, both named so they never collide with a
/// provider the user already configured. `model_provider` is set to the ChatGPT-login one (`plan`'s
/// own default when `--openai-api` isn't passed); switching to the API-key provider is a one-line
/// edit (`model_provider = "ashkelon-api"`) the printed summary points at when it changes an
/// existing choice. `toml_edit` preserves the rest of the file's formatting and comments.
pub fn apply(
    home: &Path,
    state: &mut InstallState,
    chatgpt_base_url: &str,
    openai_base_url: &str,
) -> anyhow::Result<String> {
    let path = home.join(".codex").join("config.toml");
    state.track_file(&path)?;

    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;

    let prior_provider = doc.get("model_provider").and_then(|v| v.as_str()).map(str::to_string);

    if !doc.contains_key("model_providers") {
        doc["model_providers"] = Item::Table(toml_edit::Table::new());
    }
    let providers = doc["model_providers"]
        .as_table_like_mut()
        .with_context(|| format!("{}: model_providers is not a table", path.display()))?;
    providers.insert(
        "ashkelon",
        Item::Value(Value::InlineTable(chatgpt_provider(chatgpt_base_url))),
    );
    providers.insert(
        "ashkelon-api",
        Item::Value(Value::InlineTable(api_key_provider(openai_base_url))),
    );

    doc["model_provider"] = value("ashkelon");

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;

    let mut summary = format!(
        "{}: model_providers.ashkelon (ChatGPT login) + .ashkelon-api (API key) added, model_provider -> \"ashkelon\"",
        path.display()
    );
    if let Some(prior) = prior_provider.filter(|p| p != "ashkelon") {
        summary.push_str(&format!(
            " (was \"{prior}\"; set model_provider = \"ashkelon-api\" for an API key)"
        ));
    }
    Ok(summary)
}

fn chatgpt_provider(base_url: &str) -> InlineTable {
    let mut t = InlineTable::new();
    t.insert("name", "ashkelon".into());
    t.insert("base_url", base_url.into());
    t.insert("wire_api", "responses".into());
    t.insert("requires_openai_auth", true.into());
    t
}

fn api_key_provider(base_url: &str) -> InlineTable {
    let mut t = InlineTable::new();
    t.insert("name", "ashkelon-api".into());
    t.insert("base_url", base_url.into());
    t.insert("wire_api", "responses".into());
    t.insert("env_key", "OPENAI_API_KEY".into());
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn adds_providers_and_preserves_existing_config() {
        let home = tempdir().unwrap();
        let dir = home.path().join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "model = \"gpt-6-sol\"\n# a comment\n[projects.\"/x\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

        let mut state = InstallState::default();
        apply(
            home.path(),
            &mut state,
            "http://127.0.0.1:8484/chatgpt/backend-api/codex",
            "http://127.0.0.1:8484/openai/v1",
        )
        .unwrap();

        let written = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(written.contains("# a comment"));
        assert!(written.contains("trust_level = \"trusted\""));
        assert!(written.contains("model_provider = \"ashkelon\""));
        assert!(written.contains("[model_providers]"));
        assert!(written.contains("ashkelon = {"));
        assert!(written.contains("requires_openai_auth = true"));
        assert!(written.contains("env_key = \"OPENAI_API_KEY\""));
    }

    #[test]
    fn reports_prior_provider_when_changed() {
        let home = tempdir().unwrap();
        let dir = home.path().join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "model_provider = \"openai\"\n").unwrap();

        let mut state = InstallState::default();
        let summary = apply(home.path(), &mut state, "http://x/a", "http://x/b").unwrap();
        assert!(summary.contains("was \"openai\""));
    }

    #[test]
    fn creates_config_when_missing() {
        let home = tempdir().unwrap();
        let mut state = InstallState::default();
        apply(home.path(), &mut state, "http://x/a", "http://x/b").unwrap();
        assert!(home.path().join(".codex").join("config.toml").exists());
    }
}
