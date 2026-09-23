use serde::{Deserialize, Serialize};

/// Identifies one agent conversation across calls.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    /// Set when the call came through `ashkelon run`, from the `/s/<launch>/` path prefix.
    pub launch: Option<String>,
    pub harness: Option<String>,
    /// The harness's own session id when it sends one, else a fingerprint of the conversation's opening.
    pub session: String,
}
