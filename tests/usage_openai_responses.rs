use ashkelon::usage::{parser_for, Summary};
use ashkelon::wire::Wire;

fn run(chunks: &[&[u8]]) -> (Summary, usize, String) {
    let mut parser = parser_for(Wire::OpenAiResponses).expect("responses parser");
    for chunk in chunks {
        parser.feed(chunk);
    }
    let output_chars = parser.output_chars();
    let tail = parser.output_text_tail().to_string();
    (parser.finish(), output_chars, tail)
}

fn sse_event(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

#[test]
fn streaming_completed_with_function_call_never_ends_the_turn() {
    let mut stream = String::new();
    stream += &sse_event(
        "response.created",
        r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-x","status":"in_progress","output":[]}}"#,
    );
    stream += &sse_event(
        "response.output_text.delta",
        r#"{"type":"response.output_text.delta","delta":"Hi there"}"#,
    );
    stream += &sse_event(
        "response.output_text.delta",
        r#"{"type":"response.output_text.delta","delta":", friend"}"#,
    );
    stream += &sse_event(
        "response.completed",
        r#"{"type":"response.completed","response":{"id":"resp_1","model":"gpt-x","status":"completed","output":[{"type":"message","id":"msg_1","content":[{"type":"output_text","text":"Hi there, friend"}]},{"type":"function_call","id":"fc_1","call_id":"call_abc","name":"get_time","arguments":"{}"}],"usage":{"input_tokens":30,"input_tokens_details":{"cached_tokens":5},"output_tokens":12,"output_tokens_details":{"reasoning_tokens":4}}}}"#,
    );

    let (summary, output_chars, tail) = run(&[stream.as_bytes()]);

    assert_eq!(summary.model.as_deref(), Some("gpt-x"));
    assert_eq!(summary.response_id.as_deref(), Some("resp_1"));
    assert_eq!(summary.stop_reason.as_deref(), Some("completed"));
    assert!(
        !summary.turn_end,
        "a completed response with a pending tool call is not a turn end"
    );
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_abc");
    assert_eq!(summary.tool_calls[0].name, "get_time");
    assert_eq!(summary.usage.input_tokens, Some(30));
    assert_eq!(summary.usage.cache_read_tokens, Some(5));
    assert_eq!(summary.usage.output_tokens, Some(12));
    assert_eq!(summary.usage.reasoning_tokens, Some(4));
    assert!(!summary.usage.reasoning_estimated);
    // Text comes from the streamed deltas, not re-harvested from the final
    // output array (which would double it).
    assert_eq!(summary.text, "Hi there, friend");
    assert_eq!(tail, "Hi there, friend");
    let expected_chars = "Hi there".chars().count() + ", friend".chars().count();
    assert_eq!(output_chars, expected_chars);
    assert!(summary.error.is_none());
}

#[test]
fn streaming_completed_with_no_tool_calls_ends_the_turn() {
    let mut stream = String::new();
    stream += &sse_event(
        "response.output_text.delta",
        r#"{"type":"response.output_text.delta","delta":"All done."}"#,
    );
    stream += &sse_event(
        "response.completed",
        r#"{"type":"response.completed","response":{"id":"resp_2","model":"gpt-x","status":"completed","output":[{"type":"message","id":"msg_2","content":[{"type":"output_text","text":"All done."}]}],"usage":{"input_tokens":5,"output_tokens":3}}}"#,
    );

    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert!(summary.turn_end);
    assert!(summary.tool_calls.is_empty());
}

#[test]
fn streaming_custom_tool_item_done_is_a_tool_call_without_terminal_output() {
    let stream = concat!(
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":2,\"item\":{\"type\":\"custom_tool_call\",\"call_id\":\"call_2\",\"name\":\"custom_op\",\"input\":\"{}\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_7\",\"model\":\"gpt-x\",\"status\":\"completed\",\"output\":[]}}\n\n",
    );

    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert!(!summary.turn_end);
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_2");
    assert_eq!(summary.tool_calls[0].name, "custom_op");
}

#[test]
fn streaming_output_item_done_deduplicates_terminal_output() {
    let mut stream = String::new();
    stream += &sse_event(
        "response.output_item.done",
        r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_3","name":"run","arguments":"{}"}}"#,
    );
    stream += &sse_event(
        "response.completed",
        r#"{"type":"response.completed","response":{"id":"resp_8","model":"gpt-x","status":"completed","output":[{"type":"function_call","call_id":"call_3","name":"run","arguments":"{}"}]}}"#,
    );

    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert!(!summary.turn_end);
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_3");
    assert_eq!(summary.tool_calls[0].name, "run");
}

#[test]
fn web_search_call_is_excluded_from_tool_calls() {
    let stream = sse_event(
        "response.completed",
        r#"{"type":"response.completed","response":{"id":"resp_3","model":"gpt-x","status":"completed","output":[{"type":"web_search_call","id":"ws_1","call_id":"call_ws","name":"search"},{"type":"function_call","id":"fc_2","call_id":"call_real","name":"do_thing"}],"usage":{"input_tokens":1,"output_tokens":1}}}"#,
    );
    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_real");
}

#[test]
fn incomplete_status_reports_reason_and_never_ends_the_turn() {
    let stream = sse_event(
        "response.incomplete",
        r#"{"type":"response.incomplete","response":{"id":"resp_4","model":"gpt-x","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    );
    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert_eq!(summary.stop_reason.as_deref(), Some("incomplete:max_output_tokens"));
    assert!(!summary.turn_end);
}

#[test]
fn failed_status_records_error() {
    let stream = sse_event(
        "response.failed",
        r#"{"type":"response.failed","response":{"id":"resp_5","model":"gpt-x","status":"failed","output":[],"error":{"message":"internal error"}}}"#,
    );
    let (summary, _, _) = run(&[stream.as_bytes()]);
    assert_eq!(summary.stop_reason.as_deref(), Some("failed"));
    assert_eq!(summary.error.as_deref(), Some("internal error"));
    assert!(!summary.turn_end);
}

#[test]
fn non_streamed_body_harvests_text_from_output_array() {
    let body = br#"{"id":"resp_6","model":"gpt-x","status":"completed","output":[{"type":"message","id":"msg_6","content":[{"type":"output_text","text":"Body text"}]},{"type":"custom_tool_call","id":"ct_1","call_id":"call_custom","name":"custom_op"}],"usage":{"input_tokens":7,"input_tokens_details":{"cached_tokens":1},"output_tokens":2,"output_tokens_details":{"reasoning_tokens":0}}}"#;

    let (summary, _, _) = run(&[&body[..20], &body[20..]]);

    assert_eq!(summary.text, "Body text");
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].id, "call_custom");
    assert_eq!(summary.tool_calls[0].name, "custom_op");
    assert_eq!(summary.usage.input_tokens, Some(7));
    assert_eq!(summary.usage.cache_read_tokens, Some(1));
    assert_eq!(summary.usage.output_tokens, Some(2));
    assert_eq!(summary.usage.reasoning_tokens, Some(0));
    assert!(!summary.turn_end, "a pending custom tool call is not a turn end");
}

#[test]
fn non_streamed_error_body_is_recorded() {
    let body = br#"{"error":{"message":"invalid api key","type":"invalid_request_error"}}"#;
    let (summary, _, _) = run(&[body]);
    assert_eq!(summary.error.as_deref(), Some("invalid api key"));
}

#[test]
fn chunk_split_at_every_byte_offset_agrees_with_whole_stream() {
    let mut stream = String::new();
    stream += &sse_event(
        "response.created",
        r#"{"type":"response.created","response":{"id":"resp_9","model":"gpt-x","status":"in_progress"}}"#,
    );
    stream += &sse_event(
        "response.output_text.delta",
        r#"{"type":"response.output_text.delta","delta":"abc"}"#,
    );
    stream += &sse_event(
        "response.completed",
        r#"{"type":"response.completed","response":{"id":"resp_9","model":"gpt-x","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1}}}"#,
    );

    let (whole, _, _) = run(&[stream.as_bytes()]);
    for split in 0..=stream.len() {
        let (a, b) = stream.as_bytes().split_at(split);
        let (summary, _, _) = run(&[a, b]);
        assert_eq!(summary, whole, "mismatch at split {split}");
    }
}

#[test]
fn garbage_input_never_panics() {
    let garbage: &[u8] = &[0x00, 0xff, b'\r', b'\n', 0x80, b'\n', b'\n'];
    let (summary, output_chars, tail) = run(&[garbage]);
    assert!(summary.error.is_none());
    assert_eq!(output_chars, 0);
    assert_eq!(tail, "");
    assert_eq!(summary, Summary::default());
}

#[test]
fn completed_response_with_null_error_is_not_an_error() {
    let mut p = ashkelon::usage::parser_for(ashkelon::wire::Wire::OpenAiResponses).unwrap();
    p.feed(b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"model\":\"m\",\"status\":\"completed\",\"error\":null,\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n");
    let s = p.finish();
    assert_eq!(s.error, None);
    assert!(s.turn_end);
}
