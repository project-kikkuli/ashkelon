use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `~/.config/ashkelon/config.toml`. Every section is optional.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: Option<String>,
    pub log_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    /// Record request and response bodies next to the call log. Off by default.
    pub log_bodies: bool,
    pub routes: Vec<RouteConfig>,
    pub transforms: TransformConfig,
    pub rules: RuleConfig,
    pub hooks: Vec<HookConfig>,
    pub pings: PingConfig,
    pub models: Vec<ModelConfig>,
}

/// Extra upstreams beyond the built-in ones. Requests to `/<name>/...` go to `upstream` + `...`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub name: String,
    pub upstream: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransformConfig {
    pub tool_output: Option<ToolOutputTrim>,
    pub strip: Vec<StripRule>,
}

/// Shortens tool output longer than `max_chars`, keeping the head and tail and marking the cut.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOutputTrim {
    pub max_chars: usize,
    pub keep_head: usize,
    pub keep_tail: usize,
}

/// Removes text matching `pattern` from user-side text blocks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StripRule {
    pub name: String,
    pub pattern: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuleConfig {
    /// When non-empty, requests for any other model are rejected. Entries are regexes.
    pub allow_models: Vec<String>,
    pub max_output_tokens: Option<u64>,
    /// Cut a streaming response once its visible output exceeds this many characters.
    pub max_response_chars: Option<usize>,
    pub cut_patterns: Vec<CutPattern>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutPattern {
    pub name: String,
    pub pattern: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    Prompt,
    ToolCall,
    ToolResult,
    TurnEnd,
    Compaction,
}

/// A command run in the background when `on` fires. It reads the event as JSON on stdin and writes
/// `{"status":"pass"|"fail","message":...,"fix":...}` to stdout. A failure becomes a ping.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    pub name: String,
    pub on: Vec<HookEvent>,
    pub command: Vec<String>,
    /// Only fire for sessions whose working directory is under one of these paths (`~` expanded).
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub harnesses: Vec<String>,
    #[serde(default = "default_hook_timeout")]
    pub timeout_secs: u64,
}

fn default_hook_timeout() -> u64 {
    120
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PingConfig {
    pub max_per_session: u32,
    /// Wake an idle agent to deliver a ping instead of waiting for its next request.
    pub wake_idle: bool,
    pub idle_after_secs: u64,
    pub max_concurrent_hooks: usize,
}

impl Default for PingConfig {
    fn default() -> Self {
        Self {
            max_per_session: 20,
            wake_idle: true,
            idle_after_secs: 20,
            max_concurrent_hooks: 4,
        }
    }
}

/// A model hooks may call through `ashkelon model`. Never a harness's subscription login.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub name: String,
    /// `anthropic`, `openai` (Responses), or `openai_chat` (any OpenAI-compatible server, including local ones).
    pub api: String,
    pub base_url: String,
    pub model: String,
    /// Environment variable holding the API key; omit for local servers that need none.
    pub api_key_env: Option<String>,
}

impl Config {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ashkelon")
            .join("config.toml")
    }

    pub fn load(path: Option<&std::path::Path>) -> anyhow::Result<Config> {
        let path = path.map(PathBuf::from).unwrap_or_else(Config::default_path);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn log_dir(&self) -> PathBuf {
        self.log_dir.clone().unwrap_or_else(|| state_home().join("logs"))
    }

    pub fn state_dir(&self) -> PathBuf {
        self.state_dir.clone().unwrap_or_else(state_home)
    }

    /// The address to bind, defaulting to `127.0.0.1:8484`. Refuses anything but loopback: the
    /// relay forwards callers' own auth headers to real providers, so a reachable-from-the-network
    /// bind would turn it into an open proxy for whoever can reach that address.
    pub fn listen_addr(&self) -> anyhow::Result<String> {
        let addr = self.listen.clone().unwrap_or_else(|| "127.0.0.1:8484".to_string());
        validate_loopback_listen_addr(&addr)?;
        Ok(addr)
    }
}

fn validate_loopback_listen_addr(addr: &str) -> anyhow::Result<()> {
    if is_loopback_addr(addr) {
        return Ok(());
    }
    anyhow::bail!(
        "listen = \"{addr}\" is not a loopback address. ashkelon forwards callers' own auth \
         headers straight to the real provider, so binding anything reachable from the network \
         turns it into an open proxy. Use a 127.0.0.0/8 address, ::1, or localhost."
    )
}

fn is_loopback_addr(addr: &str) -> bool {
    if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
        return sock.ip().is_loopback();
    }
    let host = addr.rsplit_once(':').map_or(addr, |(host, _)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn state_home() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ashkelon")
}

#[cfg(test)]
mod listen_addr_tests {
    use super::*;

    #[test]
    fn accepts_loopback_forms() {
        assert!(is_loopback_addr("127.0.0.1:8484"));
        assert!(is_loopback_addr("127.5.6.7:1"));
        assert!(is_loopback_addr("[::1]:8484"));
        assert!(is_loopback_addr("localhost:8484"));
        assert!(is_loopback_addr("LOCALHOST:8484"));
    }

    #[test]
    fn rejects_non_loopback_forms() {
        assert!(!is_loopback_addr("0.0.0.0:8484"));
        assert!(!is_loopback_addr("[::]:8484"));
        assert!(!is_loopback_addr("192.168.1.5:8484"));
        assert!(!is_loopback_addr("example.com:8484"));
        assert!(!is_loopback_addr("10.0.0.1:8484"));
    }

    #[test]
    fn config_listen_addr_defaults_to_loopback() {
        let cfg = Config::default();
        assert_eq!(cfg.listen_addr().unwrap(), "127.0.0.1:8484");
    }

    #[test]
    fn config_listen_addr_rejects_configured_non_loopback() {
        let cfg = Config {
            listen: Some("0.0.0.0:8484".to_string()),
            ..Config::default()
        };
        let err = cfg.listen_addr().unwrap_err();
        assert!(err.to_string().contains("not a loopback address"));
    }

    #[test]
    fn config_listen_addr_accepts_configured_loopback() {
        let cfg = Config {
            listen: Some("127.0.0.1:9999".to_string()),
            ..Config::default()
        };
        assert_eq!(cfg.listen_addr().unwrap(), "127.0.0.1:9999");
    }
}
