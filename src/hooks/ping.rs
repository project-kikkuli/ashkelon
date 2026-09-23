use sha2::{Digest, Sha256};

use crate::transform::PinnedPing;

/// A hook failure not yet attached to any request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ping {
    pub id: String,
    pub hook: String,
    pub text: String,
}

impl Ping {
    pub fn new(hook: &str, message: &str, fix: Option<&str>) -> Ping {
        let id = ping_id(hook, message);
        let text = format_ping(hook, &id, message, fix);
        Ping {
            id,
            hook: hook.to_string(),
            text,
        }
    }
}

/// First 12 hex characters of sha256(hook + message), used to de-duplicate repeat failures.
pub fn ping_id(hook: &str, message: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hook.as_bytes());
    hasher.update(message.as_bytes());
    hex::encode(hasher.finalize())[..12].to_string()
}

fn format_ping(hook: &str, id: &str, message: &str, fix: Option<&str>) -> String {
    let mut s = format!("<ashkelon-ping hook=\"{hook}\" id=\"{id}\">\n{message}\n");
    if let Some(fix) = fix {
        s.push_str("fix: ");
        s.push_str(fix);
        s.push('\n');
    }
    s.push_str("</ashkelon-ping>");
    s
}

/// True when `text` is (or starts with) an ashkelon ping we previously inserted, so event
/// derivation never mistakes our own injected message for a new user prompt or tool result.
pub fn is_ping_text(text: &str) -> bool {
    text.trim_start().starts_with("<ashkelon-ping")
}

/// A stable fingerprint of a prompt's text, so a resubmitted-unchanged prompt is not re-fired.
pub fn fingerprint(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

pub fn into_pinned(ping: &Ping, anchor: usize) -> PinnedPing {
    PinnedPing {
        id: ping.id.clone(),
        anchor,
        text: ping.text.clone(),
    }
}
