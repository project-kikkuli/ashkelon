use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::fsperm::{create_dir_private, set_private_file};
use crate::session::SessionKey;
use crate::usage::Usage;
use crate::wire::Wire;

/// One line of the daily call log. Never carries message content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallRecord {
    pub ts: String,
    pub call_id: String,
    pub session: SessionKey,
    pub route: String,
    pub wire: Wire,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub tool_calls: Vec<String>,
    pub turn_end: bool,
    pub ttfb_ms: Option<u64>,
    pub total_ms: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    /// Names of transforms that changed the request (the upstream saw different bytes than the agent sent).
    pub transforms: Vec<String>,
    /// Pings added to this request.
    pub pings_injected: Vec<String>,
    /// The rule that rejected or cut this call, if any.
    pub rule: Option<String>,
    pub error: Option<String>,
}

pub fn now_ts() -> String {
    OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default()
}

/// Which side of a call a logged body belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Request,
    Response,
}

impl BodyKind {
    fn extension(self) -> &'static str {
        match self {
            BodyKind::Request => "request",
            BodyKind::Response => "response",
        }
    }
}

/// Appends `CallRecord`s to a daily JSONL file under `log_dir`, and optionally raw request/response
/// bodies under `log_dir/bodies/`. One writer per relay process; every write is serialized.
pub struct Writer {
    log_dir: PathBuf,
    lock: Mutex<()>,
}

impl Writer {
    pub fn new(log_dir: impl Into<PathBuf>) -> std::io::Result<Writer> {
        let log_dir = log_dir.into();
        create_dir_private(&log_dir)?;
        Ok(Writer {
            log_dir,
            lock: Mutex::new(()),
        })
    }

    /// Builds a writer without requiring its log directory to exist yet. Telemetry is ashkelon's
    /// own optional work: a log directory ashkelon can't create must never stop the relay from
    /// starting and forwarding calls. Every write still tries (and still fails open on its own,
    /// logging a warning) independently of this constructor.
    pub fn degraded(log_dir: impl Into<PathBuf>) -> Writer {
        Writer {
            log_dir: log_dir.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn write(&self, record: &CallRecord) -> std::io::Result<()> {
        let line =
            serde_json::to_string(record).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let date = OffsetDateTime::now_utc().date();
        let filename = format!(
            "calls-{:04}-{:02}-{:02}.jsonl",
            date.year(),
            u8::from(date.month()),
            date.day()
        );
        let path = self.log_dir.join(filename);

        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        set_private_file(&file)?;
        writeln!(file, "{line}")?;
        file.flush()
    }

    pub fn write_body(&self, call_id: &str, kind: BodyKind, bytes: &[u8]) -> std::io::Result<()> {
        let dir = self.log_dir.join("bodies");
        create_dir_private(&dir)?;
        let path = dir.join(format!("{call_id}.{}", kind.extension()));

        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(&path)?;
        set_private_file(&file)?;
        file.write_all(bytes)?;
        file.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::wire::Wire;

    fn sample_record() -> CallRecord {
        CallRecord {
            ts: now_ts(),
            call_id: "call-1".to_string(),
            session: SessionKey {
                launch: None,
                harness: None,
                session: "s1".to_string(),
            },
            route: "anthropic".to_string(),
            wire: Wire::AnthropicMessages,
            method: "POST".to_string(),
            path: "/s/l1/anthropic/v1/messages".to_string(),
            status: 200,
            model: Some("claude-x".to_string()),
            stop_reason: Some("end_turn".to_string()),
            usage: Usage::default(),
            tool_calls: Vec::new(),
            turn_end: true,
            ttfb_ms: Some(12),
            total_ms: 34,
            request_bytes: 10,
            response_bytes: 20,
            transforms: Vec::new(),
            pings_injected: Vec::new(),
            rule: None,
            error: None,
        }
    }

    #[test]
    fn writes_one_line_per_record_to_todays_file() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::new(dir.path()).unwrap();
        writer.write(&sample_record()).unwrap();
        writer.write(&sample_record()).unwrap();

        let date = OffsetDateTime::now_utc().date();
        let filename = format!(
            "calls-{:04}-{:02}-{:02}.jsonl",
            date.year(),
            u8::from(date.month()),
            date.day()
        );
        let contents = fs::read_to_string(dir.path().join(filename)).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: CallRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.call_id, "call-1");
    }

    #[test]
    fn writes_bodies_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::new(dir.path()).unwrap();
        writer.write_body("call-1", BodyKind::Request, b"req-bytes").unwrap();
        writer.write_body("call-1", BodyKind::Response, b"resp-bytes").unwrap();

        let req = fs::read(dir.path().join("bodies/call-1.request")).unwrap();
        let resp = fs::read(dir.path().join("bodies/call-1.response")).unwrap();
        assert_eq!(req, b"req-bytes");
        assert_eq!(resp, b"resp-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn directories_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::new(dir.path().join("logs")).unwrap();
        writer.write(&sample_record()).unwrap();

        let dir_mode = fs::metadata(dir.path().join("logs")).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let date = OffsetDateTime::now_utc().date();
        let filename = format!(
            "calls-{:04}-{:02}-{:02}.jsonl",
            date.year(),
            u8::from(date.month()),
            date.day()
        );
        let file_mode = fs::metadata(dir.path().join("logs").join(filename))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
    }
}
