use std::path::Path;

use anyhow::Context;

use super::state::InstallState;

/// Points Claude Code at the relay via `env.ANTHROPIC_BASE_URL`, merging into
/// `~/.claude/settings.json` and leaving every other key untouched. `serde_json`'s `preserve_order`
/// feature (already enabled for the whole crate) keeps existing key order stable.
pub fn apply(home: &Path, state: &mut InstallState, base_url: &str) -> anyhow::Result<String> {
    let path = home.join(".claude").join("settings.json");
    state.track_file(&path)?;

    let mut root = read_object(&path)?;
    let env = root.entry("env".to_string()).or_insert_with(|| serde_json::json!({}));
    let env_obj = env
        .as_object_mut()
        .with_context(|| format!("{}: \"env\" is not an object", path.display()))?;
    env_obj.insert("ANTHROPIC_BASE_URL".to_string(), serde_json::json!(base_url));

    write_object(&path, &root)?;
    Ok(format!("{}: env.ANTHROPIC_BASE_URL -> {base_url}", path.display()))
}

fn read_object(path: &Path) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(serde_json::Map::new()),
        Ok(s) => {
            let value: serde_json::Value =
                serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
            value
                .as_object()
                .cloned()
                .with_context(|| format!("{} is not a JSON object", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_object(path: &Path, obj: &serde_json::Map<String, serde_json::Value>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(obj)?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn merges_into_existing_settings_preserving_other_keys() {
        let home = tempdir().unwrap();
        let dir = home.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"model":"opus","env":{"FOO":"bar"}}"#).unwrap();

        let mut state = InstallState::default();
        apply(home.path(), &mut state, "http://127.0.0.1:8484").unwrap();

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(written["model"], "opus");
        assert_eq!(written["env"]["FOO"], "bar");
        assert_eq!(written["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8484");
    }

    #[test]
    fn creates_settings_when_missing() {
        let home = tempdir().unwrap();
        let mut state = InstallState::default();
        apply(home.path(), &mut state, "http://127.0.0.1:8484").unwrap();
        let path = home.path().join(".claude").join("settings.json");
        assert!(path.exists());
        assert!(!state.files[0].existed_before);
    }

    #[test]
    fn rerun_does_not_clobber_the_original_snapshot() {
        let home = tempdir().unwrap();
        let dir = home.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"model":"opus"}"#).unwrap();

        let mut state = InstallState::default();
        apply(home.path(), &mut state, "http://127.0.0.1:8484").unwrap();
        apply(home.path(), &mut state, "http://127.0.0.1:9999").unwrap();

        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].prior_content.as_deref(), Some(r#"{"model":"opus"}"#));
    }
}
