use ashkelon::transform::{conversation_len, inject_pings, PinnedPing};
use ashkelon::wire::Wire;
use serde_json::{json, Value};

fn ping(id: &str, anchor: usize, text: &str) -> PinnedPing {
    PinnedPing { id: id.to_string(), anchor, text: text.to_string() }
}

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("output must still be valid json")
}

// ---- conversation_len ----

#[test]
fn conversation_len_counts_anthropic_messages() {
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": "a"}, {"role": "assistant", "content": "b"}]});
    let len = conversation_len(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(len, Some(2));
}

#[test]
fn conversation_len_counts_chat_messages() {
    let body = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "a"}]});
    let len = conversation_len(Wire::OpenAiChat, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(len, Some(1));
}

#[test]
fn conversation_len_treats_a_string_responses_input_as_one_item() {
    let body = json!({"model": "gpt", "input": "just a prompt"});
    let len = conversation_len(Wire::OpenAiResponses, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(len, Some(1));
}

#[test]
fn conversation_len_counts_responses_input_array() {
    let body = json!({"model": "gpt", "input": [{"type": "message", "role": "user", "content": []}, {"type": "message", "role": "assistant", "content": []}]});
    let len = conversation_len(Wire::OpenAiResponses, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(len, Some(2));
}

#[test]
fn conversation_len_is_none_for_unparseable_body() {
    assert_eq!(conversation_len(Wire::AnthropicMessages, b"not json"), None);
}

#[test]
fn conversation_len_is_none_when_the_wire_field_is_missing() {
    let body = json!({"model": "claude"});
    let len = conversation_len(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice());
    assert_eq!(len, None);
}

// ---- inject_pings: no-op ----

#[test]
fn inject_pings_with_no_pings_returns_original_bytes() {
    let raw = br#"{"model":"claude","messages":[{"role":"user","content":"hi"}]}"#.to_vec();
    let out = inject_pings(Wire::AnthropicMessages, &raw, &[]).unwrap();
    assert_eq!(out, raw);
}

// ---- Anthropic ----

#[test]
fn anthropic_ping_is_appended_as_a_text_block_on_the_anchored_user_message() {
    let body = json!({
        "model": "claude",
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [{"type": "text", "text": "hi"}]},
        ],
    });
    let out = inject_pings(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 0, "a nudge")]).unwrap();
    let out = parse(&out);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "hello");
    assert_eq!(content[1], json!({"type": "text", "text": "a nudge"}));
    // The assistant message at index 1 is untouched.
    assert_eq!(out["messages"][1]["content"][0]["text"], "hi");
}

#[test]
fn anthropic_multiple_pings_on_the_same_anchor_are_appended_in_order() {
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": [{"type": "text", "text": "start"}]}]});
    let pings = vec![ping("p1", 0, "first"), ping("p2", 0, "second")];
    let out = inject_pings(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice(), &pings).unwrap();
    let out = parse(&out);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[1]["text"], "first");
    assert_eq!(content[2]["text"], "second");
}

#[test]
fn anthropic_ping_on_a_non_user_anchor_is_rejected() {
    let body = json!({"model": "claude", "messages": [{"role": "assistant", "content": "hi"}]});
    let out = inject_pings(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 0, "x")]);
    assert!(out.is_none());
}

#[test]
fn anthropic_ping_out_of_range_anchor_is_rejected() {
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    let out = inject_pings(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 5, "x")]);
    assert!(out.is_none());
}

#[test]
fn anthropic_partial_failure_rejects_the_whole_batch() {
    // anchor 0 is a valid user message, but anchor 1 is out of range: nothing should be applied.
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    let pings = vec![ping("p1", 0, "ok"), ping("p2", 1, "bad")];
    let out = inject_pings(Wire::AnthropicMessages, serde_json::to_vec(&body).unwrap().as_slice(), &pings);
    assert!(out.is_none());
}

// ---- Responses ----

#[test]
fn responses_ping_inserts_a_new_user_message_right_after_the_anchor() {
    let body = json!({
        "model": "gpt",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            {"type": "function_call", "call_id": "c1", "name": "run", "arguments": "{}"},
        ],
    });
    let out = inject_pings(Wire::OpenAiResponses, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 0, "nudge")]).unwrap();
    let out = parse(&out);
    let items = out["input"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[1]["type"], "message");
    assert_eq!(items[1]["role"], "user");
    assert_eq!(items[1]["content"][0]["type"], "input_text");
    assert_eq!(items[1]["content"][0]["text"], "nudge");
    assert_eq!(items[2]["type"], "function_call");
}

#[test]
fn responses_string_input_is_treated_as_a_single_item_at_index_zero() {
    let body = json!({"model": "gpt", "input": "just a prompt"});
    let out = inject_pings(Wire::OpenAiResponses, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 0, "nudge")]).unwrap();
    let out = parse(&out);
    let items = out["input"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], json!("just a prompt"));
    assert_eq!(items[1]["role"], "user");
    assert_eq!(items[1]["content"][0]["text"], "nudge");
}

#[test]
fn responses_ping_out_of_range_anchor_is_rejected() {
    let body = json!({"model": "gpt", "input": [{"type": "message", "role": "user", "content": []}]});
    let out = inject_pings(Wire::OpenAiResponses, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 1, "x")]);
    assert!(out.is_none());
}

#[test]
fn responses_multiple_pings_on_different_anchors_use_original_positions() {
    let body = json!({
        "model": "gpt",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "one"}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "two"}]},
        ],
    });
    // anchor 0 gets "after-one", anchor 1 gets "after-two" -- both refer to positions in the ORIGINAL array.
    let pings = vec![ping("p1", 1, "after-two"), ping("p2", 0, "after-one")];
    let out = inject_pings(Wire::OpenAiResponses, serde_json::to_vec(&body).unwrap().as_slice(), &pings).unwrap();
    let out = parse(&out);
    let items = out["input"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["content"][0]["text"], "one");
    assert_eq!(items[1]["content"][0]["text"], "after-one");
    assert_eq!(items[2]["content"][0]["text"], "two");
    assert_eq!(items[3]["content"][0]["text"], "after-two");
}

// ---- Chat ----

#[test]
fn chat_ping_inserts_a_new_user_message_right_after_the_anchor() {
    let body = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
        ],
    });
    let out = inject_pings(Wire::OpenAiChat, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 1, "nudge")]).unwrap();
    let out = parse(&out);
    let messages = out["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1]["content"], "hi");
    assert_eq!(messages[2], json!({"role": "user", "content": "nudge"}));
    assert_eq!(messages[3]["content"], "hello");
}

#[test]
fn chat_ping_out_of_range_anchor_is_rejected() {
    let body = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]});
    let out = inject_pings(Wire::OpenAiChat, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 3, "x")]);
    assert!(out.is_none());
}

#[test]
fn inject_pings_on_unparseable_body_is_none() {
    let out = inject_pings(Wire::OpenAiChat, b"not json", &[ping("p1", 0, "x")]);
    assert!(out.is_none());
}

#[test]
fn inject_pings_on_opaque_wire_is_none() {
    let body = json!({"whatever": true});
    let out = inject_pings(Wire::Opaque, serde_json::to_vec(&body).unwrap().as_slice(), &[ping("p1", 0, "x")]);
    assert!(out.is_none());
}
