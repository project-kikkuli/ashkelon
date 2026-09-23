use ashkelon::usage::{parser_for, Summary};
use ashkelon::wire::Wire;

fn run(chunks: &[&[u8]]) -> (Summary, usize, String) {
    let mut parser = parser_for(Wire::AnthropicMessages).expect("anthropic parser");
    for chunk in chunks {
        parser.feed(chunk);
    }
    let output_chars = parser.output_chars();
    let tail = parser.output_text_tail().to_string();
    (parser.finish(), output_chars, tail)
}

fn run_split_at_every_offset(whole: &[u8]) -> Vec<Summary> {
    let mut summaries = Vec::new();
    for split in 0..=whole.len() {
        let (a, b) = whole.split_at(split);
        let (summary, _, _) = run(&[a, b]);
        summaries.push(summary);
    }
    summaries
}

fn sse_event(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

#[test]
fn streaming_text_and_tool_use_with_exact_reasoning() {
    let mut stream = String::new();
    stream += &sse_event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-x","role":"assistant","content":[],"stop_reason":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":1}}}"#,
    );
    stream += &sse_event("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}"#);
    stream += &sse_event("content_block_stop", r#"{"type":"content_block_stop","index":0}"#);
    stream += &sse_event(
        "content_block_start",
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
    );
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"loc"}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ation\":\"NYC\"}"}}"#);
    stream += &sse_event("content_block_stop", r#"{"type":"content_block_stop","index":1}"#);
    stream += &sse_event(
        "message_delta",
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42,"output_tokens_details":{"thinking_tokens":7}}}"#,
    );
    stream += &sse_event("message_stop", r#"{"type":"message_stop"}"#);

    let (summary, output_chars, tail) = run(&[stream.as_bytes()]);

    assert_eq!(summary.model.as_deref(), Some("claude-x"));
    assert_eq!(summary.response_id.as_deref(), Some("msg_1"));
    assert_eq!(summary.stop_reason.as_deref(), Some("tool_use"));
    assert!(!summary.turn_end);
    assert_eq!(summary.usage.input_tokens, Some(10));
    assert_eq!(summary.usage.cache_write_tokens, Some(2));
    assert_eq!(summary.usage.cache_read_tokens, Some(3));
    assert_eq!(summary.usage.output_tokens, Some(42));
    assert_eq!(summary.usage.reasoning_tokens, Some(7));
    assert!(!summary.usage.reasoning_estimated);
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "toolu_1");
    assert_eq!(summary.tool_calls[0].name, "get_weather");
    assert_eq!(summary.text, "Hello world");
    assert_eq!(tail, "Hello world");
    assert!(summary.error.is_none());

    let text_chars = "Hello ".chars().count() + "world".chars().count();
    let json_chars = "{\"loc".chars().count() + "ation\":\"NYC\"}".chars().count();
    assert_eq!(output_chars, text_chars + json_chars);
}

#[test]
fn streaming_thinking_is_estimated_and_excluded_from_visible_text() {
    let mut stream = String::new();
    stream += &sse_event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-y","role":"assistant","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":1}}}"#,
    );
    stream += &sse_event("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" about this problem"}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"deadbeef"}}"#);
    stream += &sse_event("content_block_stop", r#"{"type":"content_block_stop","index":0}"#);
    stream += &sse_event("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"The answer is 42."}}"#);
    stream += &sse_event("content_block_stop", r#"{"type":"content_block_stop","index":1}"#);
    stream += &sse_event("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#);
    stream += &sse_event("message_stop", r#"{"type":"message_stop"}"#);

    let (summary, _output_chars, _tail) = run(&[stream.as_bytes()]);

    assert!(summary.turn_end);
    assert_eq!(summary.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(summary.text, "The answer is 42.");
    let thinking_chars = "Let me think".chars().count() + " about this problem".chars().count();
    assert_eq!(summary.usage.reasoning_tokens, Some((thinking_chars / 4) as u64));
    assert!(summary.usage.reasoning_estimated);
}

#[test]
fn non_streamed_body_is_detected_by_leading_brace() {
    let body = br#"{"id":"msg_3","type":"message","role":"assistant","model":"claude-z","content":[{"type":"text","text":"Sure thing"},{"type":"tool_use","id":"toolu_2","name":"search","input":{"q":"cats"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":8,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#;

    // Feed with leading whitespace and split across several chunks to prove
    // detection tolerates both.
    let (summary, _, _) = run(&[b"  \n", &body[..10], &body[10..30], &body[30..]]);

    assert_eq!(summary.model.as_deref(), Some("claude-z"));
    assert_eq!(summary.response_id.as_deref(), Some("msg_3"));
    assert_eq!(summary.stop_reason.as_deref(), Some("tool_use"));
    assert!(!summary.turn_end);
    assert_eq!(summary.text, "Sure thing");
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "toolu_2");
    assert_eq!(summary.tool_calls[0].name, "search");
    assert_eq!(summary.usage.input_tokens, Some(20));
    assert_eq!(summary.usage.output_tokens, Some(8));
    assert_eq!(summary.usage.cache_read_tokens, Some(0));
    assert_eq!(summary.usage.cache_write_tokens, Some(0));
    assert!(summary.usage.reasoning_tokens.is_none());
    assert!(!summary.usage.reasoning_estimated);
    assert!(summary.error.is_none());
}

#[test]
fn streaming_provider_error_event_is_recorded() {
    let stream = sse_event("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#);
    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert_eq!(summary.error.as_deref(), Some("Overloaded"));
}

#[test]
fn non_streamed_error_body_is_recorded() {
    let body = br#"{"type":"error","error":{"type":"invalid_request_error","message":"bad request"}}"#;
    let (summary, _, _) = run(&[body]);
    assert_eq!(summary.error.as_deref(), Some("bad request"));
}

#[test]
fn chunk_split_at_every_byte_offset_agrees_with_whole_stream() {
    let mut stream = String::new();
    stream += &sse_event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"msg_9","model":"claude-x","usage":{"input_tokens":1,"output_tokens":1}}}"#,
    );
    stream += &sse_event("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"abc"}}"#);
    stream += &sse_event("content_block_stop", r#"{"type":"content_block_stop","index":0}"#);
    stream += &sse_event("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#);
    stream += &sse_event("message_stop", r#"{"type":"message_stop"}"#);

    let (whole, _, _) = run(&[stream.as_bytes()]);
    for summary in run_split_at_every_offset(stream.as_bytes()) {
        assert_eq!(summary, whole);
    }
}

#[test]
fn garbage_input_never_panics_and_records_no_error() {
    let garbage: &[u8] = &[0xff, 0xfe, b'!', b'@', 0x00, b'\n', 0x80, 0x80, b'\n', b'\n'];
    let (summary, output_chars, tail) = run(&[garbage]);
    assert!(summary.error.is_none());
    assert_eq!(output_chars, 0);
    assert_eq!(tail, "");
    assert_eq!(summary, Summary::default());
}

#[test]
fn malformed_json_events_are_ignored_but_valid_ones_still_land() {
    let mut stream = String::new();
    stream += "event: content_block_delta\ndata: not json at all {{{\n\n";
    stream += &sse_event("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
    stream += &sse_event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#);
    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert_eq!(summary.text, "ok");
    assert!(summary.error.is_none());
}

#[test]
fn empty_input_yields_default_summary() {
    let (summary, output_chars, tail) = run(&[]);
    assert_eq!(summary, Summary::default());
    assert_eq!(output_chars, 0);
    assert_eq!(tail, "");
}
