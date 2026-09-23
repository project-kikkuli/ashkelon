use crate::config::TransformConfig;
use crate::wire::Wire;

pub struct Applied {
    pub body: Vec<u8>,
    /// Names of the transforms that changed something; empty means `body` is the original bytes.
    pub changed: Vec<String>,
}

/// Rewrites a request body. Thinking/reasoning blocks, their signatures, and encrypted reasoning are never touched.
pub fn apply(_wire: Wire, _cfg: &TransformConfig, body: &[u8]) -> Applied {
    Applied { body: body.to_vec(), changed: Vec::new() }
}

/// A ping already shown to the model, pinned to the conversation position it was first delivered at so every
/// later request re-inserts it in the same place (keeps the prompt-cache prefix stable).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PinnedPing {
    pub id: String,
    /// Index of the conversation item (message / input item) the ping was attached to.
    pub anchor: usize,
    pub text: String,
}

/// Returns the body with `pings` inserted, or None when the body cannot carry them
/// (unparseable, or an anchor no longer exists because the conversation was compacted or rewound).
pub fn inject_pings(_wire: Wire, _body: &[u8], _pings: &[PinnedPing]) -> Option<Vec<u8>> {
    None
}

/// Number of conversation items in a request, used as the anchor for a newly delivered ping.
pub fn conversation_len(_wire: Wire, _body: &[u8]) -> Option<usize> {
    None
}
