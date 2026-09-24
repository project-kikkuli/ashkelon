//! Best-effort cwd extraction from a request body, for serve-mode sessions that have no
//! registered launch info (`run` always knows the exact cwd it launched the harness from — see
//! `Engine::register_launch`).
//!
//! Each harness injects its own working-directory context into the conversation somewhere; the
//! exact shapes below are pinned from real captured request bodies (`ashkelon serve
//! --log-bodies`, a real `claude -p`/`codex exec`/`opencode run` pointed at it), not guessed:
//!
//! - Claude Code puts `<system-reminder># Environment ... - Primary working directory: <path>`
//!   in the first user message's text content.
//! - Codex puts `<environment_context><cwd><path></cwd>...` in a later user message's text
//!   content.
//! - opencode puts `<env>\n  Working directory: <path>` in the system prompt text.
//!
//! All three are searched for regardless of which field the wire happens to put them in, so a
//! harness that moves its own context block between system and message text still matches.

use std::path::PathBuf;

use serde_json::Value;

use crate::wire::Wire;

/// Extracts the working directory a harness told the model about, if any. `None` when the body
/// doesn't parse, carries no such block, or the wire type never carries one (`Opaque`).
pub fn derive_cwd(wire: Wire, body: &[u8]) -> Option<PathBuf> {
    if wire == Wire::Opaque {
        return None;
    }
    let json: Value = serde_json::from_slice(body).ok()?;
    let mut texts = Vec::new();
    collect_text(json.get("system"), &mut texts);
    if let Some(messages) = json.get("messages").and_then(Value::as_array) {
        for m in messages {
            collect_text(m.get("content"), &mut texts);
        }
    }
    if let Some(input) = json.get("input").and_then(Value::as_array) {
        for item in input {
            collect_text(item.get("content"), &mut texts);
        }
    }
    texts.iter().find_map(|t| extract_path(t))
}

fn collect_text(value: Option<&Value>, out: &mut Vec<String>) {
    match value {
        Some(Value::String(s)) => out.push(s.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    out.push(t.to_string());
                } else if item.is_string() {
                    if let Some(s) = item.as_str() {
                        out.push(s.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Tries every known harness pattern against one text block, in order, returning the first hit.
fn extract_path(text: &str) -> Option<PathBuf> {
    if let Some(rest) = find_after(text, "Primary working directory:") {
        return Some(PathBuf::from(rest));
    }
    if let Some(rest) = find_after(text, "Working directory:") {
        return Some(PathBuf::from(rest));
    }
    if let Some(start) = text.find("<cwd>") {
        let rest = &text[start + "<cwd>".len()..];
        if let Some(end) = rest.find("</cwd>") {
            return Some(PathBuf::from(rest[..end].trim()));
        }
    }
    None
}

/// Returns the trimmed rest of the line following `label`, or `None` if `label` doesn't occur.
fn find_after<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    let idx = text.find(label)?;
    let rest = &text[idx + label.len()..];
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let value = rest[..line_end].trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_claude_code_cwd_from_first_user_message() {
        let body = br#"{"system":[{"type":"text","text":"be terse"}],"messages":[{"role":"user","content":[{"type":"text","text":"<system-reminder>\n# Environment\nYou have been invoked in the following environment: \n - Primary working directory: /Users/x/proj\n - Is a git repository: true\n</system-reminder>"}]}]}"#;
        assert_eq!(
            derive_cwd(Wire::AnthropicMessages, body),
            Some(PathBuf::from("/Users/x/proj"))
        );
    }

    #[test]
    fn extracts_codex_cwd_from_environment_context() {
        let body = br#"{"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/private/tmp/proj</cwd>\n  <shell>zsh</shell>\n</environment_context>"}]}]}"#;
        assert_eq!(
            derive_cwd(Wire::OpenAiResponses, body),
            Some(PathBuf::from("/private/tmp/proj"))
        );
    }

    #[test]
    fn extracts_opencode_cwd_from_system_prompt() {
        let body = br#"{"system":[{"type":"text","text":"Here is useful information about the environment you are running in:\n<env>\n  Working directory: /private/tmp/proj\n  Workspace root folder: /\n</env>"}],"messages":[]}"#;
        assert_eq!(
            derive_cwd(Wire::AnthropicMessages, body),
            Some(PathBuf::from("/private/tmp/proj"))
        );
    }

    #[test]
    fn returns_none_when_no_pattern_matches() {
        let body = br#"{"system":"nothing here","messages":[{"role":"user","content":"hello"}]}"#;
        assert_eq!(derive_cwd(Wire::AnthropicMessages, body), None);
    }

    #[test]
    fn opaque_wire_never_parsed() {
        assert_eq!(derive_cwd(Wire::Opaque, b"whatever"), None);
    }
}
