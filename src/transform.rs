use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde_json::Value;

use crate::config::{StripRule, ToolOutputTrim, TransformConfig};
use crate::wire::Wire;

pub struct Applied {
    pub body: Vec<u8>,
    /// Names of the transforms that changed something; empty means `body` is the original bytes.
    pub changed: Vec<String>,
}

/// Rewrites a request body. Thinking/reasoning blocks, their signatures, and encrypted reasoning are never touched.
///
/// Only re-serializes when something actually changed: an unparseable or untouched body comes back
/// byte-identical, and the rewrite of a changed body is a pure function of its input so repeated
/// requests keep producing the same prefix (needed for upstream prompt caching).
pub fn apply(wire: Wire, cfg: &TransformConfig, body: &[u8]) -> Applied {
    let Ok(mut root) = serde_json::from_slice::<Value>(body) else {
        return Applied { body: body.to_vec(), changed: Vec::new() };
    };

    let mut changed = Vec::new();

    if let Some(trim_cfg) = &cfg.tool_output {
        if apply_tool_output_trim(wire, trim_cfg, &mut root) {
            changed.push("tool_output".to_string());
        }
    }

    for rule in &cfg.strip {
        if apply_strip_rule(wire, rule, &mut root) {
            changed.push(rule.name.clone());
        }
    }

    if changed.is_empty() {
        Applied { body: body.to_vec(), changed }
    } else {
        let bytes = serde_json::to_vec(&root).unwrap_or_else(|_| body.to_vec());
        Applied { body: bytes, changed }
    }
}

fn warn_once(key: &str) -> bool {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WARNED.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap().insert(key.to_string())
}

fn apply_strip_rule(wire: Wire, rule: &StripRule, root: &mut Value) -> bool {
    match Regex::new(&rule.pattern) {
        Ok(re) => apply_strip(wire, &re, root),
        Err(e) => {
            if warn_once(&format!("transform:strip:{}:{}", rule.name, rule.pattern)) {
                tracing::warn!(
                    rule = %rule.name,
                    pattern = %rule.pattern,
                    error = %e,
                    "ashkelon: invalid strip regex, ignoring"
                );
            }
            false
        }
    }
}

/// Shortens a single string field in place. Returns true when it was over `cfg.max_chars` and got cut.
fn trim_string_field(cfg: &ToolOutputTrim, obj: &mut Value, field: &str) -> bool {
    let Some(Value::String(s)) = obj.get_mut(field) else { return false };
    match trim_text(cfg, s) {
        Some(new_text) => {
            *s = new_text;
            true
        }
        None => false,
    }
}

/// Trims `text` when it is longer than `max_chars`, keeping the head and tail and marking the cut.
/// Operates on chars, so it is always UTF-8 boundary safe. Pure in `(cfg, text)`, so it is deterministic.
fn trim_text(cfg: &ToolOutputTrim, text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len <= cfg.max_chars {
        return None;
    }
    let head_n = cfg.keep_head.min(len);
    let tail_n = cfg.keep_tail.min(len - head_n);
    let removed = len - head_n - tail_n;
    let head: String = chars[..head_n].iter().collect();
    let tail: String = chars[len - tail_n..].iter().collect();
    Some(format!("{head}\n[ashkelon: trimmed {removed} chars]\n{tail}"))
}

/// Trims a message/block's "content", whether it is a plain string or an array of `{"type":"text","text":...}`
/// parts (as tool-role Chat messages and Anthropic tool_result blocks can both be shaped).
fn trim_text_bearing_content(cfg: &ToolOutputTrim, obj: &mut Value) -> bool {
    match obj.get_mut("content") {
        Some(Value::String(_)) => trim_string_field(cfg, obj, "content"),
        Some(Value::Array(parts)) => {
            let mut changed = false;
            for part in parts.iter_mut() {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    changed |= trim_string_field(cfg, part, "text");
                }
            }
            changed
        }
        _ => false,
    }
}

fn apply_tool_output_trim(wire: Wire, cfg: &ToolOutputTrim, root: &mut Value) -> bool {
    let mut changed = false;
    match wire {
        Wire::AnthropicMessages => {
            if let Some(messages) = root.get_mut("messages").and_then(Value::as_array_mut) {
                for message in messages {
                    if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                        for block in content.iter_mut() {
                            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                                changed |= trim_text_bearing_content(cfg, block);
                            }
                        }
                    }
                }
            }
        }
        Wire::OpenAiResponses => {
            if let Some(items) = root.get_mut("input").and_then(Value::as_array_mut) {
                for item in items {
                    let ty = item.get("type").and_then(Value::as_str);
                    if ty == Some("function_call_output") || ty == Some("custom_tool_call_output") {
                        changed |= trim_string_field(cfg, item, "output");
                    }
                }
            }
        }
        Wire::OpenAiChat => {
            if let Some(messages) = root.get_mut("messages").and_then(Value::as_array_mut) {
                for message in messages {
                    if message.get("role").and_then(Value::as_str) == Some("tool") {
                        changed |= trim_text_bearing_content(cfg, message);
                    }
                }
            }
        }
        Wire::Opaque => {}
    }
    changed
}

/// Strips regex matches out of a user message's text content, dropping a block that becomes empty.
/// A plain string content has no block to drop, so it is just replaced (possibly with an empty string).
fn strip_user_content(re: &Regex, message: &mut Value) -> bool {
    match message.get_mut("content") {
        Some(Value::String(text)) => {
            let stripped = re.replace_all(text, "").into_owned();
            let changed = stripped != *text;
            *text = stripped;
            changed
        }
        Some(Value::Array(blocks)) => {
            let mut changed = false;
            blocks.retain_mut(|block| {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return true;
                }
                let Some(Value::String(text)) = block.get_mut("text") else { return true };
                let stripped = re.replace_all(text, "").into_owned();
                if stripped != *text {
                    changed = true;
                }
                let keep = !stripped.is_empty();
                *text = stripped;
                keep
            });
            changed
        }
        _ => false,
    }
}

fn apply_strip(wire: Wire, re: &Regex, root: &mut Value) -> bool {
    let mut changed = false;
    match wire {
        Wire::AnthropicMessages | Wire::OpenAiChat => {
            if let Some(messages) = root.get_mut("messages").and_then(Value::as_array_mut) {
                for message in messages {
                    if message.get("role").and_then(Value::as_str) != Some("user") {
                        continue;
                    }
                    changed |= strip_user_content(re, message);
                }
            }
        }
        Wire::OpenAiResponses => {
            if let Some(items) = root.get_mut("input").and_then(Value::as_array_mut) {
                for item in items {
                    let is_user_message = item.get("type").and_then(Value::as_str) == Some("message")
                        && item.get("role").and_then(Value::as_str) == Some("user");
                    if !is_user_message {
                        continue;
                    }
                    if let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) {
                        content.retain_mut(|part| {
                            if part.get("type").and_then(Value::as_str) != Some("input_text") {
                                return true;
                            }
                            let Some(Value::String(text)) = part.get_mut("text") else { return true };
                            let stripped = re.replace_all(text, "").into_owned();
                            if stripped != *text {
                                changed = true;
                            }
                            let keep = !stripped.is_empty();
                            *text = stripped;
                            keep
                        });
                    }
                }
            }
        }
        Wire::Opaque => {}
    }
    changed
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
pub fn inject_pings(wire: Wire, body: &[u8], pings: &[PinnedPing]) -> Option<Vec<u8>> {
    if pings.is_empty() {
        return Some(body.to_vec());
    }
    let mut root: Value = serde_json::from_slice(body).ok()?;
    match wire {
        Wire::AnthropicMessages => inject_anthropic(&mut root, pings)?,
        Wire::OpenAiResponses => inject_responses(&mut root, pings)?,
        Wire::OpenAiChat => inject_chat(&mut root, pings)?,
        Wire::Opaque => return None,
    }
    serde_json::to_vec(&root).ok()
}

/// Appends each ping's text as a new text block on the existing user message at its anchor. Every anchor
/// must exist and name a user message before any of them are applied, so a body is never partially rewritten.
fn inject_anthropic(root: &mut Value, pings: &[PinnedPing]) -> Option<()> {
    let messages = root.get_mut("messages")?.as_array_mut()?;
    for ping in pings {
        let msg = messages.get(ping.anchor)?;
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
    }
    for ping in pings {
        let msg = messages.get_mut(ping.anchor)?;
        let content = msg.get_mut("content")?;
        if let Value::String(s) = content {
            *content = Value::Array(vec![serde_json::json!({"type": "text", "text": s})]);
        }
        content.as_array_mut()?.push(serde_json::json!({"type": "text", "text": ping.text}));
    }
    Some(())
}

/// Inserts a new item right after each ping's anchor. Anchors always name a position in the ORIGINAL
/// array (the position the ping was first pinned at), never a position shifted by an earlier insertion
/// in this same call, so pings on different anchors can be applied independently of each other's order.
fn insert_after_original_indices(
    items: &mut Vec<Value>,
    pings: &[PinnedPing],
    make_item: impl Fn(&str) -> Value,
) -> Option<()> {
    let len = items.len();
    if pings.iter().any(|p| p.anchor >= len) {
        return None;
    }
    let original = std::mem::take(items);
    let mut rebuilt = Vec::with_capacity(original.len() + pings.len());
    for (i, item) in original.into_iter().enumerate() {
        rebuilt.push(item);
        for ping in pings.iter().filter(|p| p.anchor == i) {
            rebuilt.push(make_item(&ping.text));
        }
    }
    *items = rebuilt;
    Some(())
}

fn inject_responses(root: &mut Value, pings: &[PinnedPing]) -> Option<()> {
    let input = root.get_mut("input")?;
    if let Value::String(s) = input {
        *input = Value::Array(vec![Value::String(std::mem::take(s))]);
    }
    let items = root.get_mut("input")?.as_array_mut()?;
    insert_after_original_indices(items, pings, |text| {
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })
    })
}

fn inject_chat(root: &mut Value, pings: &[PinnedPing]) -> Option<()> {
    let messages = root.get_mut("messages")?.as_array_mut()?;
    insert_after_original_indices(messages, pings, |text| {
        serde_json::json!({"role": "user", "content": text})
    })
}

/// Number of conversation items in a request, used as the anchor for a newly delivered ping.
pub fn conversation_len(wire: Wire, body: &[u8]) -> Option<usize> {
    let root: Value = serde_json::from_slice(body).ok()?;
    match wire {
        Wire::AnthropicMessages | Wire::OpenAiChat => root.get("messages")?.as_array().map(Vec::len),
        Wire::OpenAiResponses => match root.get("input")? {
            Value::String(_) => Some(1),
            Value::Array(items) => Some(items.len()),
            _ => None,
        },
        Wire::Opaque => None,
    }
}
