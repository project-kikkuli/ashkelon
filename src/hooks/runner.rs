use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use crate::config::HookConfig;
use crate::session::SessionKey;
use crate::transform::{validate_image_attachments, ImageAttachment};

use super::types::{event_name, HookTrigger};

/// What a hook run amounted to.
pub enum Outcome {
    Pass,
    Noop,
    /// One-request user-message signal. Unlike a failure ping, it is not pinned.
    Signal {
        message: String,
        attachments: Vec<ImageAttachment>,
    },
    Fail {
        message: String,
        fix: Option<String>,
    },
    /// The process did not answer within `timeout_secs` and was killed.
    Timeout,
    /// Spawn failure, non-JSON stdout, or a status other than pass/fail.
    Error(String),
}

pub struct RunResult {
    pub outcome: Outcome,
    pub duration_ms: u64,
}

#[derive(Deserialize)]
struct HookOutput {
    status: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    fix: Option<String>,
    #[serde(default)]
    attachments: Vec<ImageAttachment>,
}

/// Runs one hook command to completion (or until it times out), under `semaphore`'s concurrency
/// cap. Never panics on a misbehaving child: every failure mode becomes an `Outcome`.
#[allow(clippy::too_many_arguments)]
pub async fn run_hook(
    semaphore: &Semaphore,
    hook: &HookConfig,
    key: &SessionKey,
    trigger: &HookTrigger,
    cwd: Option<&Path>,
    session_dir: &Path,
) -> RunResult {
    let start = Instant::now();
    let _permit = semaphore.acquire().await;

    let Some((program, args)) = hook.command.split_first() else {
        return RunResult {
            outcome: Outcome::Error("empty hook command".into()),
            duration_ms: elapsed_ms(start),
        };
    };

    let payload = build_payload(hook, key, trigger, cwd, session_dir);
    let stdin_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            return RunResult {
                outcome: Outcome::Error(format!("payload encode: {e}")),
                duration_ms: elapsed_ms(start),
            }
        }
    };

    let mut cmd = tokio::process::Command::new(super::matching::expand_tilde(program));
    cmd.args(args.iter().map(|a| {
        if a.starts_with('~') {
            super::matching::expand_tilde(a).into_os_string()
        } else {
            a.into()
        }
    }));
    cmd.current_dir(cwd.map(Path::to_path_buf).unwrap_or_else(fallback_dir));
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    cmd.env("ASHKELON_EVENT", event_name(trigger.event));
    cmd.env("ASHKELON_SESSION_DIR", session_dir);
    if let Ok(bin) = std::env::current_exe() {
        cmd.env("ASHKELON_BIN", bin);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return RunResult {
                outcome: Outcome::Error(format!("spawn: {e}")),
                duration_ms: elapsed_ms(start),
            }
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&stdin_bytes).await;
        // Drop closes stdin so a well-behaved hook sees EOF and can exit.
    }

    let Some(mut stdout) = child.stdout.take() else {
        return RunResult {
            outcome: Outcome::Error("no stdout pipe".into()),
            duration_ms: elapsed_ms(start),
        };
    };

    let timeout = Duration::from_secs(hook.timeout_secs);
    let read_and_wait = async {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        let status = child.wait().await;
        (status, buf)
    };

    let outcome = match tokio::time::timeout(timeout, read_and_wait).await {
        Ok((_status, buf)) => parse_output(&buf),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Outcome::Timeout
        }
    };

    RunResult {
        outcome,
        duration_ms: elapsed_ms(start),
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn fallback_dir() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn parse_output(buf: &[u8]) -> Outcome {
    match serde_json::from_slice::<HookOutput>(buf) {
        Ok(out) => match out.status.as_str() {
            "pass" => Outcome::Pass,
            "noop" => Outcome::Noop,
            "signal" => match validate_image_attachments(&out.attachments) {
                Ok(()) => Outcome::Signal {
                    message: out.message.unwrap_or_default(),
                    attachments: out.attachments,
                },
                Err(e) => Outcome::Error(format!("invalid image attachment: {e}")),
            },
            "fail" => Outcome::Fail {
                message: out.message.unwrap_or_default(),
                fix: out.fix,
            },
            other => Outcome::Error(format!("unknown hook status: {other}")),
        },
        Err(e) => Outcome::Error(format!("bad hook output: {e}")),
    }
}

fn build_payload(
    hook: &HookConfig,
    key: &SessionKey,
    trigger: &HookTrigger,
    cwd: Option<&Path>,
    session_dir: &Path,
) -> serde_json::Value {
    let request_path = session_dir.join("request.json");
    let response_path = session_dir.join("response.txt");
    let mut v = serde_json::json!({
        "event": event_name(trigger.event),
        "hook": hook.name,
        "harness": key.harness,
        "session": key.session,
        "launch": key.launch,
        "cwd": cwd.map(|p| p.display().to_string()),
        "state_dir": session_dir.display().to_string(),
        "request_path": request_path.display().to_string(),
        "response_path": response_path.display().to_string(),
        "ts": now_rfc3339(),
    });
    if let Some(p) = &trigger.prompt {
        v["prompt"] = serde_json::json!(p);
    }
    if !trigger.tool_calls.is_empty() {
        v["tool_calls"] = serde_json::json!(trigger.tool_calls);
    }
    if let Some(t) = &trigger.text {
        v["text"] = serde_json::json!(t);
    }
    v
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

pub fn today_date() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}-{:02}", now.year(), u8::from(now.month()), now.day())
}
