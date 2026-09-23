use crate::session::SessionKey;

/// How a launched agent can be reached when it is idle.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct WakeTarget {
    pub harness: String,
    pub tmux_pane: Option<String>,
    /// Harness-specific control endpoint (app-server socket, `opencode serve` URL, ...).
    pub control: Option<String>,
    pub harness_session_id: Option<String>,
}

/// Delivers `text` to an idle agent as a new user turn. Returns Ok(false) when this target cannot be woken.
pub async fn wake(_target: &WakeTarget, _session: &SessionKey, _text: &str) -> anyhow::Result<bool> {
    Ok(false)
}
