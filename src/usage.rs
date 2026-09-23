use serde::{Deserialize, Serialize};

use crate::wire::Wire;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    /// True when `reasoning_tokens` was estimated from visible reasoning text rather than reported.
    pub reasoning_estimated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
}

/// What one model response amounted to, parsed from its (possibly streamed) body.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub model: Option<String>,
    pub response_id: Option<String>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub tool_calls: Vec<ToolCall>,
    /// The model finished without asking for a tool: the agent's turn is over.
    pub turn_end: bool,
    /// Visible assistant text, for hooks. Never written to the call log.
    pub text: String,
    pub error: Option<String>,
}

/// Incremental parser over a response body (SSE or plain JSON). Must never fail on malformed input.
pub trait ResponseParser: Send {
    fn feed(&mut self, chunk: &[u8]);
    fn finish(self: Box<Self>) -> Summary;
    /// Visible output so far, in characters, for stream rules.
    fn output_chars(&self) -> usize;
    fn output_text_tail(&self) -> &str;
}

pub fn parser_for(_wire: Wire) -> Option<Box<dyn ResponseParser>> {
    None
}
