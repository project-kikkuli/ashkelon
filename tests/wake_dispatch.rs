use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ashkelon::session::SessionKey;
use ashkelon::wake::runner::{CommandOutput, CommandRunner, GetResponse, HttpPoster};
use ashkelon::wake::{wake_with, WakeTarget};

#[derive(Default)]
struct FakeRunner {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    result: CommandOutput,
}

impl FakeRunner {
    fn succeeding() -> FakeRunner {
        FakeRunner {
            calls: Mutex::new(Vec::new()),
            result: CommandOutput {
                success: true,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        }
    }
    fn failing() -> FakeRunner {
        FakeRunner {
            calls: Mutex::new(Vec::new()),
            result: CommandOutput {
                success: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        }
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

type PostCall = (String, Vec<u8>, Option<String>);

#[derive(Default)]
struct FakePoster {
    post_calls: Mutex<Vec<PostCall>>,
    get_calls: Mutex<Vec<(String, Option<String>)>>,
    status: u16,
    get_body: Vec<u8>,
}

impl HttpPoster for FakePoster {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: &'a [u8],
        authorization: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u16>> + Send + 'a>> {
        self.post_calls
            .lock()
            .unwrap()
            .push((url.to_string(), body.to_vec(), authorization.map(str::to_string)));
        let status = self.status;
        Box::pin(async move { Ok(status) })
    }

    fn get_json<'a>(
        &'a self,
        url: &'a str,
        authorization: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<GetResponse>> + Send + 'a>> {
        self.get_calls
            .lock()
            .unwrap()
            .push((url.to_string(), authorization.map(str::to_string)));
        let status = self.status;
        let body = self.get_body.clone();
        Box::pin(async move { Ok(GetResponse { status, body }) })
    }
}

fn target(harness: &str) -> WakeTarget {
    WakeTarget {
        harness: harness.to_string(),
        tmux_pane: None,
        control: None,
        control_auth: None,
        harness_session_id: None,
    }
}

fn session() -> SessionKey {
    SessionKey {
        launch: Some("deadbeef".to_string()),
        harness: Some("codex".to_string()),
        session: "s1".to_string(),
    }
}

#[tokio::test]
async fn codex_wakes_via_queue_when_session_id_known() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = WakeTarget {
        harness_session_id: Some("thread-1".to_string()),
        ..target("codex")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "codex");
    assert_eq!(calls[0].1, vec!["queue", "--thread", "thread-1", "--message", "hello"]);
}

#[tokio::test]
async fn codex_falls_back_to_session_key_thread_id() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = target("codex");

    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1, vec!["queue", "--thread", "s1", "--message", "hello"]);
}

#[tokio::test]
async fn codex_falls_back_to_tmux_when_no_session_id_at_all() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = WakeTarget {
        tmux_pane: Some("%3".to_string()),
        ..target("codex")
    };
    let empty_session = SessionKey {
        launch: None,
        harness: None,
        session: String::new(),
    };

    let woken = wake_with(&runner, &poster, &t, &empty_session, "hello").await.unwrap();

    assert!(woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        (
            "tmux".to_string(),
            vec![
                "send-keys".to_string(),
                "-t".to_string(),
                "%3".to_string(),
                "-l".to_string(),
                "hello".to_string()
            ]
        )
    );
    assert_eq!(
        calls[1],
        (
            "tmux".to_string(),
            vec![
                "send-keys".to_string(),
                "-t".to_string(),
                "%3".to_string(),
                "Enter".to_string()
            ]
        )
    );
}

#[tokio::test]
async fn codex_queue_failure_falls_back_to_tmux() {
    let runner = FakeRunner::failing();
    let poster = FakePoster::default();
    let t = WakeTarget {
        tmux_pane: Some("%3".to_string()),
        harness_session_id: Some("thread-1".to_string()),
        ..target("codex")
    };

    // The runner is configured to fail every call, including the tmux fallback, so this should
    // end up Ok(false) rather than erroring — but it must still have tried both.
    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(!woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].0, "codex");
    assert_eq!(calls[1].0, "tmux");
}

#[tokio::test]
async fn opencode_wakes_via_session_message_endpoint() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster {
        status: 200,
        ..Default::default()
    };
    let t = WakeTarget {
        control: Some("http://127.0.0.1:54321".to_string()),
        control_auth: Some("Basic dGVzdA==".to_string()),
        harness_session_id: Some("ses_abc".to_string()),
        ..target("opencode")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(woken);
    let calls = poster.post_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "http://127.0.0.1:54321/session/ses_abc/message");
    assert_eq!(calls[0].2.as_deref(), Some("Basic dGVzdA=="));
    let body: serde_json::Value = serde_json::from_slice(&calls[0].1).unwrap();
    assert_eq!(body["parts"][0]["type"], "text");
    assert_eq!(body["parts"][0]["text"], "hello");
    assert!(poster.get_calls.lock().unwrap().is_empty());
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opencode_discovers_the_most_recently_updated_session_when_id_unknown() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster {
        status: 200,
        get_body: serde_json::json!([
            { "id": "ses_old", "time": { "updated": 100 } },
            { "id": "ses_new", "time": { "updated": 500 } },
        ])
        .to_string()
        .into_bytes(),
        ..Default::default()
    };
    let t = WakeTarget {
        control: Some("http://127.0.0.1:54321".to_string()),
        ..target("opencode")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(woken);
    assert_eq!(poster.get_calls.lock().unwrap()[0].0, "http://127.0.0.1:54321/session");
    let post_calls = poster.post_calls.lock().unwrap();
    assert_eq!(post_calls[0].0, "http://127.0.0.1:54321/session/ses_new/message");
}

#[tokio::test]
async fn opencode_non_2xx_falls_back_to_tmux() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster {
        status: 500,
        ..Default::default()
    };
    let t = WakeTarget {
        tmux_pane: Some("%1".to_string()),
        control: Some("http://127.0.0.1:54321".to_string()),
        harness_session_id: Some("ses_abc".to_string()),
        ..target("opencode")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(woken);
    assert_eq!(poster.post_calls.lock().unwrap().len(), 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn opencode_no_sessions_found_falls_back_to_tmux() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster {
        status: 200,
        get_body: b"[]".to_vec(),
        ..Default::default()
    };
    let t = WakeTarget {
        tmux_pane: Some("%1".to_string()),
        control: Some("http://127.0.0.1:54321".to_string()),
        ..target("opencode")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hello").await.unwrap();

    assert!(woken);
    assert!(poster.post_calls.lock().unwrap().is_empty());
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn claude_wakes_via_channel_socket() {
    let dir = std::env::temp_dir().join(format!("ashkelon-wake-claude-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("test.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

    let accept = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stream).lines();
        lines.next_line().await.unwrap().unwrap()
    });

    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = WakeTarget {
        control: Some(socket_path.to_string_lossy().into_owned()),
        ..target("claude")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hello from ashkelon")
        .await
        .unwrap();
    assert!(woken);

    let received = tokio::time::timeout(std::time::Duration::from_secs(2), accept)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, "hello from ashkelon");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn claude_missing_socket_falls_back_to_tmux() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = WakeTarget {
        tmux_pane: Some("%0".to_string()),
        control: Some("/nonexistent/ashkelon-test.sock".to_string()),
        ..target("claude")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hi").await.unwrap();

    assert!(woken);
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn claude_without_channel_uses_tmux_directly() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = WakeTarget {
        tmux_pane: Some("%0".to_string()),
        ..target("claude")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hi").await.unwrap();

    assert!(woken);
    assert!(poster.post_calls.lock().unwrap().is_empty());
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn nothing_applies_returns_false_without_running_anything() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = target("hermes");

    let woken = wake_with(&runner, &poster, &t, &session(), "hi").await.unwrap();

    assert!(!woken);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(poster.post_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tmux_collapses_newlines_to_spaces() {
    let runner = FakeRunner::succeeding();
    let poster = FakePoster::default();
    let t = WakeTarget {
        tmux_pane: Some("%2".to_string()),
        ..target("claude")
    };

    wake_with(&runner, &poster, &t, &session(), "line one\nline two\r\nline three")
        .await
        .unwrap();

    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1[4], "line one line two  line three");
}

#[tokio::test]
async fn tmux_skips_enter_when_typing_fails() {
    let runner = FakeRunner::failing();
    let poster = FakePoster::default();
    let t = WakeTarget {
        tmux_pane: Some("%2".to_string()),
        ..target("claude")
    };

    let woken = wake_with(&runner, &poster, &t, &session(), "hi").await.unwrap();

    assert!(!woken);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1[0], "send-keys");
}
