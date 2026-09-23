mod relay_support;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, spawn_fake_upstream, spawn_relay, wait_for_records};

/// With `log_bodies` on, the pipeline must persist the exact request and response bytes for a
/// call next to its record, keyed by the record's own `call_id`.
#[tokio::test]
async fn logs_request_and_response_bodies_when_enabled() {
    const RESPONSE: &str = r#"{"type":"message","id":"msg_1"}"#;
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(full_body(RESPONSE))
            .unwrap()
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, true).await;

    const REQUEST: &[u8] = br#"{"model":"claude-x","messages":[{"role":"user","content":"hi"}]}"#;
    let client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(REQUEST)))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), RESPONSE.as_bytes());

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    let call_id = records[0]["call_id"].as_str().unwrap();

    let req_path = log_dir.path().join("bodies").join(format!("{call_id}.request"));
    let resp_path = log_dir.path().join("bodies").join(format!("{call_id}.response"));
    let logged_request = wait_for_file(&req_path).await;
    let logged_response = wait_for_file(&resp_path).await;

    assert_eq!(logged_request, REQUEST);
    assert_eq!(logged_response, RESPONSE.as_bytes());
}

async fn wait_for_file(path: &std::path::Path) -> Vec<u8> {
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(path) {
            return bytes;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    std::fs::read(path).unwrap_or_else(|e| panic!("body file {path:?} never appeared: {e}"))
}
