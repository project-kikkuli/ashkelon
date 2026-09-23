use serde_json::Value;

use super::sse::SseSplitter;
use super::Summary;

/// Bound a tail buffer to at most `max_chars` characters, trimming from the front.
fn bound_tail(buf: &mut String, max_chars: usize) {
    let len = buf.chars().count();
    if len > max_chars {
        let excess = len - max_chars;
        match buf.char_indices().nth(excess) {
            Some((idx, _)) => {
                buf.drain(..idx);
            }
            None => buf.clear(),
        }
    }
}

/// Tracks visible output text: the full accumulation (for `Summary::text`) and
/// a bounded tail of the last 4096 characters (for `output_text_tail`).
#[derive(Default)]
pub struct TextTracker {
    pub full: String,
    pub tail: String,
}

impl TextTracker {
    pub fn push(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.full.push_str(s);
        self.tail.push_str(s);
        bound_tail(&mut self.tail, 4096);
    }
}

pub fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

pub fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| v.as_u64())
}

/// Best-effort human-readable message out of a provider error object shaped
/// like `{"type": "...", "message": "..."}` (or any other shape — this never
/// fails, it just falls back to a generic string).
pub fn extract_error_message(error: &Value) -> String {
    str_field(error, "message")
        .or_else(|| str_field(error, "type"))
        .or_else(|| error.as_str().map(str::to_string))
        .unwrap_or_else(|| "provider error".to_string())
}

/// A per-wire accumulator fed either whole SSE events or one whole non-streamed
/// JSON body, producing a `Summary` when the response is done.
pub trait EventSink {
    /// `event_type` is the SSE `event:` field, if present; `data` is the joined
    /// `data:` lines' text, not yet parsed as JSON (may be `[DONE]` or garbage).
    fn on_event(&mut self, event_type: Option<&str>, data: &str);
    /// The whole body, for a non-streamed JSON response.
    fn on_body(&mut self, body: &[u8]);
    fn output_chars(&self) -> usize;
    fn output_text_tail(&self) -> &str;
    fn into_summary(self) -> Summary;
}

enum Mode {
    Undetermined,
    Sse,
    Json,
}

/// Detects, from the first non-whitespace byte fed, whether a response is a
/// single JSON body or an SSE stream, and routes bytes to the right handling
/// regardless of how they are chunked.
pub struct StreamOrBody<S> {
    mode: Mode,
    predetect: Vec<u8>,
    json_body: Vec<u8>,
    sse: SseSplitter,
    sink: S,
}

impl<S: EventSink> StreamOrBody<S> {
    pub fn new(sink: S) -> Self {
        StreamOrBody { mode: Mode::Undetermined, predetect: Vec::new(), json_body: Vec::new(), sse: SseSplitter::new(), sink }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if matches!(self.mode, Mode::Undetermined) {
            self.predetect.extend_from_slice(chunk);
            let Some(idx) = self.predetect.iter().position(|b| !b.is_ascii_whitespace()) else {
                return;
            };
            self.mode = if self.predetect[idx] == b'{' { Mode::Json } else { Mode::Sse };
            let buffered = std::mem::take(&mut self.predetect);
            self.route(&buffered);
            return;
        }
        self.route(chunk);
    }

    fn route(&mut self, bytes: &[u8]) {
        match self.mode {
            Mode::Json => self.json_body.extend_from_slice(bytes),
            Mode::Sse => {
                let sink = &mut self.sink;
                self.sse.feed(bytes, |et, data| sink.on_event(et, data));
            }
            Mode::Undetermined => unreachable!(),
        }
    }

    pub fn output_chars(&self) -> usize {
        self.sink.output_chars()
    }

    pub fn output_text_tail(&self) -> &str {
        self.sink.output_text_tail()
    }

    pub fn finish(mut self) -> S {
        match self.mode {
            Mode::Json => self.sink.on_body(&self.json_body),
            Mode::Sse => {
                let sink = &mut self.sink;
                self.sse.flush(|et, data| sink.on_event(et, data));
            }
            Mode::Undetermined => {}
        }
        self.sink
    }
}

/// Parses `data` as JSON unless it is the `[DONE]` sentinel or malformed, in
/// which case it is silently ignored (never a parse error, never a panic).
pub fn parse_event_json(data: &str) -> Option<Value> {
    if data.trim() == "[DONE]" {
        return None;
    }
    serde_json::from_str(data).ok()
}
