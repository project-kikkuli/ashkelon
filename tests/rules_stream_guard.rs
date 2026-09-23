use ashkelon::config::{CutPattern, RuleConfig};
use ashkelon::rules::{cut_tail, StreamGuard};
use ashkelon::usage::{ResponseParser, Summary};
use ashkelon::wire::Wire;

/// A fake parser for exercising `StreamGuard` without any real provider wire format:
/// it just accumulates fed bytes as text.
struct FakeParser {
    text: String,
}

impl FakeParser {
    fn new() -> FakeParser {
        FakeParser { text: String::new() }
    }
}

impl ResponseParser for FakeParser {
    fn feed(&mut self, chunk: &[u8]) {
        self.text.push_str(&String::from_utf8_lossy(chunk));
    }

    fn finish(self: Box<Self>) -> Summary {
        Summary { text: self.text, ..Summary::default() }
    }

    fn output_chars(&self) -> usize {
        self.text.chars().count()
    }

    fn output_text_tail(&self) -> &str {
        &self.text
    }
}

fn cut_pattern(name: &str, pattern: &str) -> CutPattern {
    CutPattern { name: name.to_string(), pattern: pattern.to_string() }
}

#[test]
fn stream_guard_allows_output_under_the_char_cap() {
    let cfg = RuleConfig { max_response_chars: Some(10), ..RuleConfig::default() };
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(b"short");
    assert_eq!(guard.check(&parser), None);
}

#[test]
fn stream_guard_cuts_once_output_exceeds_the_char_cap() {
    let cfg = RuleConfig { max_response_chars: Some(5), ..RuleConfig::default() };
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(b"1234");
    assert_eq!(guard.check(&parser), None);
    parser.feed(b"5678");
    assert_eq!(guard.check(&parser), Some("max_response_chars".to_string()));
}

#[test]
fn stream_guard_only_reports_a_rule_once() {
    let cfg = RuleConfig { max_response_chars: Some(2), ..RuleConfig::default() };
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(b"abcdefgh");
    assert_eq!(guard.check(&parser), Some("max_response_chars".to_string()));
    // Already triggered: keeps quiet even though the condition still holds.
    assert_eq!(guard.check(&parser), None);
}

#[test]
fn stream_guard_cuts_on_a_matching_cut_pattern() {
    let cfg = RuleConfig { cut_patterns: vec![cut_pattern("leak", r"BEGIN PRIVATE KEY")], ..RuleConfig::default() };
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(b"here is some -----BEGIN PRIVATE KEY----- data");
    assert_eq!(guard.check(&parser), Some("leak".to_string()));
}

#[test]
fn stream_guard_checks_char_cap_before_cut_patterns() {
    let cfg = RuleConfig {
        max_response_chars: Some(3),
        cut_patterns: vec![cut_pattern("leak", r"BAD")],
        ..RuleConfig::default()
    };
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(b"BAD"); // matches the pattern AND is already at the char cap once one more byte lands
    parser.feed(b"!");
    assert_eq!(guard.check(&parser), Some("max_response_chars".to_string()));
}

#[test]
fn stream_guard_ignores_an_invalid_cut_pattern_but_keeps_others() {
    let cfg = RuleConfig {
        cut_patterns: vec![cut_pattern("broken", "("), cut_pattern("leak", "BAD")],
        ..RuleConfig::default()
    };
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(b"totally BAD text");
    assert_eq!(guard.check(&parser), Some("leak".to_string()));
}

#[test]
fn stream_guard_with_no_rules_never_cuts() {
    let cfg = RuleConfig::default();
    let mut guard = StreamGuard::new(&cfg);
    let mut parser = FakeParser::new();
    parser.feed(&vec![b'x'; 10_000]);
    assert_eq!(guard.check(&parser), None);
}

// ---- cut_tail shapes ----

#[test]
fn anthropic_cut_tail_is_a_well_formed_error_event() {
    let bytes = cut_tail(Wire::AnthropicMessages, "leak");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("event: error\ndata: "));
    assert!(text.ends_with("\n\n"));
    let data_line = text.strip_prefix("event: error\ndata: ").unwrap().trim_end();
    let v: serde_json::Value = serde_json::from_str(data_line).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "ashkelon_rule");
    assert!(v["error"]["message"].as_str().unwrap().contains("leak"));
}

#[test]
fn responses_cut_tail_is_a_response_failed_event() {
    let bytes = cut_tail(Wire::OpenAiResponses, "leak");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("event: response.failed\ndata: "));
    let data_line = text.strip_prefix("event: response.failed\ndata: ").unwrap().trim_end();
    let v: serde_json::Value = serde_json::from_str(data_line).unwrap();
    assert_eq!(v["type"], "response.failed");
    assert_eq!(v["response"]["status"], "failed");
    assert_eq!(v["response"]["error"]["code"], "ashkelon_rule");
    assert!(v["response"]["error"]["message"].as_str().unwrap().contains("leak"));
}

#[test]
fn chat_cut_tail_is_an_error_chunk_followed_by_done() {
    let bytes = cut_tail(Wire::OpenAiChat, "leak");
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let data_line = lines.next().unwrap();
    let json_part = data_line.strip_prefix("data: ").unwrap();
    let v: serde_json::Value = serde_json::from_str(json_part).unwrap();
    assert_eq!(v["error"]["code"], "ashkelon_rule");
    assert!(v["error"]["message"].as_str().unwrap().contains("leak"));
    let done_line = lines.next().unwrap();
    assert_eq!(done_line, "data: [DONE]");
    assert!(lines.next().is_none());
}

#[test]
fn cut_tail_rule_name_appears_in_every_wire_shape() {
    for wire in [Wire::AnthropicMessages, Wire::OpenAiResponses, Wire::OpenAiChat, Wire::Opaque] {
        let bytes = cut_tail(wire, "my_rule");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("my_rule"), "rule name missing from cut_tail for {wire:?}");
    }
}
