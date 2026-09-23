//! Light, best-effort JSON extraction over a request body, used only to decide which hook
//! events fired. Deliberately independent of `transform::conversation_len` / `inject_pings`
//! (the shared, harness-facing pinning mechanism): this module never touches the bytes sent
//! upstream, so it can stay simple and never needs to round-trip through the real wire shape.

use serde_json::Value;

use crate::wire::Wire;

use super::ping;

/// What the last conversation item told us, independent of each other: a message can carry a
/// new prompt, a tool result, both (Anthropic mixes content blocks freely), or neither.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub prompt: Option<String>,
    pub has_tool_result: bool,
}

fn items_array(wire: Wire, v: &Value) -> Option<&Vec<Value>> {
    let key = match wire {
        Wire::AnthropicMessages | Wire::OpenAiChat => "messages",
        Wire::OpenAiResponses => "input",
        Wire::Opaque => return None,
    };
    v.get(key)?.as_array()
}

/// Number of conversation items in the request, for compaction detection.
pub fn conversation_length(wire: Wire, body: &[u8]) -> Option<usize> {
    let v: Value = serde_json::from_slice(body).ok()?;
    items_array(wire, &v).map(Vec::len)
}

/// What the last conversation item amounts to, or `None` when there is nothing to derive
/// (empty conversation, unparseable body, or a wire this module does not understand).
pub fn last_item(wire: Wire, body: &[u8]) -> Option<Extracted> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let items = items_array(wire, &v)?;
    let last = items.last()?;
    match wire {
        Wire::AnthropicMessages => Some(extract_anthropic(last)),
        Wire::OpenAiResponses => Some(extract_openai_responses(last)),
        Wire::OpenAiChat => Some(extract_openai_chat(last)),
        Wire::Opaque => None,
    }
}

fn push_text(buf: &mut String, text: &str) {
    if ping::is_ping_text(text) || text.is_empty() {
        return;
    }
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(text);
}

fn extract_anthropic(msg: &Value) -> Extracted {
    if msg.get("role").and_then(Value::as_str) != Some("user") {
        return Extracted::default();
    }
    let mut out = Extracted::default();
    let mut prompt = String::new();
    match msg.get("content") {
        Some(Value::String(s)) => push_text(&mut prompt, s),
        Some(Value::Array(blocks)) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_result") => out.has_tool_result = true,
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            push_text(&mut prompt, t);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if !prompt.is_empty() {
        out.prompt = Some(prompt);
    }
    out
}

fn extract_openai_responses(item: &Value) -> Extracted {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("message");
    if item_type == "function_call_output" || item_type == "custom_tool_call_output" {
        return Extracted { prompt: None, has_tool_result: true };
    }
    if item_type != "message" || item.get("role").and_then(Value::as_str) != Some("user") {
        return Extracted::default();
    }
    let mut prompt = String::new();
    match item.get("content") {
        Some(Value::String(s)) => push_text(&mut prompt, s),
        Some(Value::Array(parts)) => {
            for p in parts {
                if let Some(t) = p.get("text").and_then(Value::as_str) {
                    push_text(&mut prompt, t);
                }
            }
        }
        _ => {}
    }
    Extracted { prompt: (!prompt.is_empty()).then_some(prompt), has_tool_result: false }
}

fn extract_openai_chat(msg: &Value) -> Extracted {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    if role == "tool" {
        return Extracted { prompt: None, has_tool_result: true };
    }
    if role != "user" {
        return Extracted::default();
    }
    let mut prompt = String::new();
    match msg.get("content") {
        Some(Value::String(s)) => push_text(&mut prompt, s),
        Some(Value::Array(parts)) => {
            for p in parts {
                if let Some(t) = p.get("text").and_then(Value::as_str) {
                    push_text(&mut prompt, t);
                }
            }
        }
        _ => {}
    }
    Extracted { prompt: (!prompt.is_empty()).then_some(prompt), has_tool_result: false }
}

