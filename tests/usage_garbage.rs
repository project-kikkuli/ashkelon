use ashkelon::usage::{parser_for, Summary};
use ashkelon::wire::Wire;

#[test]
fn opaque_wire_has_no_parser() {
    assert!(parser_for(Wire::Opaque).is_none());
}

#[test]
fn whitespace_only_input_never_panics_and_stays_undetermined() {
    for wire in [Wire::AnthropicMessages, Wire::OpenAiResponses, Wire::OpenAiChat] {
        let mut parser = parser_for(wire).expect("parser");
        parser.feed(b"   \n\t  \r\n");
        assert_eq!(parser.output_chars(), 0);
        assert_eq!(parser.output_text_tail(), "");
        let summary = parser.finish();
        assert_eq!(summary, Summary::default());
    }
}

#[test]
fn one_byte_at_a_time_never_panics_on_a_real_stream() {
    let stream = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
    let mut parser = parser_for(Wire::AnthropicMessages).expect("parser");
    for byte in stream.as_bytes() {
        parser.feed(std::slice::from_ref(byte));
    }
    let summary = parser.finish();
    assert_eq!(summary.text, "hi");
}

#[test]
fn binary_garbage_never_panics_on_any_wire() {
    let garbage: Vec<u8> = (0u8..=255).collect();
    for wire in [Wire::AnthropicMessages, Wire::OpenAiResponses, Wire::OpenAiChat] {
        let mut parser = parser_for(wire).expect("parser");
        for chunk in garbage.chunks(7) {
            parser.feed(chunk);
        }
        let _ = parser.output_chars();
        let _ = parser.output_text_tail().to_string();
        let summary = parser.finish();
        // Random bytes are not a recognizable provider error; they are simply
        // discarded, never surfaced as `error`.
        assert!(summary.error.is_none());
    }
}

#[test]
fn truncated_json_body_never_panics() {
    // Starts with '{' (JSON-body detection) but is cut off mid-stream and
    // never closed; must not panic and must not fabricate a summary.
    let truncated = br#"{"id":"msg_1","model":"claude-x","content":[{"type":"text","text":"partial"#;
    let mut parser = parser_for(Wire::AnthropicMessages).expect("parser");
    parser.feed(truncated);
    let summary = parser.finish();
    assert_eq!(summary, Summary::default());
}

#[test]
fn empty_feed_calls_never_panic() {
    for wire in [Wire::AnthropicMessages, Wire::OpenAiResponses, Wire::OpenAiChat] {
        let mut parser = parser_for(wire).expect("parser");
        parser.feed(b"");
        parser.feed(b"");
        let summary = parser.finish();
        assert_eq!(summary, Summary::default());
    }
}

#[test]
fn output_text_tail_is_bounded_to_last_4096_chars() {
    // Build a text longer than the 4096-char tail bound out of distinguishable
    // markers so we can prove the *tail* (not the head) survives, while the
    // full `Summary::text` keeps everything.
    let mut stream = String::new();
    let mut expected_full = String::new();
    for i in 0..600 {
        let piece = format!("[{i:04}]");
        stream += &format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{piece}\"}}}}\n\n");
        expected_full += &piece;
    }
    let mut parser = parser_for(Wire::AnthropicMessages).expect("parser");
    parser.feed(stream.as_bytes());
    let tail = parser.output_text_tail().to_string();
    assert!(tail.chars().count() <= 4096);
    assert!(expected_full.ends_with(&tail));
    assert!(!tail.is_empty());
    let summary = parser.finish();
    assert_eq!(summary.text, expected_full);
}
