use ashkelon::config::{ToolOutputTrim, TransformConfig};
use ashkelon::transform::apply;
use ashkelon::wire::Wire;
use serde_json::{json, Value};

fn trim_cfg(max_chars: usize, keep_head: usize, keep_tail: usize) -> TransformConfig {
    TransformConfig { tool_output: Some(ToolOutputTrim { max_chars, keep_head, keep_tail }), strip: Vec::new() }
}

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("output must still be valid json")
}

#[test]
fn anthropic_tool_result_string_content_is_trimmed() {
    let long = "x".repeat(100);
    let body = json!({
        "model": "claude",
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "run", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": long}]},
        ],
    });
    let cfg = trim_cfg(20, 5, 5);
    let applied = apply(Wire::AnthropicMessages, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);

    let out = parse(&applied.body);
    let trimmed = out["messages"][1]["content"][0]["content"].as_str().unwrap();
    assert!(trimmed.starts_with("xxxxx\n[ashkelon: trimmed"));
    assert!(trimmed.ends_with("xxxxx"));
    assert!(trimmed.contains("trimmed 90 chars"));
}

#[test]
fn anthropic_tool_result_block_array_content_is_trimmed_per_block() {
    let long = "y".repeat(50);
    let body = json!({
        "model": "claude",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": [
                    {"type": "text", "text": long},
                    {"type": "text", "text": "short"},
                ],
            }],
        }],
    });
    let cfg = trim_cfg(10, 2, 2);
    let applied = apply(Wire::AnthropicMessages, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);

    let out = parse(&applied.body);
    let blocks = out["messages"][0]["content"][0]["content"].as_array().unwrap();
    assert!(blocks[0]["text"].as_str().unwrap().contains("trimmed"));
    assert_eq!(blocks[1]["text"].as_str().unwrap(), "short");
}

#[test]
fn responses_function_call_output_is_trimmed() {
    let long = "z".repeat(100);
    let body = json!({
        "model": "gpt",
        "input": [
            {"type": "function_call", "call_id": "c1", "name": "run", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c1", "output": long},
        ],
    });
    let cfg = trim_cfg(20, 5, 5);
    let applied = apply(Wire::OpenAiResponses, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);

    let out = parse(&applied.body);
    let trimmed = out["input"][1]["output"].as_str().unwrap();
    assert!(trimmed.contains("trimmed 90 chars"));
    // Untouched sibling item and field must be exactly as sent.
    assert_eq!(out["input"][0]["call_id"], "c1");
}

#[test]
fn responses_custom_tool_call_output_is_trimmed() {
    let long = "w".repeat(30);
    let body = json!({
        "model": "gpt",
        "input": [{"type": "custom_tool_call_output", "call_id": "c1", "output": long}],
    });
    let cfg = trim_cfg(10, 2, 2);
    let applied = apply(Wire::OpenAiResponses, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);
    let out = parse(&applied.body);
    assert!(out["input"][0]["output"].as_str().unwrap().contains("trimmed 26 chars"));
}

#[test]
fn chat_tool_message_string_content_is_trimmed() {
    let long = "q".repeat(40);
    let body = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": "run the thing"},
            {"role": "tool", "tool_call_id": "c1", "content": long},
        ],
    });
    let cfg = trim_cfg(10, 3, 3);
    let applied = apply(Wire::OpenAiChat, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);
    let out = parse(&applied.body);
    assert!(out["messages"][1]["content"].as_str().unwrap().contains("trimmed 34 chars"));
    // The user message is untouched by tool_output trimming.
    assert_eq!(out["messages"][0]["content"], "run the thing");
}

#[test]
fn chat_tool_message_content_parts_are_trimmed_per_part() {
    let long = "p".repeat(40);
    let body = json!({
        "model": "gpt-4",
        "messages": [{
            "role": "tool",
            "tool_call_id": "c1",
            "content": [{"type": "text", "text": long}],
        }],
    });
    let cfg = trim_cfg(10, 3, 3);
    let applied = apply(Wire::OpenAiChat, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);
    let out = parse(&applied.body);
    assert!(out["messages"][0]["content"][0]["text"].as_str().unwrap().contains("trimmed 34 chars"));
}

#[test]
fn short_tool_output_is_left_alone() {
    let body = json!({
        "model": "claude",
        "messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "short"}]}],
    });
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = trim_cfg(1000, 10, 10);
    let applied = apply(Wire::AnthropicMessages, &cfg, &raw);
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}

#[test]
fn byte_identical_when_no_transform_configured() {
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = TransformConfig::default();
    let applied = apply(Wire::AnthropicMessages, &cfg, &raw);
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}

#[test]
fn byte_identical_when_body_is_not_json() {
    let raw = b"not json at all".to_vec();
    let cfg = trim_cfg(1, 1, 1);
    let applied = apply(Wire::AnthropicMessages, &cfg, &raw);
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}

#[test]
fn trimming_is_deterministic_across_repeated_calls() {
    let long = "abc".repeat(200);
    let body = json!({
        "model": "claude",
        "messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": long}]}],
    });
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = trim_cfg(50, 10, 10);
    let first = apply(Wire::AnthropicMessages, &cfg, &raw);
    let second = apply(Wire::AnthropicMessages, &cfg, &raw);
    assert_eq!(first.body, second.body);
    assert_eq!(first.changed, second.changed);
}

#[test]
fn trimming_never_touches_thinking_signature_or_encrypted_content() {
    let long = "x".repeat(100);
    let body = json!({
        "model": "claude",
        "messages": [
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "secret reasoning", "signature": "sig-abc-123"},
                {"type": "redacted_thinking", "data": "opaque-blob"},
            ]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": long}]},
        ],
    });
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = trim_cfg(20, 5, 5);
    let applied = apply(Wire::AnthropicMessages, &cfg, &raw);
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);

    let out = parse(&applied.body);
    assert_eq!(out["messages"][0]["content"][0]["thinking"], "secret reasoning");
    assert_eq!(out["messages"][0]["content"][0]["signature"], "sig-abc-123");
    assert_eq!(out["messages"][0]["content"][1]["data"], "opaque-blob");
    assert_eq!(out["messages"][0]["content"][1]["type"], "redacted_thinking");
}

#[test]
fn responses_reasoning_item_encrypted_content_is_untouched_by_trim() {
    let long = "z".repeat(80);
    let body = json!({
        "model": "gpt",
        "input": [
            {"type": "reasoning", "id": "r1", "encrypted_content": "opaque-cipher-text", "summary": []},
            {"type": "function_call_output", "call_id": "c1", "output": long},
        ],
    });
    let cfg = trim_cfg(20, 5, 5);
    let applied = apply(Wire::OpenAiResponses, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);
    let out = parse(&applied.body);
    assert_eq!(out["input"][0]["encrypted_content"], "opaque-cipher-text");
}

#[test]
fn key_order_is_preserved_on_a_changed_body() {
    let long = "m".repeat(100);
    // Keys deliberately out of alphabetical order, to prove insertion order survives the round trip.
    let raw = format!(
        r#"{{"zzz_last_field":1,"model":"claude","messages":[{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"{long}"}}]}}],"aaa_first_field":2}}"#
    );
    let cfg = trim_cfg(20, 5, 5);
    let applied = apply(Wire::AnthropicMessages, &cfg, raw.as_bytes());
    assert_eq!(applied.changed, vec!["tool_output".to_string()]);

    let text = String::from_utf8(applied.body).unwrap();
    let zzz_pos = text.find("zzz_last_field").unwrap();
    let model_pos = text.find("\"model\"").unwrap();
    let aaa_pos = text.find("aaa_first_field").unwrap();
    assert!(zzz_pos < model_pos, "top-level key order must be preserved");
    assert!(model_pos < aaa_pos, "top-level key order must be preserved");
}
