mod relay_support;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use ashkelon::config::{Config, CutPattern, RouteConfig, ToolOutputTrim};
use ashkelon::hooks::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use tokio::net::TcpListener;

use relay_support::{full_body, spawn_fake_upstream, wait_for_records};

const ANTHROPIC_SSE: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":100,\"output_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"The answer is forty-two, and here is a long explanation of why that is.\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":17}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

type Seen = Arc<Mutex<Vec<Vec<u8>>>>;

async fn upstream_recording(seen: Seen) -> SocketAddr {
    spawn_fake_upstream(move |req: Request<hyper::body::Incoming>| {
        let seen = seen.clone();
        async move {
            let body = req.into_body().collect().await.unwrap().to_bytes();
            seen.lock().unwrap().push(body.to_vec());
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(full_body(ANTHROPIC_SSE))
                .unwrap()
        }
    })
    .await
}

async fn relay_with(upstream: SocketAddr, tweak: impl FnOnce(&mut Config)) -> (SocketAddr, tempfile::TempDir) {
    let log_dir = tempfile::tempdir().unwrap();
    let mut cfg = Config { log_dir: Some(log_dir.path().to_path_buf()), ..Config::default() };
    cfg.state_dir = Some(log_dir.path().join("state"));
    cfg.routes.push(RouteConfig { name: "test".into(), upstream: format!("http://{upstream}") });
    tweak(&mut cfg);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = Arc::new(cfg);
    let engine = Engine::new(cfg.clone());
    tokio::spawn(async move {
        let _ = ashkelon::relay::serve(cfg, listener, engine).await;
    });
    (addr, log_dir)
}

async fn post(relay: SocketAddr, body: &str) -> (StatusCode, Bytes) {
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    let status = resp.status();
    (status, resp.into_body().collect().await.unwrap().to_bytes())
}

const SIMPLE: &str = r#"{"model":"claude-x","max_tokens":1000,"messages":[{"role":"user","content":"hi"}]}"#;

#[tokio::test]
async fn call_record_carries_parsed_usage() {
    let seen = Seen::default();
    let (relay, log_dir) = relay_with(upstream_recording(seen.clone()).await, |_| {}).await;
    let (status, body) = post(relay, SIMPLE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), ANTHROPIC_SSE.as_bytes());
    assert_eq!(seen.lock().unwrap()[0], SIMPLE.as_bytes());

    let record = &wait_for_records(log_dir.path(), 1).await[0];
    assert_eq!(record["model"], "claude-x");
    assert_eq!(record["stop_reason"], "end_turn");
    assert_eq!(record["turn_end"], true);
    assert_eq!(record["usage"]["input_tokens"], 12);
    assert_eq!(record["usage"]["cache_read_tokens"], 100);
    assert_eq!(record["usage"]["output_tokens"], 17);
    assert!(record["ttfb_ms"].is_u64());
    assert_eq!(record["transforms"], serde_json::json!([]));
}

#[tokio::test]
async fn disallowed_model_is_rejected_without_calling_upstream() {
    let seen = Seen::default();
    let (relay, log_dir) =
        relay_with(upstream_recording(seen.clone()).await, |c| c.rules.allow_models = vec!["^gpt-".into()]).await;
    let (status, body) = post(relay, SIMPLE).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(err["type"], "error");
    assert!(seen.lock().unwrap().is_empty());
    let record = &wait_for_records(log_dir.path(), 1).await[0];
    assert!(record["rule"].as_str().unwrap().contains("allow_models"));
}

#[tokio::test]
async fn stream_is_cut_by_pattern_rule() {
    let seen = Seen::default();
    let (relay, log_dir) = relay_with(upstream_recording(seen.clone()).await, |c| {
        c.rules.cut_patterns = vec![CutPattern { name: "no-forty-two".into(), pattern: "forty-two".into() }]
    })
    .await;
    let (status, body) = post(relay, SIMPLE).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: error"), "cut tail missing: {text}");
    assert!(!text.contains("message_stop"), "stream continued past the cut: {text}");
    let record = &wait_for_records(log_dir.path(), 1).await[0];
    assert_eq!(record["rule"], "no-forty-two");
}

#[tokio::test]
async fn long_tool_output_is_trimmed_before_upstream() {
    let seen = Seen::default();
    let (relay, log_dir) = relay_with(upstream_recording(seen.clone()).await, |c| {
        c.transforms.tool_output = Some(ToolOutputTrim { max_chars: 100, keep_head: 10, keep_tail: 10 })
    })
    .await;
    let long = "x".repeat(5000);
    let request = serde_json::json!({
        "model": "claude-x",
        "messages": [
            {"role": "user", "content": "run it"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "sig=="},
                {"type": "tool_use", "id": "t1", "name": "sh", "input": {}}
            ]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": long}]}
        ]
    })
    .to_string();
    let (status, _) = post(relay, &request).await;
    assert_eq!(status, StatusCode::OK);
    let sent: serde_json::Value = serde_json::from_slice(&seen.lock().unwrap()[0]).unwrap();
    let out = sent["messages"][2]["content"][0]["content"].as_str().unwrap();
    assert!(out.contains("[ashkelon: trimmed 4980 chars]"));
    assert_eq!(sent["messages"][1]["content"][0]["signature"], "sig==");
    let record = &wait_for_records(log_dir.path(), 1).await[0];
    assert_eq!(record["transforms"], serde_json::json!(["tool_output"]));
}
