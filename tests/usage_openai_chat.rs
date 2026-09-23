use ashkelon::usage::{parser_for, Summary};
use ashkelon::wire::Wire;

fn run(chunks: &[&[u8]]) -> (Summary, usize, String) {
    let mut parser = parser_for(Wire::OpenAiChat).expect("chat parser");
    for chunk in chunks {
        parser.feed(chunk);
    }
    let output_chars = parser.output_chars();
    let tail = parser.output_text_tail().to_string();
    (parser.finish(), output_chars, tail)
}

fn sse_data(data: &str) -> String {
    format!("data: {data}\n\n")
}

#[test]
fn streaming_tool_calls_assembled_by_index_with_exact_usage() {
    let mut stream = String::new();
    stream += &sse_data(
        r#"{"id":"chatcmpl_1","object":"chat.completion.chunk","model":"gpt-y","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
    );
    stream +=
        &sse_data(r#"{"id":"chatcmpl_1","choices":[{"index":0,"delta":{"content":"Sure, "},"finish_reason":null}]}"#);
    stream += &sse_data(
        r#"{"id":"chatcmpl_1","choices":[{"index":0,"delta":{"content":"let me check."},"finish_reason":null}]}"#,
    );
    stream += &sse_data(
        r#"{"id":"chatcmpl_1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":""}}]},"finish_reason":null}]}"#,
    );
    stream += &sse_data(
        r#"{"id":"chatcmpl_1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]},"finish_reason":null}]}"#,
    );
    stream += &sse_data(
        r#"{"id":"chatcmpl_1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"cats\"}"}}]},"finish_reason":null}]}"#,
    );
    stream += &sse_data(r#"{"id":"chatcmpl_1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#);
    stream += &sse_data(
        r#"{"id":"chatcmpl_1","choices":[],"usage":{"prompt_tokens":50,"prompt_tokens_details":{"cached_tokens":10},"completion_tokens":9,"completion_tokens_details":{"reasoning_tokens":0}}}"#,
    );
    stream += &sse_data("[DONE]");

    let (summary, output_chars, tail) = run(&[stream.as_bytes()]);

    assert_eq!(summary.model.as_deref(), Some("gpt-y"));
    assert_eq!(summary.response_id.as_deref(), Some("chatcmpl_1"));
    assert_eq!(summary.stop_reason.as_deref(), Some("tool_calls"));
    assert!(!summary.turn_end);
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_1");
    assert_eq!(summary.tool_calls[0].name, "lookup");
    assert_eq!(summary.usage.input_tokens, Some(50));
    assert_eq!(summary.usage.cache_read_tokens, Some(10));
    assert_eq!(summary.usage.output_tokens, Some(9));
    assert_eq!(summary.usage.reasoning_tokens, Some(0));
    assert!(!summary.usage.reasoning_estimated);
    assert_eq!(summary.text, "Sure, let me check.");
    assert_eq!(tail, "Sure, let me check.");
    let expected_chars = "Sure, ".chars().count() + "let me check.".chars().count();
    assert_eq!(output_chars, expected_chars);
}

#[test]
fn openrouter_reasoning_delta_is_estimated_when_no_exact_count() {
    let mut stream = String::new();
    stream += &sse_data(
        r#"{"id":"chatcmpl_2","model":"or-model","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
    );
    stream += &sse_data(
        r#"{"id":"chatcmpl_2","choices":[{"index":0,"delta":{"reasoning":"Thinking really hard"},"finish_reason":null}]}"#,
    );
    stream += &sse_data(
        r#"{"id":"chatcmpl_2","choices":[{"index":0,"delta":{"reasoning":" about it"},"finish_reason":null}]}"#,
    );
    stream +=
        &sse_data(r#"{"id":"chatcmpl_2","choices":[{"index":0,"delta":{"content":"Done."},"finish_reason":null}]}"#);
    stream += &sse_data(r#"{"id":"chatcmpl_2","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#);
    stream += &sse_data("[DONE]");

    let (summary, _, _) = run(&[stream.as_bytes()]);

    assert!(summary.turn_end);
    assert_eq!(summary.text, "Done.");
    assert!(summary.usage.reasoning_tokens.is_some());
    assert!(summary.usage.reasoning_estimated);
    let thinking_chars = "Thinking really hard".chars().count() + " about it".chars().count();
    assert_eq!(summary.usage.reasoning_tokens, Some((thinking_chars / 4) as u64));
    assert!(summary.usage.input_tokens.is_none());
}

#[test]
fn non_streamed_body_reads_message_shape() {
    let body = br#"{"id":"chatcmpl_3","model":"gpt-y","choices":[{"index":0,"message":{"role":"assistant","content":"Answer","tool_calls":[{"id":"call_9","type":"function","function":{"name":"do_x","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":3}}"#;

    let (summary, _, _) = run(&[&body[..15], &body[15..]]);

    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_9");
    assert_eq!(summary.tool_calls[0].name, "do_x");
    assert_eq!(summary.text, "Answer");
    assert!(!summary.turn_end);
    assert_eq!(summary.usage.input_tokens, Some(5));
    assert_eq!(summary.usage.output_tokens, Some(3));
    assert!(summary.usage.reasoning_tokens.is_none());
    assert!(!summary.usage.reasoning_estimated);
}

#[test]
fn non_streamed_error_body_is_recorded() {
    let body = br#"{"error":{"message":"invalid api key","type":"invalid_request_error"}}"#;
    let (summary, _, _) = run(&[body]);
    assert_eq!(summary.error.as_deref(), Some("invalid api key"));
}

#[test]
fn streaming_error_event_is_recorded() {
    let stream = sse_data(r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#);
    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert_eq!(summary.error.as_deref(), Some("rate limited"));
}

#[test]
fn chunk_split_at_every_byte_offset_agrees_with_whole_stream() {
    let mut stream = String::new();
    stream += &sse_data(
        r#"{"id":"chatcmpl_9","model":"gpt-y","choices":[{"index":0,"delta":{"content":"abc"},"finish_reason":null}]}"#,
    );
    stream += &sse_data(r#"{"id":"chatcmpl_9","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#);
    stream += &sse_data("[DONE]");

    let (whole, _, _) = run(&[stream.as_bytes()]);
    for split in 0..=stream.len() {
        let (a, b) = stream.as_bytes().split_at(split);
        let (summary, _, _) = run(&[a, b]);
        assert_eq!(summary, whole, "mismatch at split {split}");
    }
}

#[test]
fn garbage_input_never_panics() {
    let garbage: &[u8] = &[0x01, 0x02, b'\n', 0xfe, 0xfd, b'\n', b'\n'];
    let (summary, output_chars, tail) = run(&[garbage]);
    assert!(summary.error.is_none());
    assert_eq!(output_chars, 0);
    assert_eq!(tail, "");
    assert_eq!(summary, Summary::default());
}
