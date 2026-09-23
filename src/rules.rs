use crate::config::RuleConfig;
use crate::usage::ResponseParser;
use crate::wire::Wire;

pub enum PreDecision {
    Allow,
    Reject { rule: String, message: String },
}

pub fn check_request(_wire: Wire, _cfg: &RuleConfig, _body: &[u8]) -> PreDecision {
    PreDecision::Allow
}

/// Status and body of a rejection, shaped like the provider's own error so the harness reports it cleanly.
pub fn reject_response(_wire: Wire, rule: &str, message: &str) -> (u16, Vec<u8>) {
    (400, serde_json::json!({"error": {"type": "ashkelon_rule", "rule": rule, "message": message}}).to_string().into_bytes())
}

/// Watches a streaming response and names the rule to cut it with, if any.
pub struct StreamGuard;

impl StreamGuard {
    pub fn new(_cfg: &RuleConfig) -> StreamGuard {
        StreamGuard
    }

    pub fn check(&mut self, _parser: &dyn ResponseParser) -> Option<String> {
        None
    }
}

/// Bytes that end a cut SSE stream the way the provider ends an errored one.
pub fn cut_tail(_wire: Wire, _rule: &str) -> Vec<u8> {
    Vec::new()
}
