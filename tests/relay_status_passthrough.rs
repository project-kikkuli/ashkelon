mod relay_support;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, spawn_fake_upstream, spawn_relay, wait_for_records};

const ERROR_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"model not found"}}"#;

#[tokio::test]
async fn passes_through_4xx_status_and_body_unchanged() {
    let upstream = spawn_fake_upstream(|_req: Request<hyper::body::Incoming>| async move {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("content-type", "application/json")
            .body(full_body(ERROR_BODY))
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
        .body(Full::new(Bytes::from_static(br#"{"model":"nope","messages":[]}"#)))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), ERROR_BODY.as_bytes());

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], 404);
}
