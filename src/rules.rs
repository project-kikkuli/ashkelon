use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde_json::Value;

use crate::config::{CutPattern, RuleConfig};
use crate::usage::ResponseParser;
use crate::wire::Wire;

pub enum PreDecision {
    Allow,
    Reject { rule: String, message: String },
}

fn warn_once(key: &str) -> bool {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WARNED.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap().insert(key.to_string())
}

fn compile_patterns(patterns: &[String], context: &str) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| match Regex::new(p) {
            Ok(re) => Some(re),
            Err(e) => {
                if warn_once(&format!("rules:{context}:{p}")) {
                    tracing::warn!(pattern = %p, error = %e, "ashkelon: invalid {context} regex, ignoring");
                }
                None
            }
        })
        .collect()
}

fn compile_cut_patterns(patterns: &[CutPattern]) -> Vec<(String, Regex)> {
    patterns
        .iter()
        .filter_map(|c| match Regex::new(&c.pattern) {
            Ok(re) => Some((c.name.clone(), re)),
            Err(e) => {
                if warn_once(&format!("rules:cut_patterns:{}:{}", c.name, c.pattern)) {
                    tracing::warn!(
                        rule = %c.name,
                        pattern = %c.pattern,
                        error = %e,
                        "ashkelon: invalid cut_patterns regex, ignoring"
                    );
                }
                None
            }
        })
        .collect()
}

/// The declared max-output-tokens field, whichever name this wire's body happens to use.
const MAX_OUTPUT_TOKEN_FIELDS: [&str; 3] = ["max_tokens", "max_output_tokens", "max_completion_tokens"];

pub fn check_request(_wire: Wire, cfg: &RuleConfig, body: &[u8]) -> PreDecision {
    let Ok(root) = serde_json::from_slice::<Value>(body) else {
        return PreDecision::Allow;
    };

    if !cfg.allow_models.is_empty() {
        let patterns = compile_patterns(&cfg.allow_models, "allow_models");
        let model = root.get("model").and_then(Value::as_str);
        let allowed = model.is_some_and(|m| patterns.iter().any(|re| re.is_match(m)));
        if !allowed {
            return PreDecision::Reject {
                rule: "allow_models".to_string(),
                message: format!("model {} is not in allow_models", model.unwrap_or("<none>")),
            };
        }
    }

    if let Some(cap) = cfg.max_output_tokens {
        for field in MAX_OUTPUT_TOKEN_FIELDS {
            if let Some(n) = root.get(field).and_then(Value::as_u64) {
                if n > cap {
                    return PreDecision::Reject {
                        rule: "max_output_tokens".to_string(),
                        message: format!("{field}={n} exceeds cap of {cap}"),
                    };
                }
            }
        }
    }

    PreDecision::Allow
}

/// Status and body of a rejection, shaped like the provider's own error so the harness reports it cleanly.
pub fn reject_response(wire: Wire, rule: &str, message: &str) -> (u16, Vec<u8>) {
    let full_message = format!("ashkelon rule {rule}: {message}");
    let body = match wire {
        Wire::AnthropicMessages => serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": full_message,
            },
        }),
        Wire::OpenAiResponses | Wire::OpenAiChat | Wire::Opaque => serde_json::json!({
            "error": {
                "message": full_message,
                "type": "invalid_request_error",
                "code": "ashkelon_rule",
            },
        }),
    };
    (400, serde_json::to_vec(&body).unwrap_or_default())
}

/// Watches a streaming response and names the rule to cut it with, if any.
pub struct StreamGuard {
    max_response_chars: Option<usize>,
    cut_patterns: Vec<(String, Regex)>,
    triggered: bool,
}

impl StreamGuard {
    pub fn new(cfg: &RuleConfig) -> StreamGuard {
        StreamGuard {
            max_response_chars: cfg.max_response_chars,
            cut_patterns: compile_cut_patterns(&cfg.cut_patterns),
            triggered: false,
        }
    }

    pub fn check(&mut self, parser: &dyn ResponseParser) -> Option<String> {
        if self.triggered {
            return None;
        }
        if let Some(max) = self.max_response_chars {
            if parser.output_chars() > max {
                self.triggered = true;
                return Some("max_response_chars".to_string());
            }
        }
        let tail = parser.output_text_tail();
        for (name, re) in &self.cut_patterns {
            if re.is_match(tail) {
                self.triggered = true;
                return Some(name.clone());
            }
        }
        None
    }
}

/// Bytes that end a cut SSE stream the way the provider ends an errored one.
pub fn cut_tail(wire: Wire, rule: &str) -> Vec<u8> {
    let message = format!("ashkelon rule {rule}");
    match wire {
        Wire::AnthropicMessages => {
            let data = serde_json::json!({
                "type": "error",
                "error": {"type": "ashkelon_rule", "message": message},
            });
            format!("event: error\ndata: {data}\n\n").into_bytes()
        }
        Wire::OpenAiResponses => {
            let data = serde_json::json!({
                "type": "response.failed",
                "response": {
                    "status": "failed",
                    "error": {"code": "ashkelon_rule", "message": message},
                },
            });
            format!("event: response.failed\ndata: {data}\n\n").into_bytes()
        }
        Wire::OpenAiChat | Wire::Opaque => {
            let data = serde_json::json!({
                "error": {"message": message, "type": "invalid_request_error", "code": "ashkelon_rule"},
            });
            format!("data: {data}\n\ndata: [DONE]\n\n").into_bytes()
        }
    }
}
