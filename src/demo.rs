//! `ashkelon demo`: a self-contained, credential-free walkthrough of the whole pipeline. Every
//! piece is real — the relay, the hook engine, the transforms and rules — except the model
//! provider and the agent, which are both scripted in-process so the demo needs no login and
//! calls no real network.

use std::convert::Infallible;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;

use crate::config::{
    Config, CutPattern, HookConfig, HookEvent, RouteConfig, RuleConfig, ToolOutputTrim, TransformConfig,
};
use crate::hooks::Engine;
use crate::relay::Tracker;

const SESSION_ID: &str = "demo-session";
const SECRET_PATTERN: &str = "LEAKED-SECRET-1234abcd";

type FakeBody = BoxBody<Bytes, Infallible>;

fn text_body(bytes: impl Into<Bytes>) -> FakeBody {
    Full::new(bytes.into()).map_err(|e: Infallible| match e {}).boxed()
}

/// Captures every request the relay forwards it, and answers with the next queued SSE body (or
/// an empty 200 once the queue is drained).
struct FakeProvider {
    seen: Mutex<Vec<Vec<u8>>>,
    responses: Mutex<Vec<String>>,
}

impl FakeProvider {
    fn new(responses: Vec<String>) -> Arc<FakeProvider> {
        Arc::new(FakeProvider {
            seen: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
        })
    }

    fn last_seen(&self) -> Vec<u8> {
        self.seen.lock().unwrap().last().cloned().unwrap_or_default()
    }
}

async fn spawn_fake_provider(provider: Arc<FakeProvider>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake provider");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let provider = provider.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let provider = provider.clone();
                    async move {
                        let body = req
                            .into_body()
                            .collect()
                            .await
                            .map(|c| c.to_bytes())
                            .unwrap_or_default();
                        provider.seen.lock().unwrap().push(body.to_vec());
                        let sse = provider.responses.lock().unwrap().pop_front_compat();
                        let body = sse.unwrap_or_default();
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "text/event-stream")
                                .body(text_body(body))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
    addr
}

/// `Vec::pop_front` doesn't exist; queued responses are consumed oldest-first.
trait PopFrontCompat {
    fn pop_front_compat(&mut self) -> Option<String>;
}
impl PopFrontCompat for Vec<String> {
    fn pop_front_compat(&mut self) -> Option<String> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

fn sse_response(
    thinking: Option<&str>,
    text: &str,
    stop_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> String {
    let mut s = String::new();
    s.push_str("event: message_start\n");
    s.push_str(&format!(
        "data: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_demo\",\"model\":\"claude-demo\",\"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":1}}}}}}\n\n"
    ));
    let mut index = 0;
    if let Some(thinking) = thinking {
        s.push_str(&format!(
            "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\"}}}}\n\n"
        ));
        s.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"thinking_delta\",\"thinking\":{}}}}}\n\n",
            serde_json::to_string(thinking).unwrap()
        ));
        s.push_str(&format!(
            "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{index}}}\n\n"
        ));
        index += 1;
    }
    s.push_str(&format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n"
    ));
    // Stream the text one word at a time, so a mid-stream cut rule has somewhere to land.
    for word in text.split_inclusive(' ') {
        s.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}\n\n",
            serde_json::to_string(word).unwrap()
        ));
    }
    s.push_str(&format!(
        "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{index}}}\n\n"
    ));
    s.push_str(&format!(
        "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":{}}},\"usage\":{{\"output_tokens\":{output_tokens}}}}}\n\n",
        serde_json::to_string(stop_reason).unwrap()
    ));
    s.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    s
}

fn anthropic_request(messages: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "model": "claude-demo",
        "max_tokens": 1024,
        "messages": messages,
    }))
    .unwrap()
}

fn println_header(n: usize, title: &str) {
    println!();
    println!("── step {n}: {title} ──────────────────────────────────────────");
}

fn write_hook_script(path: &std::path::Path) {
    let script = r#"#!/bin/sh
# Demo hook: fails the first time it runs for this session, passes on every later run.
count_file="$ASHKELON_SESSION_DIR/demo-hook-count"
n=$(cat "$count_file" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$count_file"
if [ "$n" -eq 1 ]; then
  printf '{"status":"fail","message":"Demo hook: this always fails on the first turn.","fix":"Retry — it passes from the second turn onward."}'
else
  printf '{"status":"pass"}'
fi
"#;
    let mut f = std::fs::File::create(path).expect("writing demo hook script");
    f.write_all(script.as_bytes()).expect("writing demo hook script");
    drop(f);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Reads `hooks-<today>.jsonl` under `log_dir` and returns the first line (if any) with the
/// given hook name and status, polling up to `timeout`.
async fn wait_for_hook_status(
    log_dir: &std::path::Path,
    hook: &str,
    status: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    let today = time::OffsetDateTime::now_utc().date();
    let filename = format!(
        "hooks-{:04}-{:02}-{:02}.jsonl",
        today.year(),
        u8::from(today.month()),
        today.day()
    );
    let path = log_dir.join(filename);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().rev() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if v["hook"] == hook && v["status"] == status {
                        return Some(v);
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn last_call_record(log_dir: &std::path::Path) -> Option<serde_json::Value> {
    let today = time::OffsetDateTime::now_utc().date();
    let filename = format!(
        "calls-{:04}-{:02}-{:02}.jsonl",
        today.year(),
        u8::from(today.month()),
        today.day()
    );
    let text = std::fs::read_to_string(log_dir.join(filename)).ok()?;
    text.lines().last().and_then(|l| serde_json::from_str(l).ok())
}

/// Parses a captured Anthropic request body and returns the unescaped text of the first
/// `<ashkelon-ping...>` content block found on any message, if any.
fn find_ping_text(body: &[u8]) -> Option<String> {
    let root: serde_json::Value = serde_json::from_slice(body).ok()?;
    let messages = root.get("messages")?.as_array()?;
    for message in messages {
        let Some(content) = message.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for block in content {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                if crate::hooks::is_ping_text(text) {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

pub async fn run() -> anyhow::Result<()> {
    println!("ashkelon demo — a self-contained walkthrough (no credentials, no real network)");

    let scratch = std::env::temp_dir().join(format!("ashkelon-demo-{}", std::process::id()));
    let log_dir = scratch.join("logs");
    let state_dir = scratch.join("state");
    crate::fsperm::create_dir_private(&log_dir)?;
    crate::fsperm::create_dir_private(&state_dir)?;
    let hook_path = scratch.join("demo-hook.sh");
    write_hook_script(&hook_path);

    // Every response the fake provider will give, in call order (see the six steps below).
    let responses = vec![
        sse_response(Some("The user just said hello."), "Hi there! ", "end_turn", 12, 6),
        sse_response(None, "Good to hear from you again. ", "end_turn", 20, 8),
        sse_response(None, "All quiet now. ", "end_turn", 10, 4),
        sse_response(
            None,
            &format!("Here is something safe, but also here is a {SECRET_PATTERN} that should never reach the agent, plus a trailing sentence that must never arrive either. "),
            "end_turn",
            15,
            40,
        ),
        sse_response(None, "Noted the tool output. ", "end_turn", 5, 4),
    ];
    let provider = FakeProvider::new(responses);
    let provider_addr = spawn_fake_provider(provider.clone()).await;

    let cfg = Arc::new(Config {
        listen: None,
        log_dir: Some(log_dir.clone()),
        state_dir: Some(state_dir.clone()),
        log_bodies: false,
        routes: vec![RouteConfig {
            name: "anthropic".to_string(),
            upstream: format!("http://{provider_addr}"),
        }],
        transforms: TransformConfig {
            tool_output: Some(ToolOutputTrim {
                max_chars: 120,
                keep_head: 30,
                keep_tail: 30,
            }),
            strip: Vec::new(),
        },
        rules: RuleConfig {
            allow_models: Vec::new(),
            max_output_tokens: None,
            max_response_chars: None,
            cut_patterns: vec![CutPattern {
                name: "leaked_secret".to_string(),
                pattern: "LEAKED-SECRET-[0-9a-zA-Z]+".to_string(),
            }],
        },
        hooks: vec![HookConfig {
            name: "demo-hook".to_string(),
            on: vec![HookEvent::TurnEnd],
            command: vec![hook_path.to_string_lossy().into_owned()],
            projects: Vec::new(),
            harnesses: Vec::new(),
            timeout_secs: 10,
        }],
        pings: Default::default(),
        models: Vec::new(),
    });

    let engine = Engine::new(cfg.clone());
    Engine::start(&engine);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let relay_addr = listener.local_addr()?;
    let tracker = Arc::new(Tracker::default());
    let relay_cfg = cfg.clone();
    let relay_engine = engine.clone();
    let relay_tracker = tracker.clone();
    tokio::spawn(async move {
        let _ = crate::relay::serve(relay_cfg, listener, relay_engine, relay_tracker).await;
    });

    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let base = format!("http://{relay_addr}/s/demo/anthropic");

    let send = |messages: serde_json::Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri(format!("{base}/v1/messages"))
                .header("host", relay_addr.to_string())
                .header("content-type", "application/json")
                .header("x-session-id", SESSION_ID)
                .body(Full::new(Bytes::from(anthropic_request(messages))))
                .unwrap();
            let resp = client.request(req).await.expect("request to relay");
            let status = resp.status();
            let body = resp
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            (status, body)
        }
    };

    // Step 1: an ordinary turn. Triggers the call log and, once it finishes, the TurnEnd hook
    // (which fails on this, its first, invocation).
    println_header(1, "an ordinary turn, logged as one call");
    let (status, _body) = send(serde_json::json!([{"role": "user", "content": "Hello, ashkelon!"}])).await;
    println!("agent -> relay -> fake provider: HTTP {status}");
    tokio::time::sleep(Duration::from_millis(50)).await;
    match last_call_record(&log_dir) {
        Some(rec) => println!(
            "call log:  status={} model={} wire={} path={} turn_end={} usage={}",
            rec["status"], rec["model"], rec["wire"], rec["path"], rec["turn_end"], rec["usage"]
        ),
        None => println!("call log:  (no record yet — this would be a bug)"),
    }

    println_header(2, "the demo hook fails in the background");
    match wait_for_hook_status(&log_dir, "demo-hook", "fail", Duration::from_secs(5)).await {
        Some(rec) => println!("hooks log: {rec}"),
        None => println!("hooks log: demo-hook never reported fail — this would be a bug"),
    }

    // Step 2: a second turn. The pending ping from the failed hook gets pinned into THIS
    // request before it is forwarded — show what the fake provider actually received.
    println_header(3, "the ping is injected into the next request");
    let (status, _body) = send(serde_json::json!([
        {"role": "user", "content": "Hello, ashkelon!"},
        {"role": "assistant", "content": "Hi there! "},
        {"role": "user", "content": "Second turn, please continue."},
    ]))
    .await;
    println!("agent -> relay -> fake provider: HTTP {status}");
    match find_ping_text(&provider.last_seen()) {
        Some(text) => {
            println!("what the fake provider received (the injected ping text):");
            println!("{text}");
        }
        None => println!("no ping text found in the forwarded request — this would be a bug"),
    }

    // This turn's TurnEnd hook run is demo-hook's second invocation, so it passes and clears
    // the outbox.
    println_header(4, "the hook passes; the ping is cleared");
    match wait_for_hook_status(&log_dir, "demo-hook", "pass", Duration::from_secs(5)).await {
        Some(rec) => println!("hooks log: {rec}"),
        None => println!("hooks log: demo-hook never reported pass — this would be a bug"),
    }
    let (status, _body) = send(serde_json::json!([
        {"role": "user", "content": "Hello, ashkelon!"},
        {"role": "assistant", "content": "Hi there! "},
        {"role": "user", "content": "Second turn, please continue."},
        {"role": "assistant", "content": "Good to hear from you again. "},
        {"role": "user", "content": "Third turn, just checking in."},
    ]))
    .await;
    println!("agent -> relay -> fake provider: HTTP {status}");
    let seen = provider.last_seen();
    let clean = !String::from_utf8_lossy(&seen).contains("<ashkelon-ping");
    println!("third request carries a ping: {} (expected: false)", !clean);

    // Step: a rule cuts a stream carrying a forbidden pattern.
    println_header(5, "a rule cuts a streaming response");
    let (status, body) = send(serde_json::json!([
        {"role": "user", "content": "Print the secret token."},
    ]))
    .await;
    let received = String::from_utf8_lossy(&body);
    println!("agent <- relay <- fake provider: HTTP {status}");
    println!("fake provider intended to send the trailing sentence; the agent actually received:");
    println!("{}", received.trim_end());
    println!(
        "trailing sentence reached the agent: {} (expected: false)",
        received.contains("trailing sentence")
    );

    // Step: a large tool_result in the agent's OWN request gets trimmed before it reaches the
    // provider.
    println_header(6, "a large tool output is trimmed before it reaches the provider");
    let huge = "x".repeat(500);
    let (status, _body) = send(serde_json::json!([
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": huge},
        ]},
    ]))
    .await;
    println!("agent -> relay -> fake provider: HTTP {status}");
    let seen = String::from_utf8_lossy(&provider.last_seen()).into_owned();
    let received_len = seen.matches('x').count();
    println!("tool_result sent by the agent: {} chars", 500);
    println!("tool_result received by the fake provider: {received_len} chars of 'x' (trimmed)");
    if let Some(idx) = seen.find("[ashkelon: trimmed") {
        let end = seen[idx..].find(']').map(|e| idx + e + 1).unwrap_or(seen.len());
        println!("marker: {}", &seen[idx..end]);
    }

    println!();
    println!("demo complete.");
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}
