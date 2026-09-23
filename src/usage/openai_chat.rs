use std::collections::BTreeMap;

use serde_json::Value;

use super::common::{extract_error_message, parse_event_json, str_field, u64_field, EventSink, StreamOrBody, TextTracker};
use super::{ResponseParser, Summary, ToolCall, Usage};

#[derive(Default)]
struct ChatSink {
    model: Option<String>,
    response_id: Option<String>,
    finish_reason: Option<String>,
    prompt_tokens: Option<u64>,
    cache_read: Option<u64>,
    completion_tokens: Option<u64>,
    reasoning_exact: Option<u64>,
    /// Assembled by tool_call array index, since chunks stream id/name/arguments
    /// separately; only id and name are kept.
    tool_call_builders: BTreeMap<u64, (Option<String>, Option<String>)>,
    error: Option<String>,
    text: TextTracker,
    thinking_chars: usize,
    output_chars: usize,
}

impl ChatSink {
    fn consume_usage(&mut self, usage: &Value) {
        if let Some(v) = u64_field(usage, "prompt_tokens") {
            self.prompt_tokens = Some(v);
        }
        if let Some(v) = usage.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")).and_then(|v| v.as_u64()) {
            self.cache_read = Some(v);
        }
        if let Some(v) = u64_field(usage, "completion_tokens") {
            self.completion_tokens = Some(v);
        }
        if let Some(v) = usage.get("completion_tokens_details").and_then(|d| d.get("reasoning_tokens")).and_then(|v| v.as_u64()) {
            self.reasoning_exact = Some(v);
        }
    }

    fn consume_tool_call_delta(&mut self, tc: &Value) {
        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
        let entry = self.tool_call_builders.entry(idx).or_insert((None, None));
        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
            entry.0 = Some(id.to_string());
        }
        if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) {
            entry.1 = Some(name.to_string());
        }
    }

    fn consume_full_tool_call(&mut self, idx: u64, tc: &Value) {
        let id = str_field(tc, "id");
        let name = tc.get("function").and_then(|f| str_field(f, "name"));
        self.tool_call_builders.insert(idx, (id, name));
    }
}

impl EventSink for ChatSink {
    fn on_event(&mut self, event_type: Option<&str>, data: &str) {
        let _ = event_type;
        let Some(value) = parse_event_json(data) else { return };
        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
            self.error = Some(extract_error_message(err));
            return;
        }
        if let Some(id) = str_field(&value, "id") {
            self.response_id = Some(id);
        }
        if let Some(m) = str_field(&value, "model") {
            self.model = Some(m);
        }
        if let Some(usage) = value.get("usage") {
            if !usage.is_null() {
                self.consume_usage(usage);
            }
        }
        let Some(choice0) = value.get("choices").and_then(|v| v.as_array()).and_then(|c| c.first()) else { return };
        if let Some(fr) = choice0.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(fr.to_string());
        }
        let Some(delta) = choice0.get("delta") else { return };
        if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
            self.text.push(t);
            self.output_chars += t.chars().count();
        }
        if let Some(t) = delta.get("reasoning").and_then(|v| v.as_str()) {
            self.thinking_chars += t.chars().count();
            self.output_chars += t.chars().count();
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                self.consume_tool_call_delta(tc);
            }
        }
    }

    fn on_body(&mut self, body: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(body) else { return };
        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
            self.error = Some(extract_error_message(err));
            return;
        }
        if let Some(id) = str_field(&value, "id") {
            self.response_id = Some(id);
        }
        if let Some(m) = str_field(&value, "model") {
            self.model = Some(m);
        }
        if let Some(usage) = value.get("usage") {
            self.consume_usage(usage);
        }
        let Some(choice0) = value.get("choices").and_then(|v| v.as_array()).and_then(|c| c.first()) else { return };
        if let Some(fr) = choice0.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(fr.to_string());
        }
        let Some(message) = choice0.get("message") else { return };
        if let Some(t) = message.get("content").and_then(|v| v.as_str()) {
            self.text.push(t);
            self.output_chars += t.chars().count();
        }
        if let Some(t) = message.get("reasoning").and_then(|v| v.as_str()) {
            self.thinking_chars += t.chars().count();
            self.output_chars += t.chars().count();
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for (i, tc) in tool_calls.iter().enumerate() {
                self.consume_full_tool_call(i as u64, tc);
            }
        }
    }

    fn output_chars(&self) -> usize {
        self.output_chars
    }

    fn output_text_tail(&self) -> &str {
        &self.text.tail
    }

    fn into_summary(self) -> Summary {
        let (reasoning_tokens, reasoning_estimated) = match self.reasoning_exact {
            Some(t) => (Some(t), false),
            None if self.thinking_chars > 0 => (Some((self.thinking_chars / 4) as u64), true),
            None => (None, false),
        };
        let turn_end = self.finish_reason.as_deref() == Some("stop");
        let tool_calls = self
            .tool_call_builders
            .into_values()
            .filter(|(id, name)| id.is_some() || name.is_some())
            .map(|(id, name)| ToolCall { id: id.unwrap_or_default(), name: name.unwrap_or_default() })
            .collect();
        Summary {
            model: self.model,
            response_id: self.response_id,
            stop_reason: self.finish_reason,
            usage: Usage {
                input_tokens: self.prompt_tokens,
                output_tokens: self.completion_tokens,
                cache_read_tokens: self.cache_read,
                cache_write_tokens: None,
                reasoning_tokens,
                reasoning_estimated,
            },
            tool_calls,
            turn_end,
            text: self.text.full,
            error: self.error,
        }
    }
}

pub struct ChatParser {
    inner: StreamOrBody<ChatSink>,
}

impl ChatParser {
    pub fn new() -> Self {
        ChatParser { inner: StreamOrBody::new(ChatSink::default()) }
    }
}

impl ResponseParser for ChatParser {
    fn feed(&mut self, chunk: &[u8]) {
        self.inner.feed(chunk);
    }

    fn finish(self: Box<Self>) -> Summary {
        let this = *self;
        this.inner.finish().into_summary()
    }

    fn output_chars(&self) -> usize {
        self.inner.output_chars()
    }

    fn output_text_tail(&self) -> &str {
        self.inner.output_text_tail()
    }
}
