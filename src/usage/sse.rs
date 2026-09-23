/// Splits an incrementally-fed byte stream into Server-Sent Events, tolerating
/// chunk boundaries anywhere (including mid-UTF8-character, since a `\n` byte
/// never appears inside a multi-byte UTF-8 sequence) and both CRLF and LF line
/// endings. Comment lines (`:...`) and unrecognized fields are ignored; `data:`
/// lines within one event are joined with `\n` per the SSE spec.
pub struct SseSplitter {
    buf: Vec<u8>,
    event_type: Option<String>,
    data_lines: Vec<String>,
    any_field_seen: bool,
}

impl SseSplitter {
    pub fn new() -> Self {
        SseSplitter {
            buf: Vec::new(),
            event_type: None,
            data_lines: Vec::new(),
            any_field_seen: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8], mut on_event: impl FnMut(Option<&str>, &str)) {
        self.buf.extend_from_slice(chunk);
        loop {
            let Some(pos) = self.buf.iter().position(|&b| b == b'\n') else {
                break;
            };
            let mut line_bytes: Vec<u8> = self.buf.drain(..=pos).collect();
            line_bytes.pop(); // trailing '\n'
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes).into_owned();
            self.process_line(&line, &mut on_event);
        }
    }

    /// Dispatches any event left pending without a trailing blank line (some
    /// servers close the stream without one).
    pub fn flush(&mut self, mut on_event: impl FnMut(Option<&str>, &str)) {
        if !self.buf.is_empty() {
            let mut line_bytes = std::mem::take(&mut self.buf);
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes).into_owned();
            if !line.is_empty() {
                self.process_line(&line, &mut on_event);
            }
        }
        self.dispatch(&mut on_event);
    }

    fn process_line(&mut self, line: &str, on_event: &mut impl FnMut(Option<&str>, &str)) {
        if line.is_empty() {
            self.dispatch(on_event);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        self.any_field_seen = true;
        match field {
            "event" => self.event_type = Some(value.to_string()),
            "data" => self.data_lines.push(value.to_string()),
            _ => {}
        }
    }

    fn dispatch(&mut self, on_event: &mut impl FnMut(Option<&str>, &str)) {
        if self.any_field_seen && !self.data_lines.is_empty() {
            let data = self.data_lines.join("\n");
            let event_type = self.event_type.take();
            on_event(event_type.as_deref(), &data);
        }
        self.event_type = None;
        self.data_lines.clear();
        self.any_field_seen = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<(Option<String>, String)> {
        let mut splitter = SseSplitter::new();
        let mut events = Vec::new();
        for chunk in chunks {
            splitter.feed(chunk, |et, data| events.push((et.map(String::from), data.to_string())));
        }
        splitter.flush(|et, data| events.push((et.map(String::from), data.to_string())));
        events
    }

    #[test]
    fn basic_event() {
        let events = collect(&[b"event: message_start\ndata: {\"a\":1}\n\n"]);
        assert_eq!(
            events,
            vec![(Some("message_start".to_string()), "{\"a\":1}".to_string())]
        );
    }

    #[test]
    fn split_at_every_byte_offset() {
        let input: &[u8] = b"event: foo\ndata: {\"x\":1}\ndata: {\"y\":2}\n\nevent: bar\ndata: baz\n\n";
        for split in 0..=input.len() {
            let (a, b) = input.split_at(split);
            let events = collect(&[a, b]);
            assert_eq!(
                events,
                vec![
                    (Some("foo".to_string()), "{\"x\":1}\n{\"y\":2}".to_string()),
                    (Some("bar".to_string()), "baz".to_string()),
                ],
                "failed at split offset {split}"
            );
        }
    }

    #[test]
    fn crlf_line_endings() {
        let events = collect(&[b"event: foo\r\ndata: bar\r\n\r\n"]);
        assert_eq!(events, vec![(Some("foo".to_string()), "bar".to_string())]);
    }

    #[test]
    fn comments_and_unknown_fields_ignored() {
        let events = collect(&[b": keep-alive\nid: 5\nretry: 100\ndata: hi\n\n"]);
        assert_eq!(events, vec![(None, "hi".to_string())]);
    }

    #[test]
    fn no_trailing_blank_line_still_flushes() {
        let events = collect(&[b"event: foo\ndata: bar"]);
        assert_eq!(events, vec![(Some("foo".to_string()), "bar".to_string())]);
    }

    #[test]
    fn blank_lines_between_events_do_not_emit_empty() {
        let events = collect(&[b"\n\n\ndata: only\n\n"]);
        assert_eq!(events, vec![(None, "only".to_string())]);
    }

    #[test]
    fn garbage_bytes_never_panic() {
        let mut splitter = SseSplitter::new();
        let garbage: Vec<u8> = vec![0xff, 0xfe, 0x00, b'\n', 0x80, 0x80, b'\n', b'\n'];
        splitter.feed(&garbage, |_, _| {});
        splitter.flush(|_, _| {});
    }
}
