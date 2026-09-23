mod codex;
mod opencode;
pub mod runner;
mod tmux;

use runner::{CommandRunner, HttpPoster, SystemPoster, SystemRunner};

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
pub async fn wake(target: &WakeTarget, session: &SessionKey, text: &str) -> anyhow::Result<bool> {
    wake_with(&SystemRunner, &SystemPoster, target, session, text).await
}

/// Same as [`wake`], with the command runner and HTTP poster injectable for tests.
pub async fn wake_with(
    commands: &dyn CommandRunner,
    http: &dyn HttpPoster,
    target: &WakeTarget,
    _session: &SessionKey,
    text: &str,
) -> anyhow::Result<bool> {
    if let Some(woken) = try_verified_adapter(commands, http, target, text).await? {
        if woken {
            return Ok(true);
        }
    }
    if let Some(pane) = &target.tmux_pane {
        return tmux::send(commands, pane, text).await;
    }
    Ok(false)
}

/// Returns `None` when no verified adapter applies to this target at all (so the caller should
/// try tmux); `Some(false)` when the adapter applies but the attempt itself failed (the caller
/// still falls back to tmux, in case the harness-specific channel merely raced or went stale).
async fn try_verified_adapter(
    commands: &dyn CommandRunner,
    http: &dyn HttpPoster,
    target: &WakeTarget,
    text: &str,
) -> anyhow::Result<Option<bool>> {
    match target.harness.as_str() {
        "codex" => {
            let Some(thread) = &target.harness_session_id else { return Ok(None) };
            Ok(Some(codex::queue_message(commands, thread, text).await?))
        }
        "opencode" => {
            let (Some(control), Some(session_id)) = (&target.control, &target.harness_session_id) else {
                return Ok(None);
            };
            Ok(Some(opencode::send_message(http, control, session_id, text).await?))
        }
        // claude, omp, hermes, ori, cursor: no verified local channel (see wake-research notes);
        // tmux is the only path.
        _ => Ok(None),
    }
}
