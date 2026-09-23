use ashkelon::config::{StripRule, TransformConfig};
use ashkelon::transform::apply;
use ashkelon::wire::Wire;
use serde_json::{json, Value};

fn strip_cfg(rules: Vec<(&str, &str)>) -> TransformConfig {
    TransformConfig {
        tool_output: None,
        strip: rules
            .into_iter()
            .map(|(name, pattern)| StripRule { name: name.to_string(), pattern: pattern.to_string() })
            .collect(),
    }
}

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("output must still be valid json")
}

#[test]
fn anthropic_user_string_content_is_stripped() {
    let body = json!({
        "model": "claude",
        "messages": [{"role": "user", "content": "hello SECRET-1234 world"}],
    });
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-\d+")]);
    let applied = apply(Wire::AnthropicMessages, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    assert_eq!(out["messages"][0]["content"], "hello  world");
}

#[test]
fn anthropic_user_text_block_is_dropped_when_it_becomes_empty() {
    let body = json!({
        "model": "claude",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "SECRET-1"},
                {"type": "text", "text": "keep me"},
                {"type": "image", "source": {"type": "base64", "data": "abc"}},
            ],
        }],
    });
    let cfg = strip_cfg(vec![("drop_secrets", r"^SECRET-1$")]);
    let applied = apply(Wire::AnthropicMessages, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["text"], "keep me");
    assert_eq!(content[1]["type"], "image");
}

#[test]
fn anthropic_assistant_and_tool_result_text_is_never_stripped() {
    let body = json!({
        "model": "claude",
        "messages": [
            {"role": "assistant", "content": [{"type": "text", "text": "SECRET-1 in my own reply"}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "SECRET-1"}]},
        ],
    });
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::AnthropicMessages, &cfg, &raw);
    // Only user-role plain TEXT blocks are in scope; the assistant text and the tool_result content
    // (a non-text field on a user-role message) must be left alone, so nothing here changes.
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}

#[test]
fn responses_input_text_is_stripped_and_dropped_when_empty() {
    let body = json!({
        "model": "gpt",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "SECRET-1"},
                {"type": "input_text", "text": "keep SECRET-1 me"},
                {"type": "input_image", "image_url": "http://example.test/x.png"},
            ],
        }],
    });
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::OpenAiResponses, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    let content = out["input"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["text"], "keep  me");
    assert_eq!(content[1]["type"], "input_image");
}

#[test]
fn responses_non_user_message_is_untouched() {
    let body = json!({
        "model": "gpt",
        "input": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "SECRET-1"}],
        }],
    });
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::OpenAiResponses, &cfg, &raw);
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}

#[test]
fn chat_user_string_content_is_stripped() {
    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi SECRET-1 there"}],
    });
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::OpenAiChat, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    assert_eq!(out["messages"][0]["content"], "hi  there");
}

#[test]
fn chat_user_text_part_is_dropped_when_it_becomes_empty() {
    let body = json!({
        "model": "gpt-4",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "SECRET-1"},
                {"type": "text", "text": "keep me"},
            ],
        }],
    });
    let cfg = strip_cfg(vec![("drop_secrets", r"^SECRET-1$")]);
    let applied = apply(Wire::OpenAiChat, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["text"], "keep me");
}

#[test]
fn multiple_strip_rules_each_report_their_own_name_when_they_change_something() {
    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "alpha-1 beta-2"}],
    });
    let cfg = strip_cfg(vec![("drop_alpha", r"alpha-\d"), ("drop_gamma", r"gamma-\d")]);
    let applied = apply(Wire::OpenAiChat, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    // drop_gamma matches nothing, so only drop_alpha is reported as changed.
    assert_eq!(applied.changed, vec!["drop_alpha".to_string()]);
    let out = parse(&applied.body);
    assert_eq!(out["messages"][0]["content"], " beta-2");
}

#[test]
fn invalid_regex_is_ignored_without_blocking_other_rules() {
    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "SECRET-1 stays or goes"}],
    });
    let cfg = strip_cfg(vec![("broken", r"("), ("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::OpenAiChat, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    assert_eq!(out["messages"][0]["content"], " stays or goes");
}

#[test]
fn responses_reasoning_item_is_never_stripped() {
    // A "reasoning" item is never a user "message" item, so the role/type guard must skip it
    // entirely, even though its encrypted_content field is a string like any other.
    let body = json!({
        "model": "gpt",
        "input": [
            {"type": "reasoning", "id": "r1", "encrypted_content": "SECRET-1", "summary": []},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
        ],
    });
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::OpenAiResponses, &cfg, &raw);
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}

#[test]
fn anthropic_non_text_blocks_inside_a_user_message_are_never_touched_by_strip() {
    // Only type=="text" blocks are in scope; any other block type on a user-role message
    // (however unusual) must survive strip untouched, including its signature-shaped fields.
    let body = json!({
        "model": "claude",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "thinking", "thinking": "SECRET-1", "signature": "SECRET-1"},
                {"type": "text", "text": "keep SECRET-1 middle"},
            ],
        }],
    });
    let cfg = strip_cfg(vec![("drop_secrets", r"SECRET-1")]);
    let applied = apply(Wire::AnthropicMessages, &cfg, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(applied.changed, vec!["drop_secrets".to_string()]);
    let out = parse(&applied.body);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["thinking"], "SECRET-1");
    assert_eq!(content[0]["signature"], "SECRET-1");
    assert_eq!(content[1]["text"], "keep  middle");
}

#[test]
fn purely_invalid_regex_leaves_body_byte_identical() {
    let body = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hello"}]});
    let raw = serde_json::to_vec(&body).unwrap();
    let cfg = strip_cfg(vec![("broken", r"(unclosed")]);
    let applied = apply(Wire::OpenAiChat, &cfg, &raw);
    assert!(applied.changed.is_empty());
    assert_eq!(applied.body, raw);
}
