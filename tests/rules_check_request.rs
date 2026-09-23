use ashkelon::config::RuleConfig;
use ashkelon::rules::{check_request, reject_response, PreDecision};
use ashkelon::wire::Wire;
use serde_json::json;

fn allow_models_cfg(patterns: &[&str]) -> RuleConfig {
    RuleConfig { allow_models: patterns.iter().map(|s| s.to_string()).collect(), ..RuleConfig::default() }
}

fn max_tokens_cfg(cap: u64) -> RuleConfig {
    RuleConfig { max_output_tokens: Some(cap), ..RuleConfig::default() }
}

fn is_reject(d: &PreDecision) -> bool {
    matches!(d, PreDecision::Reject { .. })
}

#[test]
fn allow_models_admits_a_matching_model() {
    let cfg = allow_models_cfg(&["^claude-.*$"]);
    let body = json!({"model": "claude-opus-4"}).to_string();
    assert!(matches!(check_request(Wire::AnthropicMessages, &cfg, body.as_bytes()), PreDecision::Allow));
}

#[test]
fn allow_models_rejects_a_non_matching_model() {
    let cfg = allow_models_cfg(&["^claude-.*$"]);
    let body = json!({"model": "gpt-4"}).to_string();
    match check_request(Wire::AnthropicMessages, &cfg, body.as_bytes()) {
        PreDecision::Reject { rule, message } => {
            assert_eq!(rule, "allow_models");
            assert!(message.contains("gpt-4"));
        }
        PreDecision::Allow => panic!("expected a rejection"),
    }
}

#[test]
fn allow_models_rejects_a_missing_model_field() {
    let cfg = allow_models_cfg(&["^claude-.*$"]);
    let body = json!({}).to_string();
    assert!(is_reject(&check_request(Wire::AnthropicMessages, &cfg, body.as_bytes())));
}

#[test]
fn empty_allow_models_admits_anything() {
    let cfg = RuleConfig::default();
    let body = json!({"model": "anything-goes"}).to_string();
    assert!(matches!(check_request(Wire::AnthropicMessages, &cfg, body.as_bytes()), PreDecision::Allow));
}

#[test]
fn an_invalid_allow_models_pattern_is_ignored_but_others_still_apply() {
    let cfg = allow_models_cfg(&["(unclosed", "^claude-.*$"]);
    let matching = json!({"model": "claude-haiku"}).to_string();
    assert!(matches!(check_request(Wire::AnthropicMessages, &cfg, matching.as_bytes()), PreDecision::Allow));
    let non_matching = json!({"model": "gpt-4"}).to_string();
    assert!(is_reject(&check_request(Wire::AnthropicMessages, &cfg, non_matching.as_bytes())));
}

#[test]
fn max_output_tokens_allows_under_the_cap() {
    let cfg = max_tokens_cfg(100);
    let body = json!({"model": "claude", "max_tokens": 50}).to_string();
    assert!(matches!(check_request(Wire::AnthropicMessages, &cfg, body.as_bytes()), PreDecision::Allow));
}

#[test]
fn max_output_tokens_rejects_anthropic_max_tokens_over_the_cap() {
    let cfg = max_tokens_cfg(100);
    let body = json!({"model": "claude", "max_tokens": 500}).to_string();
    match check_request(Wire::AnthropicMessages, &cfg, body.as_bytes()) {
        PreDecision::Reject { rule, message } => {
            assert_eq!(rule, "max_output_tokens");
            assert!(message.contains("500"));
        }
        PreDecision::Allow => panic!("expected a rejection"),
    }
}

#[test]
fn max_output_tokens_rejects_responses_max_output_tokens_over_the_cap() {
    let cfg = max_tokens_cfg(100);
    let body = json!({"model": "gpt", "max_output_tokens": 500}).to_string();
    assert!(is_reject(&check_request(Wire::OpenAiResponses, &cfg, body.as_bytes())));
}

#[test]
fn max_output_tokens_rejects_chat_max_completion_tokens_over_the_cap() {
    let cfg = max_tokens_cfg(100);
    let body = json!({"model": "gpt-4", "max_completion_tokens": 500}).to_string();
    assert!(is_reject(&check_request(Wire::OpenAiChat, &cfg, body.as_bytes())));
}

#[test]
fn no_cap_configured_allows_any_token_count() {
    let cfg = RuleConfig::default();
    let body = json!({"model": "claude", "max_tokens": 999_999}).to_string();
    assert!(matches!(check_request(Wire::AnthropicMessages, &cfg, body.as_bytes()), PreDecision::Allow));
}

#[test]
fn unparseable_body_is_allowed() {
    let cfg = allow_models_cfg(&["^claude-.*$"]);
    assert!(matches!(check_request(Wire::AnthropicMessages, &cfg, b"not json"), PreDecision::Allow));
}

// ---- reject_response shapes ----

#[test]
fn anthropic_reject_response_is_shaped_like_an_anthropic_error() {
    let (status, body) = reject_response(Wire::AnthropicMessages, "allow_models", "model x is not allowed");
    assert_eq!(status, 400);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("ashkelon rule allow_models"));
    assert!(message.contains("model x is not allowed"));
}

#[test]
fn openai_responses_reject_response_is_shaped_like_an_openai_error() {
    let (status, body) = reject_response(Wire::OpenAiResponses, "max_output_tokens", "too many tokens");
    assert_eq!(status, 400);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "ashkelon_rule");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert!(v["error"]["message"].as_str().unwrap().contains("too many tokens"));
}

#[test]
fn openai_chat_reject_response_is_shaped_like_an_openai_error() {
    let (status, body) = reject_response(Wire::OpenAiChat, "max_output_tokens", "too many tokens");
    assert_eq!(status, 400);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "ashkelon_rule");
    assert_eq!(v["error"]["type"], "invalid_request_error");
}
