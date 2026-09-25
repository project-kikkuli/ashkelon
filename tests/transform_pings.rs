use ashkelon::transform::{conversation_len, inject_pings, validate_image_attachments, ImageAttachment, PinnedPing};
use ashkelon::wire::Wire;
use serde_json::{json, Value};

fn ping(id: &str, anchor: usize, text: &str) -> PinnedPing {
    PinnedPing {
        id: id.to_string(),
        anchor,
        text: text.to_string(),
        attachments: Vec::new(),
    }
}

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("output must still be valid json")
}

fn tiny_png() -> ImageAttachment {
    // A valid 1x1 RGBA PNG used for provider-native media shape tests.
    ImageAttachment {
        mime_type: "image/png".into(),
        data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
            .into(),
        alt_text: Some("visual reference".into()),
    }
}

#[test]
fn image_attachments_are_validated_and_injected_as_native_media() {
    let image = tiny_png();
    assert!(validate_image_attachments(std::slice::from_ref(&image)).is_ok());
    let malformed = ImageAttachment {
        mime_type: "image/png".into(),
        data_base64: "bm90cG5n".into(),
        alt_text: None,
    };
    assert!(validate_image_attachments(&[malformed]).is_err());
    let mismatched = ImageAttachment {
        mime_type: "image/jpeg".into(),
        ..image.clone()
    };
    assert!(validate_image_attachments(&[mismatched]).is_err());
    let too_many = vec![image.clone(); 9];
    assert!(validate_image_attachments(&too_many).is_err());
    let too_large = ImageAttachment {
        data_base64: "A".repeat(7 * 1024 * 1024),
        ..image.clone()
    };
    assert!(validate_image_attachments(&[too_large]).is_err());

    let anthropic_body = json!({"messages":[{"role":"user","content":[
        {"type":"tool_result","content":"preserve"}, {"type":"text","text":"task"}
    ]}]});
    let mut p = ping("media", 0, "[Auxiliary channel]");
    p.attachments.push(image.clone());
    let out = parse(
        &inject_pings(
            Wire::AnthropicMessages,
            &serde_json::to_vec(&anthropic_body).unwrap(),
            &[p],
        )
        .unwrap(),
    );
    let blocks = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[0], anthropic_body["messages"][0]["content"][0]);
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[2]["text"], "visual reference");
    assert_eq!(blocks[3]["type"], "image");
    assert_eq!(blocks[3]["source"]["type"], "base64");
    assert_eq!(blocks[3]["source"]["media_type"], "image/png");
    assert_eq!(blocks[3]["source"]["data"], image.data_base64);
    assert_eq!(blocks[4], anthropic_body["messages"][0]["content"][1]);

    let responses_body = json!({"input":[
        {"type":"message","role":"user","content":[{"type":"input_text","text":"task"}]},
        {"type":"function_call_output","call_id":"c1","output":"preserve"}
    ]});
    let mut p = ping("media", 0, "[Auxiliary channel]");
    p.attachments.push(image.clone());
    let out = parse(
        &inject_pings(
            Wire::OpenAiResponses,
            &serde_json::to_vec(&responses_body).unwrap(),
            &[p],
        )
        .unwrap(),
    );
    let items = out["input"].as_array().unwrap();
    assert_eq!(items[0]["content"][1]["type"], "input_text");
    assert_eq!(items[0]["content"][2]["type"], "input_image");
    assert_eq!(
        items[0]["content"][2]["image_url"],
        format!("data:image/png;base64,{}", image.data_base64)
    );
    assert_eq!(items[2], responses_body["input"][1]);

    let chat_body = json!({"messages":[
        {"role":"user","content":"task"}, {"role":"tool","tool_call_id":"c1","content":"preserve"}
    ]});
    let mut p = ping("media", 0, "[Auxiliary channel]");
    p.attachments.push(image.clone());
    let mut second_image = image.clone();
    second_image.alt_text = Some("second visual reference".into());
    p.attachments.push(second_image);
    let out = parse(&inject_pings(Wire::OpenAiChat, &serde_json::to_vec(&chat_body).unwrap(), &[p]).unwrap());
    let messages = out["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"][1]["type"], "text");
    assert_eq!(messages[0]["content"][2]["type"], "image_url");
    assert_eq!(
        messages[0]["content"][2]["image_url"]["url"],
        format!("data:image/png;base64,{}", image.data_base64)
    );
    assert_eq!(messages[0]["content"][3]["text"], "second visual reference");
    assert_eq!(messages[0]["content"][4]["type"], "image_url");
    assert_eq!(messages[2], chat_body["messages"][1]);

    // Media-only signals are valid and do not create empty provider text blocks.
    let mut p = ping("media", 0, "");
    p.attachments.push(image);
    let out = parse(&inject_pings(Wire::OpenAiChat, &serde_json::to_vec(&chat_body).unwrap(), &[p]).unwrap());
    assert_eq!(out["messages"][0]["content"].as_array().unwrap().len(), 2);
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
fn anthropic_ping_precedes_current_user_text() {
    let body = json!({
        "model": "claude",
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [{"type": "text", "text": "hi"}]},
        ],
    });
    let out = inject_pings(
        Wire::AnthropicMessages,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 0, "a nudge")],
    )
    .unwrap();
    let out = parse(&out);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0], json!({"type": "text", "text": "a nudge"}));
    assert_eq!(content[1]["text"], "hello");
    // The assistant message at index 1 is untouched.
    assert_eq!(out["messages"][1]["content"][0]["text"], "hi");
}

#[test]
fn anthropic_multiple_pings_on_the_same_anchor_precede_prompt_in_order() {
    let body =
        json!({"model": "claude", "messages": [{"role": "user", "content": [{"type": "text", "text": "start"}]}]});
    let pings = vec![ping("p1", 0, "first"), ping("p2", 0, "second")];
    let out = inject_pings(
        Wire::AnthropicMessages,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &pings,
    )
    .unwrap();
    let out = parse(&out);
    let content = out["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["text"], "first");
    assert_eq!(content[1]["text"], "second");
    assert_eq!(content[2]["text"], "start");
}

#[test]
fn anthropic_ping_on_a_non_user_anchor_is_rejected() {
    let body = json!({"model": "claude", "messages": [{"role": "assistant", "content": "hi"}]});
    let out = inject_pings(
        Wire::AnthropicMessages,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 0, "x")],
    );
    assert!(out.is_none());
}

#[test]
fn anthropic_ping_after_tool_call_keeps_tool_results_first_and_adjacent() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "run the tool"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "run", "input": {}},
                {"type": "tool_use", "id": "t2", "name": "run", "input": {}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "done"},
                {"type": "tool_result", "tool_use_id": "t2", "content": "also done"}
            ]}
        ]
    });
    let mut p = ping("p1", 2, "signal");
    p.attachments.push(tiny_png());
    let out = parse(&inject_pings(Wire::AnthropicMessages, &serde_json::to_vec(&body).unwrap(), &[p]).unwrap());
    let messages = out["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "do not insert a message between tool_use and its result"
    );
    assert_eq!(messages[1], body["messages"][1]);
    let blocks = messages[2]["content"].as_array().unwrap();
    assert_eq!(blocks[0], body["messages"][2]["content"][0]);
    assert_eq!(blocks[1], body["messages"][2]["content"][1]);
    assert_eq!(blocks[2]["text"], "signal");
    assert_eq!(blocks[3]["text"], "visual reference");
    assert_eq!(blocks[4]["type"], "image");
}

#[test]
fn anthropic_ping_out_of_range_anchor_is_rejected() {
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    let out = inject_pings(
        Wire::AnthropicMessages,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 5, "x")],
    );
    assert!(out.is_none());
}

#[test]
fn anthropic_partial_failure_rejects_the_whole_batch() {
    // anchor 0 is a valid user message, but anchor 1 is out of range: nothing should be applied.
    let body = json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    let pings = vec![ping("p1", 0, "ok"), ping("p2", 1, "bad")];
    let out = inject_pings(
        Wire::AnthropicMessages,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &pings,
    );
    assert!(out.is_none());
}

// ---- Responses ----

#[test]
fn responses_ping_inserts_a_new_user_message_before_the_anchor() {
    let body = json!({
        "model": "gpt",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            {"type": "function_call", "call_id": "c1", "name": "run", "arguments": "{}"},
        ],
    });
    let out = inject_pings(
        Wire::OpenAiResponses,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 0, "nudge")],
    )
    .unwrap();
    let out = parse(&out);
    let items = out["input"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["content"][0]["text"], "nudge");
    assert_eq!(items[1]["type"], "message");
    assert_eq!(items[1]["role"], "user");
    assert_eq!(items[1]["content"][0]["type"], "input_text");
    assert_eq!(items[1]["content"][0]["text"], "hi");
    assert_eq!(items[2]["type"], "function_call");
}

#[test]
fn responses_ping_after_tool_call_keeps_function_output_adjacent() {
    let body = json!({
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "run"}]},
            {"type": "function_call", "call_id": "c1", "name": "run", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c1", "output": "done"}
        ]
    });
    let mut p = ping("p1", 2, "signal");
    p.attachments.push(tiny_png());
    let out = parse(&inject_pings(Wire::OpenAiResponses, &serde_json::to_vec(&body).unwrap(), &[p]).unwrap());
    let items = out["input"].as_array().unwrap();
    assert_eq!(items[1], body["input"][1]);
    assert_eq!(items[2], body["input"][2]);
    assert_eq!(items[3]["role"], "user");
    assert_eq!(items[3]["content"][0]["text"], "signal");
    assert_eq!(items[3]["content"][2]["type"], "input_image");
}

#[test]
fn responses_string_input_is_treated_as_a_single_item_at_index_zero() {
    let body = json!({"model": "gpt", "input": "just a prompt"});
    let out = inject_pings(
        Wire::OpenAiResponses,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 0, "nudge")],
    )
    .unwrap();
    let out = parse(&out);
    let items = out["input"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[0]["content"][0]["text"], "nudge");
    assert_eq!(items[1], json!("just a prompt"));
}

#[test]
fn responses_ping_out_of_range_anchor_is_rejected() {
    let body = json!({"model": "gpt", "input": [{"type": "message", "role": "user", "content": []}]});
    let out = inject_pings(
        Wire::OpenAiResponses,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 1, "x")],
    );
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
    let out = inject_pings(
        Wire::OpenAiResponses,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &pings,
    )
    .unwrap();
    let out = parse(&out);
    let items = out["input"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["content"][0]["text"], "after-one");
    assert_eq!(items[1]["content"][0]["text"], "one");
    assert_eq!(items[2]["content"][0]["text"], "after-two");
    assert_eq!(items[3]["content"][0]["text"], "two");
}

// ---- Chat ----

#[test]
fn chat_ping_inserts_a_new_user_message_before_the_anchor() {
    let body = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
        ],
    });
    let out = inject_pings(
        Wire::OpenAiChat,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 1, "nudge")],
    )
    .unwrap();
    let out = parse(&out);
    let messages = out["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1], json!({"role": "user", "content": "nudge"}));
    assert_eq!(messages[2]["content"], "hi");
    assert_eq!(messages[3]["content"], "hello");
}

#[test]
fn chat_ping_after_tool_calls_keeps_tool_messages_contiguous() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "run both tools"},
            {"role": "assistant", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "one", "arguments": "{}"}},
                {"id": "c2", "type": "function", "function": {"name": "two", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "c1", "content": "one done"},
            {"role": "tool", "tool_call_id": "c2", "content": "two done"}
        ]
    });
    let mut p = ping("p1", 3, "signal");
    p.attachments.push(tiny_png());
    let out = parse(&inject_pings(Wire::OpenAiChat, &serde_json::to_vec(&body).unwrap(), &[p]).unwrap());
    let messages = out["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[1], body["messages"][1]);
    assert_eq!(messages[2], body["messages"][2]);
    assert_eq!(messages[3], body["messages"][3]);
    assert_eq!(messages[4]["role"], "user");
    assert_eq!(messages[4]["content"][0]["text"], "signal");
    assert_eq!(messages[4]["content"][2]["type"], "image_url");
}

#[test]
fn chat_ping_out_of_range_anchor_is_rejected() {
    let body = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]});
    let out = inject_pings(
        Wire::OpenAiChat,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 3, "x")],
    );
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
    let out = inject_pings(
        Wire::Opaque,
        serde_json::to_vec(&body).unwrap().as_slice(),
        &[ping("p1", 0, "x")],
    );
    assert!(out.is_none());
}
