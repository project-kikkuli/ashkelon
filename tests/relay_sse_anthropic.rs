mod relay_support;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, read_call_records, spawn_fake_upstream, spawn_relay, wait_for_records};

const SSE_BODY: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me consider this carefully.\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"deadbeefsignature==\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello there!\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":6}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

#[tokio::test]
async fn forwards_anthropic_sse_with_thinking_and_signature_untouched() {
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(full_body(SSE_BODY))
            .unwrap()
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    let client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(
            br#"{"model":"claude-x","system":"be terse","messages":[{"role":"user","content":"hi"}]}"#,
        )))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), SSE_BODY.as_bytes());

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["route"], "test");
    assert_eq!(record["wire"], "anthropic_messages");
    assert_eq!(record["status"], 200);
    assert_eq!(record["error"], serde_json::Value::Null);

    // Directly reading it back proves nothing was buffered away by an intermediate rewrite.
    let again = read_call_records(log_dir.path());
    assert_eq!(again.len(), 1);
}
