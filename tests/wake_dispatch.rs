use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ashkelon::session::SessionKey;
use ashkelon::wake::runner::{CommandOutput, CommandRunner, HttpPoster};
use ashkelon::wake::{wake_with, WakeTarget};

#[derive(Default)]
struct FakeRunner {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    result: CommandOutput,
}

impl FakeRunner {
    fn succeeding() -> FakeRunner {
        FakeRunner { calls: Mutex::new(Vec::new()), result: CommandOutput { success: true, stdout: Vec::new(), stderr: Vec::new() } }
    }
    fn failing() -> FakeRunner {
        FakeRunner { calls: Mutex::new(Vec::new()), result: CommandOutput { success: false, stdout: Vec::new(), stderr: Vec::new() } }
    }
}

impl CommandRunner for FakeRunner {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CommandOutput>> + Send + 'a>> {
        self.calls.lock().unwrap().push((program.to_string(), args.to_vec()));
        let result = self.result.clone();
        Box::pin(async move { Ok(result) })
    }
}

#[derive(Default)]
struct FakePoster {
    calls: Mutex<Vec<(String, Vec<u8>)>>,
    status: u16,
}

impl HttpPoster for FakePoster {
    fn post_json<'a>(&'a self, url: &'a str, body: &'a [u8]) -> Pin<Box<dyn Future<Output = anyhow::Result<u16>> + Send + 'a>> {
        self.calls.lock().unwrap().push((url.to_string(), body.to_vec()));
        let status = self.status;
        Box::pin(async move { Ok(status) })
    }
}

fn session() -> SessionKey {
    SessionKey { launch: Some("deadbeef".to_string()), harness: Some("codex".to_string()), session: "s1".to_string() }
}

#[tokio::test]
async fn codex_wakes_via_queue_when_session_id_known() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "codex".to_string(), tmux_pane: None, control: None, harness_session_id: Some("thread-1".to_string()) };

    let woken = wake_with(&runner, &poster, &target, &session(), "hello").await.unwrap();

    assert!(woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "codex");
    assert_eq!(calls[0].1, vec!["queue", "--thread", "thread-1", "--message", "hello"]);
}

#[tokio::test]
async fn codex_falls_back_to_tmux_without_session_id() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "codex".to_string(), tmux_pane: Some("%3".to_string()), control: None, harness_session_id: None };

    let woken = wake_with(&runner, &poster, &target, &session(), "hello").await.unwrap();

    assert!(woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], ("tmux".to_string(), vec!["send-keys".to_string(), "-t".to_string(), "%3".to_string(), "-l".to_string(), "hello".to_string()]));
    assert_eq!(calls[1], ("tmux".to_string(), vec!["send-keys".to_string(), "-t".to_string(), "%3".to_string(), "Enter".to_string()]));
}

#[tokio::test]
async fn codex_queue_failure_falls_back_to_tmux() {
    let runner = FakeRunner::failing();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "codex".to_string(), tmux_pane: Some("%3".to_string()), control: None, harness_session_id: Some("thread-1".to_string()) };

    // The runner is configured to fail every call, including the tmux fallback, so this should
    // end up Ok(false) rather than erroring — but it must still have tried both.
    let woken = wake_with(&runner, &poster, &target, &session(), "hello").await.unwrap();

    assert!(!woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].0, "codex");
    assert_eq!(calls[1].0, "tmux");
}

#[tokio::test]
async fn opencode_wakes_via_session_message_endpoint() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster { calls: Mutex::new(Vec::new()), status: 200 };
    let target = WakeTarget {
        harness: "opencode".to_string(),
        tmux_pane: None,
        control: Some("http://127.0.0.1:54321".to_string()),
        harness_session_id: Some("ses_abc".to_string()),
    };

    let woken = wake_with(&runner, &poster, &target, &session(), "hello").await.unwrap();

    assert!(woken);
    let calls = poster.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "http://127.0.0.1:54321/session/ses_abc/message");
    let body: serde_json::Value = serde_json::from_slice(&calls[0].1).unwrap();
    assert_eq!(body["parts"][0]["type"], "text");
    assert_eq!(body["parts"][0]["text"], "hello");
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opencode_non_2xx_falls_back_to_tmux() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster { calls: Mutex::new(Vec::new()), status: 500 };
    let target = WakeTarget {
        harness: "opencode".to_string(),
        tmux_pane: Some("%1".to_string()),
        control: Some("http://127.0.0.1:54321".to_string()),
        harness_session_id: Some("ses_abc".to_string()),
    };

    let woken = wake_with(&runner, &poster, &target, &session(), "hello").await.unwrap();

    assert!(woken);
    assert_eq!(poster.calls.lock().unwrap().len(), 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn claude_has_no_verified_adapter_and_uses_tmux_directly() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "claude".to_string(), tmux_pane: Some("%0".to_string()), control: None, harness_session_id: None };

    let woken = wake_with(&runner, &poster, &target, &session(), "hi").await.unwrap();

    assert!(woken);
    assert!(poster.calls.lock().unwrap().is_empty());
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn nothing_applies_returns_false_without_running_anything() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "hermes".to_string(), tmux_pane: None, control: None, harness_session_id: None };

    let woken = wake_with(&runner, &poster, &target, &session(), "hi").await.unwrap();

    assert!(!woken);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(poster.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tmux_collapses_newlines_to_spaces() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "claude".to_string(), tmux_pane: Some("%2".to_string()), control: None, harness_session_id: None };

    wake_with(&runner, &poster, &target, &session(), "line one\nline two\r\nline three").await.unwrap();

    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1[4], "line one line two  line three");
}

#[tokio::test]
async fn tmux_skips_enter_when_typing_fails() {
    let runner = FakeRunner::failing();
    let poster = FakePoster::default();
    let target = WakeTarget { harness: "claude".to_string(), tmux_pane: Some("%2".to_string()), control: None, harness_session_id: None };

    let woken = wake_with(&runner, &poster, &target, &session(), "hi").await.unwrap();

    assert!(!woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1[0], "send-keys");
}
