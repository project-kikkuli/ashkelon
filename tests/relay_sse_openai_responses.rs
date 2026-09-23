mod relay_support;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, spawn_fake_upstream, spawn_relay, wait_for_records};

const SSE_BODY: &str = "event: response.output_item.added\n\
data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"enc_abcdef0123\",\"summary\":[]}}\n\n\
event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"enc_abcdef0123\",\"summary\":[]}}\n\n\
event: response.output_item.added\n\
data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[]}}\n\n\
event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"delta\":\"Hi there\"}\n\n\
event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hi there\"}]}}\n\n\
event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":8,\"output_tokens\":3}}}\n\n";

#[tokio::test]
async fn forwards_openai_responses_sse_with_encrypted_reasoning_untouched() {
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(full_body(SSE_BODY))
            .unwrap()
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/responses"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(
            br#"{"model":"gpt-x","instructions":"be terse","input":[{"role":"user","content":"hi"}]}"#,
        )))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), SSE_BODY.as_bytes());

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["wire"], "open_ai_responses");
    assert_eq!(record["status"], 200);
}
