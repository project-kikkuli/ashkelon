use ashkelon::hooks::extract::{conversation_length, last_item};
use ashkelon::wire::Wire;

#[test]
fn anthropic_prompt_from_string_content() {
    let body = br#"{"messages":[{"role":"user","content":"hi there"}]}"#;
    assert_eq!(conversation_length(Wire::AnthropicMessages, body), Some(1));
    let e = last_item(Wire::AnthropicMessages, body).unwrap();
    assert_eq!(e.prompt.as_deref(), Some("hi there"));
    assert!(!e.has_tool_result);
}

#[test]
fn anthropic_tool_result_block() {
    let body = br#"{"messages":[
        {"role":"assistant","content":[{"type":"tool_use","id":"1","name":"x"}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"1","content":"ok"}]}
    ]}"#;
    let e = last_item(Wire::AnthropicMessages, body).unwrap();
    assert!(e.has_tool_result);
    assert!(e.prompt.is_none());
}

#[test]
fn anthropic_mixed_tool_result_and_text_fires_both() {
    let body = br#"{"messages":[
        {"role":"user","content":[
            {"type":"tool_result","tool_use_id":"1","content":"ok"},
            {"type":"text","text":"also, one more thing"}
        ]}
    ]}"#;
    let e = last_item(Wire::AnthropicMessages, body).unwrap();
    assert!(e.has_tool_result);
    assert_eq!(e.prompt.as_deref(), Some("also, one more thing"));
}

#[test]
fn anthropic_ignores_ashkelon_ping_text() {
    let body = br#"{"messages":[
        {"role":"user","content":[
            {"type":"text","text":"<ashkelon-ping hook=\"lint\" id=\"abc\">\nmsg\n</ashkelon-ping>"}
        ]}
    ]}"#;
    let e = last_item(Wire::AnthropicMessages, body).unwrap();
    assert!(e.prompt.is_none());
}

#[test]
fn anthropic_assistant_last_message_yields_nothing() {
    let body = br#"{"messages":[{"role":"assistant","content":"done"}]}"#;
    let e = last_item(Wire::AnthropicMessages, body).unwrap();
    assert!(e.prompt.is_none());
    assert!(!e.has_tool_result);
}

#[test]
fn openai_responses_function_call_output_is_tool_result() {
    let body = br#"{"input":[{"type":"function_call_output","call_id":"1","output":"ok"}]}"#;
    assert_eq!(conversation_length(Wire::OpenAiResponses, body), Some(1));
    let e = last_item(Wire::OpenAiResponses, body).unwrap();
    assert!(e.has_tool_result);
}

#[test]
fn openai_responses_custom_tool_call_output_is_tool_result() {
    let body = br#"{"input":[{"type":"custom_tool_call_output","call_id":"1","output":"ok"}]}"#;
    let e = last_item(Wire::OpenAiResponses, body).unwrap();
    assert!(e.has_tool_result);
}

#[test]
fn openai_responses_user_message_prompt() {
    let body = br#"{"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]}]}"#;
    let e = last_item(Wire::OpenAiResponses, body).unwrap();
    assert_eq!(e.prompt.as_deref(), Some("go"));
}

#[test]
fn openai_responses_typeless_message_defaults_to_message() {
    let body = br#"{"input":[{"role":"user","content":"go"}]}"#;
    let e = last_item(Wire::OpenAiResponses, body).unwrap();
    assert_eq!(e.prompt.as_deref(), Some("go"));
}

#[test]
fn openai_chat_tool_role_is_tool_result() {
    let body = br#"{"messages":[{"role":"tool","tool_call_id":"1","content":"ok"}]}"#;
    assert_eq!(conversation_length(Wire::OpenAiChat, body), Some(1));
    let e = last_item(Wire::OpenAiChat, body).unwrap();
    assert!(e.has_tool_result);
    assert!(e.prompt.is_none());
}

#[test]
fn openai_chat_user_message_prompt() {
    let body = br#"{"messages":[{"role":"user","content":"hello"}]}"#;
    let e = last_item(Wire::OpenAiChat, body).unwrap();
    assert_eq!(e.prompt.as_deref(), Some("hello"));
}

#[test]
fn openai_chat_multimodal_parts_join_text() {
    let body = br#"{"messages":[{"role":"user","content":[{"type":"text","text":"a"},{"type":"image_url","image_url":{"url":"x"}},{"type":"text","text":"b"}]}]}"#;
    let e = last_item(Wire::OpenAiChat, body).unwrap();
    assert_eq!(e.prompt.as_deref(), Some("a\nb"));
}

#[test]
fn opaque_wire_yields_nothing() {
    let body = br#"{"anything":true}"#;
    assert_eq!(conversation_length(Wire::Opaque, body), None);
    assert!(last_item(Wire::Opaque, body).is_none());
}

#[test]
fn unparseable_body_yields_nothing() {
    let body = b"not json";
    assert_eq!(conversation_length(Wire::AnthropicMessages, body), None);
    assert!(last_item(Wire::AnthropicMessages, body).is_none());
}

#[test]
fn empty_conversation_yields_nothing() {
    let body = br#"{"messages":[]}"#;
    assert_eq!(conversation_length(Wire::AnthropicMessages, body), Some(0));
    assert!(last_item(Wire::AnthropicMessages, body).is_none());
}
