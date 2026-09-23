use serde_json::Value;

use super::common::{extract_error_message, parse_event_json, str_field, u64_field, EventSink, StreamOrBody, TextTracker};
use super::{ResponseParser, Summary, ToolCall, Usage};

#[derive(Default)]
struct AnthropicSink {
    model: Option<String>,
    response_id: Option<String>,
    stop_reason: Option<String>,
    input_tokens: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_exact: Option<u64>,
    tool_calls: Vec<ToolCall>,
    error: Option<String>,
    text: TextTracker,
    thinking_chars: usize,
    output_chars: usize,
}

impl AnthropicSink {
    fn consume_usage(&mut self, usage: &Value) {
        if let Some(v) = u64_field(usage, "input_tokens") {
            self.input_tokens = Some(v);
        }
        if let Some(v) = u64_field(usage, "cache_read_input_tokens") {
            self.cache_read = Some(v);
        }
        if let Some(v) = u64_field(usage, "cache_creation_input_tokens") {
            self.cache_write = Some(v);
        }
        if let Some(v) = u64_field(usage, "output_tokens") {
            self.output_tokens = Some(v);
        }
        let thinking = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("thinking_tokens"))
            .and_then(|v| v.as_u64())
            .or_else(|| u64_field(usage, "thinking_tokens"));
        if let Some(t) = thinking {
            self.reasoning_exact = Some(t);
        }
    }

    fn consume_block_full(&mut self, block: &Value) {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    self.text.push(t);
                    self.output_chars += t.chars().count();
                }
            }
            Some("thinking") => {
                if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                    self.thinking_chars += t.chars().count();
                    self.output_chars += t.chars().count();
                }
            }
            Some("tool_use") => {
                let id = str_field(block, "id").unwrap_or_default();
                let name = str_field(block, "name").unwrap_or_default();
                self.tool_calls.push(ToolCall { id, name });
                if let Some(input) = block.get("input") {
                    self.output_chars += input.to_string().chars().count();
                }
            }
            _ => {}
        }
    }
}

impl EventSink for AnthropicSink {
    fn on_event(&mut self, event_type: Option<&str>, data: &str) {
        let Some(value) = parse_event_json(data) else { return };
        let ty = value.get("type").and_then(|v| v.as_str()).or(event_type).unwrap_or("");
        match ty {
            "message_start" => {
                if let Some(message) = value.get("message") {
                    if let Some(id) = str_field(message, "id") {
                        self.response_id = Some(id);
                    }
                    if let Some(m) = str_field(message, "model") {
                        self.model = Some(m);
                    }
                    if let Some(usage) = message.get("usage") {
                        self.consume_usage(usage);
                    }
                }
            }
            "content_block_start" => {
                if let Some(cb) = value.get("content_block") {
                    match cb.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(t) = cb.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    self.text.push(t);
                                    self.output_chars += t.chars().count();
                                }
                            }
                        }
                        Some("thinking") => {
                            if let Some(t) = cb.get("thinking").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    self.thinking_chars += t.chars().count();
                                    self.output_chars += t.chars().count();
                                }
                            }
                        }
                        Some("tool_use") => {
                            let id = str_field(cb, "id").unwrap_or_default();
                            let name = str_field(cb, "name").unwrap_or_default();
                            self.tool_calls.push(ToolCall { id, name });
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = value.get("delta") {
                    match delta.get("type").and_then(|v| v.as_str()) {
                        Some("text_delta") => {
                            if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                                self.text.push(t);
                                self.output_chars += t.chars().count();
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                                self.thinking_chars += t.chars().count();
                                self.output_chars += t.chars().count();
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(t) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                self.output_chars += t.chars().count();
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = value.get("delta") {
                    if let Some(sr) = str_field(delta, "stop_reason") {
                        self.stop_reason = Some(sr);
                    }
                }
                if let Some(usage) = value.get("usage") {
                    self.consume_usage(usage);
                }
            }
            "error" => {
                self.error = Some(value.get("error").map(extract_error_message).unwrap_or_else(|| "provider error".to_string()));
            }
            _ => {}
        }
    }

    fn on_body(&mut self, body: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(body) else { return };
        if value.get("type").and_then(|v| v.as_str()) == Some("error") {
            self.error = Some(value.get("error").map(extract_error_message).unwrap_or_else(|| "provider error".to_string()));
            return;
        }
        if let Some(id) = str_field(&value, "id") {
            self.response_id = Some(id);
        }
        if let Some(m) = str_field(&value, "model") {
            self.model = Some(m);
        }
        if let Some(sr) = str_field(&value, "stop_reason") {
            self.stop_reason = Some(sr);
        }
        if let Some(content) = value.get("content").and_then(|v| v.as_array()) {
            for block in content {
                self.consume_block_full(block);
            }
        }
        if let Some(usage) = value.get("usage") {
            self.consume_usage(usage);
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
        let turn_end = matches!(self.stop_reason.as_deref(), Some("end_turn") | Some("stop_sequence"));
        Summary {
            model: self.model,
            response_id: self.response_id,
            stop_reason: self.stop_reason,
            usage: Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read,
                cache_write_tokens: self.cache_write,
                reasoning_tokens,
                reasoning_estimated,
            },
            tool_calls: self.tool_calls,
            turn_end,
            text: self.text.full,
            error: self.error,
        }
    }
}

pub struct AnthropicParser {
    inner: StreamOrBody<AnthropicSink>,
}

impl AnthropicParser {
    pub fn new() -> Self {
        AnthropicParser { inner: StreamOrBody::new(AnthropicSink::default()) }
    }
}

impl ResponseParser for AnthropicParser {
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
