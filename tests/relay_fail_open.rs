//! Ashkelon's own optional work (transforms, response parsing, telemetry) must never cost the
//! agent its model access. These prove the fail-open behavior in `relay::pipeline` against the
//! same fake-provider harness the rest of the relay tests use, using the `test-util`-gated fault
//! injection switches in `ashkelon::relay::test_hooks` (never present in a normal build).

mod relay_support;

use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use ashkelon::relay::test_hooks;
use bytes::Bytes;
use futures_util::stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, spawn_fake_upstream, spawn_relay, wait_for_records};

fn http_client() -> hyper_util::client::legacy::Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>
{
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http()
}

/// The `test_hooks` fault-injection flags are process-global, but every response of the
/// `anthropic_messages` wire (whatever its own test intends to exercise) runs through the same
/// "response parser" fail-open step in `forward_response`. Without serializing these tests, one
/// test's request can race another's and consume the wrong flag. This lock scopes that
/// serialization to just this file's tests; it's async-aware so holding it across an `.await` is fine.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn transform_panic_still_relays_the_upstream_response_unmodified() {
    let _guard = TEST_LOCK.lock().await;
    let seen_request: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let seen_request_bg = seen_request.clone();
    let upstream = spawn_fake_upstream(move |req: Request<hyper::body::Incoming>| {
        let seen_request = seen_request_bg.clone();
        async move {
            let body = req.into_body().collect().await.unwrap().to_bytes();
            *seen_request.lock().unwrap() = Some(body.to_vec());
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(full_body(r#"{"type":"message","id":"msg_1"}"#))
                .unwrap()
        }
    })
    .await;
    let (relay_addr, _log_dir) = spawn_relay(upstream, false).await;

    const REQUEST: &[u8] = br#"{"model":"claude-x","messages":[{"role":"user","content":"hi"}]}"#;
    test_hooks::PANIC_IN_TRANSFORM.store(true, Ordering::SeqCst);

    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(REQUEST)))
        .unwrap();
    let resp = http_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        body.as_ref(),
        br#"{"type":"message","id":"msg_1"}"#,
        "the agent must still get the upstream's own response back"
    );

    // The flag auto-clears when the injected panic actually fires, so this proves the panicking
    // code path was reached (not just that the call happened to succeed some other way).
    assert!(
        !test_hooks::PANIC_IN_TRANSFORM.load(Ordering::SeqCst),
        "the transform panic never fired"
    );
    assert_eq!(
        seen_request.lock().unwrap().as_deref(),
        Some(REQUEST),
        "the upstream must receive the request unmodified when the transform step panics"
    );
}

const SSE_CHUNKS: &[&str] = &[
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello there!\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
];

fn chunked_sse_body() -> BoxBody<Bytes, Infallible> {
    let frames: Vec<Result<Frame<Bytes>, Infallible>> = SSE_CHUNKS
        .iter()
        .map(|chunk| Ok(Frame::data(Bytes::from_static(chunk.as_bytes()))))
        .collect();
    BoxBody::new(StreamBody::new(stream::iter(frames)))
}

#[tokio::test]
async fn parser_panic_mid_stream_still_delivers_the_full_response() {
    let _guard = TEST_LOCK.lock().await;
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(chunked_sse_body())
            .unwrap()
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    test_hooks::PANIC_IN_PARSER.store(true, Ordering::SeqCst);

    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(br#"{"model":"claude-x","messages":[]}"#)))
        .unwrap();
    let resp = http_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let expected: String = SSE_CHUNKS.concat();
    assert_eq!(
        body.as_ref(),
        expected.as_bytes(),
        "the full stream must still reach the agent byte-for-byte despite the parser panicking mid-stream"
    );

    assert!(
        !test_hooks::PANIC_IN_PARSER.load(Ordering::SeqCst),
        "the parser panic never fired"
    );

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    let error = records[0]["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("panicked"),
        "the call record should note the parser panic, got: {:?}",
        records[0]
    );
}

#[tokio::test]
async fn telemetry_dir_unwritable_still_relays_the_call() {
    let _guard = TEST_LOCK.lock().await;
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(full_body(r#"{"ok":true}"#))
            .unwrap()
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    // The relay's `Writer` already exists (created at `serve()` startup); breaking the directory
    // afterward exercises a write failure, not a missing directory.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(log_dir.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    }

    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(br#"{"model":"claude-x","messages":[]}"#)))
        .unwrap();
    let resp = http_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), br#"{"ok":true}"#);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(log_dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
