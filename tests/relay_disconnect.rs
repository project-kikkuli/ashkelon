mod relay_support;

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode};

use relay_support::{spawn_fake_upstream, spawn_relay, RawClient};

/// A slow SSE stream: five chunks, one every 60ms, giving the test room to disconnect partway
/// through and still have the relay notice before the stream would have finished on its own.
fn slow_sse_body() -> BoxBody<Bytes, Infallible> {
    let s = stream::unfold(0u32, |i| async move {
        if i >= 5 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        let chunk = Bytes::from(format!("event: message_delta\ndata: {{\"i\":{i}}}\n\n"));
        Some((Ok::<_, Infallible>(Frame::data(chunk)), i + 1))
    });
    BoxBody::new(StreamBody::new(s))
}

#[tokio::test]
async fn client_disconnect_mid_stream_still_produces_a_call_record() {
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(slow_sse_body())
            .unwrap()
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    let mut client = RawClient::connect(relay_addr).await;
    let req = Request::builder()
        .method("POST")
        .uri("/test/v1/messages")
        .header("host", relay_addr.to_string())
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(br#"{"model":"claude-x","messages":[]}"#)))
        .unwrap();
    let resp = client.sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let mut body = resp.into_body();
    let first = body.frame().await;
    assert!(first.is_some(), "expected at least one chunk before disconnecting");

    // Simulate the agent process vanishing mid-response.
    drop(body);
    client.disconnect();

    let mut records = Vec::new();
    for _ in 0..150 {
        records = relay_support::read_call_records(log_dir.path());
        if !records.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(records.len(), 1, "a disconnected call must still be recorded exactly once");
    let record = &records[0];
    assert_eq!(record["status"], 200);
    let error = record["error"].as_str().unwrap_or_default();
    assert!(!error.is_empty(), "a mid-stream disconnect must be recorded as an error, got: {record}");
}
