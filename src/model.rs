use crate::config::Config;

/// Calls a model from `[[models]]` for a hook. Returns the response text.
pub async fn complete(_cfg: &Config, _name: &str, _system: Option<&str>, _prompt: &str) -> anyhow::Result<String> {
    anyhow::bail!("not implemented")
}
