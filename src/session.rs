use http::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::wire::Wire;

/// Identifies one agent conversation across calls.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    /// Set when the call came through `ashkelon run`, from the `/s/<launch>/` path prefix.
    pub launch: Option<String>,
    pub harness: Option<String>,
    /// The harness's own session id when it sends one, else a fingerprint of the conversation's opening.
    pub session: String,
}

const SESSION_HEADERS: &[&str] =
    &["session_id", "x-session-id", "conversation_id", "x-claude-code-session-id"];

/// Identifies one agent conversation from a request. `body` is the decoded (uncompressed) body.
pub fn derive(launch: Option<&str>, wire: Wire, headers: &HeaderMap, body: &[u8]) -> SessionKey {
    SessionKey {
        launch: launch.map(str::to_string),
        harness: harness_from_user_agent(headers),
        session: session_id(wire, headers, body),
    }
}

fn harness_from_user_agent(headers: &HeaderMap) -> Option<String> {
    let ua = headers.get(http::header::USER_AGENT)?.to_str().ok()?.to_ascii_lowercase();
    if ua.contains("claude-cli") {
        Some("claude".to_string())
    } else if ua.contains("codex") {
        Some("codex".to_string())
    } else if ua.contains("opencode") {
        Some("opencode".to_string())
    } else if ua.contains("omp") || ua.contains("/pi") || ua.starts_with("pi/") {
        Some("omp".to_string())
    } else if ua.contains("hermes") {
        Some("hermes".to_string())
    } else {
        None
    }
}

fn session_id(wire: Wire, headers: &HeaderMap, body: &[u8]) -> String {
    if wire == Wire::AnthropicMessages {
        if let Some(id) = anthropic_metadata_session(body) {
            return id;
        }
    }
    for name in SESSION_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    if wire == Wire::OpenAiResponses {
        if let Some(key) = responses_prompt_cache_key(body) {
            return key;
        }
    }
    fingerprint(wire, body)
}

fn anthropic_metadata_session(body: &[u8]) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let user_id = json.get("metadata")?.get("user_id")?.as_str()?;
    // Newer clients encode user_id as a JSON object; older ones as `user_..._session_<id>`.
    if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(user_id) {
        return obj.get("session_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    }
    let idx = user_id.find("session_")?;
    let rest = &user_id[idx + "session_".len()..];
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn responses_prompt_cache_key(body: &[u8]) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    json.get("prompt_cache_key")?.as_str().map(str::to_string)
}

/// First 16 hex chars of SHA-256 over the system/instructions text plus the first user message text.
fn fingerprint(wire: Wire, body: &[u8]) -> String {
    let (system, first_user) = extract_opening(wire, body);
    let mut hasher = Sha256::new();
    hasher.update(system.as_bytes());
    hasher.update(b"\0");
    hasher.update(first_user.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)[..16].to_string()
}

fn extract_opening(wire: Wire, body: &[u8]) -> (String, String) {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (String::new(), String::new());
    };
    match wire {
        Wire::AnthropicMessages => {
            let system = value_text(json.get("system"));
            let first_user = json
                .get("messages")
                .and_then(|m| m.as_array())
                .and_then(|arr| arr.iter().find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")))
                .map(|m| value_text(m.get("content")))
                .unwrap_or_default();
            (system, first_user)
        }
        Wire::OpenAiResponses => {
            let system = value_text(json.get("instructions"));
            let first_user = match json.get("input") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .find(|item| item.get("role").and_then(|r| r.as_str()) == Some("user"))
                    .map(|item| value_text(item.get("content")))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            (system, first_user)
        }
        Wire::OpenAiChat => {
            let messages = json.get("messages").and_then(|m| m.as_array());
            let system = messages
                .and_then(|arr| arr.iter().find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system")))
                .map(|m| value_text(m.get("content")))
                .unwrap_or_default();
            let first_user = messages
                .and_then(|arr| arr.iter().find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")))
                .map(|m| value_text(m.get("content")))
                .unwrap_or_default();
            (system, first_user)
        }
        Wire::Opaque => (String::new(), String::new()),
    }
}

/// Flattens a message `content` field: a plain string, or an array of `{type, text}` blocks.
fn value_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> HeaderMap {
        HeaderMap::new()
    }

    #[test]
    fn harness_detected_from_user_agent() {
        let mut h = headers();
        h.insert(http::header::USER_AGENT, "claude-cli/1.0".parse().unwrap());
        assert_eq!(harness_from_user_agent(&h), Some("claude".to_string()));

        let mut h = headers();
        h.insert(http::header::USER_AGENT, "codex-cli/0.9".parse().unwrap());
        assert_eq!(harness_from_user_agent(&h), Some("codex".to_string()));

        let mut h = headers();
        h.insert(http::header::USER_AGENT, "some-other-tool/1.0".parse().unwrap());
        assert_eq!(harness_from_user_agent(&h), None);
    }

    #[test]
    fn session_id_from_anthropic_metadata() {
        let body = br#"{"metadata":{"user_id":"user_abc-session_xyz123"},"messages":[]}"#;
        let key = derive(None, Wire::AnthropicMessages, &headers(), body);
        assert_eq!(key.session, "xyz123");
    }

    #[test]
    fn session_id_from_json_encoded_anthropic_metadata() {
        let body = br#"{"metadata":{"user_id":"{\"device_id\":\"d1\",\"session_id\":\"0b7c-44\"}"},"messages":[]}"#;
        assert_eq!(session_id(Wire::AnthropicMessages, &HeaderMap::new(), body), "0b7c-44");
    }

    #[test]
    fn session_id_from_header_takes_priority_over_fallback() {
        let mut h = headers();
        h.insert("x-session-id", "abc-123".parse().unwrap());
        let key = derive(None, Wire::OpenAiChat, &h, b"{}");
        assert_eq!(key.session, "abc-123");
    }

    #[test]
    fn session_id_from_responses_prompt_cache_key() {
        let body = br#"{"prompt_cache_key":"cache-key-1"}"#;
        let key = derive(None, Wire::OpenAiResponses, &headers(), body);
        assert_eq!(key.session, "cache-key-1");
    }

    #[test]
    fn session_id_falls_back_to_fingerprint_and_is_stable() {
        let body = br#"{"system":"be terse","messages":[{"role":"user","content":"hello"}]}"#;
        let a = derive(None, Wire::AnthropicMessages, &headers(), body);
        let b = derive(None, Wire::AnthropicMessages, &headers(), body);
        assert_eq!(a.session, b.session);
        assert_eq!(a.session.len(), 16);

        let other = br#"{"system":"be terse","messages":[{"role":"user","content":"goodbye"}]}"#;
        let c = derive(None, Wire::AnthropicMessages, &headers(), other);
        assert_ne!(a.session, c.session);
    }
}
