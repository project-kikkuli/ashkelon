mod relay_support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, spawn_fake_upstream, spawn_relay, wait_for_records};

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn zstd_compress(data: &[u8]) -> Vec<u8> {
    zstd::encode_all(data, 0).unwrap()
}

/// The response tee decodes a copy for observation but must never alter what the client receives:
/// a gzip-compressed upstream response reaches the agent byte-for-byte, still gzipped.
#[tokio::test]
async fn gzip_response_reaches_client_unchanged() {
    let plain = br#"{"type":"message","id":"msg_1","content":[{"type":"text","text":"hello"}]}"#;
    let compressed = gzip(plain);
    let compressed_for_upstream = compressed.clone();

    let upstream = spawn_fake_upstream(move |_req: hyper::Request<hyper::body::Incoming>| {
        let compressed = compressed_for_upstream.clone();
        async move {
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-encoding", "gzip")
                .body(full_body(compressed))
                .unwrap()
        }
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(br#"{"model":"claude-x","messages":[]}"#)))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.headers().get("content-encoding").unwrap(), "gzip");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), compressed.as_slice());

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], 200);
}

/// With no transform or ping registered (the current stub state), a compressed request body must
/// be forwarded exactly as the agent sent it — ashkelon only decodes a side copy for inspection.
#[tokio::test]
async fn zstd_request_body_passed_untouched() {
    let plain = br#"{"model":"claude-x","messages":[{"role":"user","content":"hi"}]}"#;
    let compressed = zstd_compress(plain);

    type Received = Arc<Mutex<Option<(Vec<u8>, Option<String>)>>>;
    let received: Received = Arc::new(Mutex::new(None));
    let received_for_handler = received.clone();
    let upstream = spawn_fake_upstream(move |req: hyper::Request<hyper::body::Incoming>| {
        let received = received_for_handler.clone();
        async move {
            let encoding = req.headers().get("content-encoding").and_then(|v| v.to_str().ok()).map(str::to_string);
            let body = req.into_body().collect().await.unwrap().to_bytes().to_vec();
            *received.lock().unwrap() = Some((body, encoding));
            Response::builder().status(StatusCode::OK).body(full_body(Bytes::from_static(b"{}"))).unwrap()
        }
    })
    .await;
    let (relay_addr, log_dir) = spawn_relay(upstream, false).await;

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay_addr}/test/v1/messages"))
        .header("content-type", "application/json")
        .header("content-encoding", "zstd")
        .body(Full::new(Bytes::from(compressed.clone())))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = resp.into_body().collect().await.unwrap().to_bytes();

    wait_for_records(log_dir.path(), 1).await;

    let (received_body, received_encoding) = received.lock().unwrap().clone().expect("upstream received a request");
    assert_eq!(received_body, compressed, "request body must reach upstream byte-for-byte when untouched");
    assert_eq!(received_encoding.as_deref(), Some("zstd"), "content-encoding must survive when the body is untouched");
}
