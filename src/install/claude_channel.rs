use std::path::{Path, PathBuf};

use anyhow::Context;

use super::state::InstallState;

/// Registers the channel MCP server at Claude Code user scope: the same shape
/// `claude mcp add --scope user ashkelon -- <bin> channel ...` writes into `~/.claude.json`
/// (confirmed by running that exact command against a throwaway `HOME` and reading the result),
/// built directly here instead of shelling out to `claude` so install doesn't depend on it being
/// on `PATH` or already logged in.
pub fn apply(
    home: &Path,
    state: &mut InstallState,
    bin: &Path,
    socket_dir: &Path,
    register_url: &str,
) -> anyhow::Result<String> {
    let path = home.join(".claude.json");
    state.track_file(&path)?;

    let mut root = read_object(&path)?;
    let servers = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .with_context(|| format!("{}: \"mcpServers\" is not an object", path.display()))?;
    servers_obj.insert(
        "ashkelon".to_string(),
        serde_json::json!({
            "type": "stdio",
            "command": bin.to_string_lossy(),
            "args": [
                "channel",
                "--socket-dir", socket_dir.to_string_lossy(),
                "--register-url", register_url,
            ],
            "env": {},
        }),
    );

    write_object(&path, &root)?;
    Ok(format!(
        "{}: mcpServers.ashkelon registered (user scope)",
        path.display()
    ))
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

pub fn default_socket_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("channel")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn registers_server_preserving_other_top_level_keys() {
        let home = tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{"userID":"abc","mcpServers":{}}"#).unwrap();

        let mut state = InstallState::default();
        apply(
            home.path(),
            &mut state,
            Path::new("/usr/local/bin/ashkelon"),
            Path::new("/state/channel"),
            "http://127.0.0.1:8484/internal/claude-channel",
        )
        .unwrap();

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap()).unwrap();
        assert_eq!(written["userID"], "abc");
        assert_eq!(written["mcpServers"]["ashkelon"]["type"], "stdio");
        assert_eq!(written["mcpServers"]["ashkelon"]["command"], "/usr/local/bin/ashkelon");
    }

    #[test]
    fn creates_file_when_missing() {
        let home = tempdir().unwrap();
        let mut state = InstallState::default();
        apply(
            home.path(),
            &mut state,
            Path::new("/bin/ashkelon"),
            Path::new("/state/channel"),
            "http://x/register",
        )
        .unwrap();
        assert!(home.path().join(".claude.json").exists());
    }
}
