use serde_json::Value;

use super::common::{
    extract_error_message, parse_event_json, str_field, u64_field, EventSink, StreamOrBody, TextTracker,
};
use super::{ResponseParser, Summary, ToolCall, Usage};

/// Item types the platform surfaces as client tool calls. `web_search_call` and
/// anything else (e.g. `message`, `reasoning`, `file_search_call`) are excluded.
fn is_client_tool_call(item_type: &str) -> bool {
    matches!(item_type, "function_call" | "custom_tool_call" | "local_shell_call")
}

#[derive(Default)]
struct ResponsesSink {
    model: Option<String>,
    response_id: Option<String>,
    status: Option<String>,
    incomplete_reason: Option<String>,
    input_tokens: Option<u64>,
    cache_read: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_exact: Option<u64>,
    tool_calls: Vec<ToolCall>,
    error: Option<String>,
    text: TextTracker,
    output_chars: usize,
}

impl ResponsesSink {
    fn take_model_id(&mut self, resp: &Value) {
        if let Some(id) = str_field(resp, "id") {
            self.response_id = Some(id);
        }
        if let Some(m) = str_field(resp, "model") {
            self.model = Some(m);
        }
    }

    fn consume_usage(&mut self, usage: &Value) {
        if let Some(v) = u64_field(usage, "input_tokens") {
            self.input_tokens = Some(v);
        }
        if let Some(v) = usage
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
        {
            self.cache_read = Some(v);
        }
        if let Some(v) = u64_field(usage, "output_tokens") {
            self.output_tokens = Some(v);
        }
        if let Some(v) = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
        {
            self.reasoning_exact = Some(v);
        }
    }

    /// `harvest_text` is only set for a non-streamed body, where no
    /// `response.output_text.delta` events ran to build the text incrementally.
    fn consume_output_item(&mut self, item: &Value, harvest_text: bool) {
        match item.get("type").and_then(|v| v.as_str()) {
            Some(t) if is_client_tool_call(t) => {
                let id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("id").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                let name = str_field(item, "name").unwrap_or_default();
                self.tool_calls.push(ToolCall { id, name });
            }
            Some("message") if harvest_text => {
                if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                    for c in content {
                        if c.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                self.text.push(t);
                                self.output_chars += t.chars().count();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn consume_terminal_response(&mut self, resp: &Value) {
        self.take_model_id(resp);
        if let Some(s) = str_field(resp, "status") {
            self.status = Some(s);
        }
        if let Some(r) = resp.get("incomplete_details").and_then(|d| str_field(d, "reason")) {
            self.incomplete_reason = Some(r);
        }
        if let Some(usage) = resp.get("usage") {
            self.consume_usage(usage);
        }
        if let Some(output) = resp.get("output").and_then(|v| v.as_array()) {
            for item in output {
                self.consume_output_item(item, false);
            }
        }
        if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
            self.error = Some(extract_error_message(err));
        }
    }
}

impl EventSink for ResponsesSink {
    fn on_event(&mut self, event_type: Option<&str>, data: &str) {
        let Some(value) = parse_event_json(data) else { return };
        let ty = value.get("type").and_then(|v| v.as_str()).or(event_type).unwrap_or("");
        match ty {
            "response.created" | "response.in_progress" => {
                if let Some(resp) = value.get("response") {
                    self.take_model_id(resp);
                    if let Some(s) = str_field(resp, "status") {
                        self.status = Some(s);
                    }
                }
            }
            "response.output_text.delta" => {
                if let Some(t) = value.get("delta").and_then(|v| v.as_str()) {
                    self.text.push(t);
                    self.output_chars += t.chars().count();
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                if let Some(resp) = value.get("response") {
                    self.consume_terminal_response(resp);
                }
                if ty == "response.failed" {
                    if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
                        self.error = Some(extract_error_message(err));
                    } else if self.error.is_none() {
                        self.error = Some("response failed".to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn on_body(&mut self, body: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return;
        };
        self.take_model_id(&value);
        if let Some(s) = str_field(&value, "status") {
            self.status = Some(s);
        }
        if let Some(r) = value.get("incomplete_details").and_then(|d| str_field(d, "reason")) {
            self.incomplete_reason = Some(r);
        }
        if let Some(usage) = value.get("usage") {
            self.consume_usage(usage);
        }
        if let Some(output) = value.get("output").and_then(|v| v.as_array()) {
            for item in output {
                self.consume_output_item(item, true);
            }
        }
        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
            self.error = Some(extract_error_message(err));
        }
    }

    fn output_chars(&self) -> usize {
        self.output_chars
    }

    fn output_text_tail(&self) -> &str {
        &self.text.tail
    }

    fn into_summary(self) -> Summary {
        let stop_reason = match self.status.as_deref() {
            Some("incomplete") => Some(match &self.incomplete_reason {
                Some(r) => format!("incomplete:{r}"),
                None => "incomplete".to_string(),
            }),
            Some(other) => Some(other.to_string()),
            None => None,
        };
        let turn_end = self.status.as_deref() == Some("completed") && self.tool_calls.is_empty();
        Summary {
            model: self.model,
            response_id: self.response_id,
            stop_reason,
            usage: Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read,
                cache_write_tokens: None,
                reasoning_tokens: self.reasoning_exact,
                reasoning_estimated: false,
            },
            tool_calls: self.tool_calls,
            turn_end,
            text: self.text.full,
            error: self.error,
        }
    }
}

pub struct ResponsesParser {
    inner: StreamOrBody<ResponsesSink>,
}

impl ResponsesParser {
    pub fn new() -> Self {
        ResponsesParser {
            inner: StreamOrBody::new(ResponsesSink::default()),
        }
    }
}

impl ResponseParser for ResponsesParser {
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
