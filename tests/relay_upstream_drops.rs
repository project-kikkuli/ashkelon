mod relay_support;

use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use relay_support::{full_body, spawn_relay, spawn_relay_to_url, wait_for_records, RawClient};

async fn post(relay: SocketAddr) -> Result<Response<hyper::body::Incoming>, hyper_util::client::legacy::Error> {
    let client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{relay}/test/v1/messages"))
        .body(Full::new(Bytes::from_static(b"{}")))
        .unwrap();
    client.request(req).await
}

async fn read_request_head(socket: &mut tokio::net::TcpStream) {
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0, "relay closed before sending the request head");
        seen.extend_from_slice(&buf[..n]);
    }
}

#[allow(deprecated)] // zero-linger close sends an RST, which is the failure under test
fn reset(socket: tokio::net::TcpStream) {
    socket.set_linger(Some(Duration::ZERO)).unwrap();
    drop(socket);
}

/// Spawns an upstream that reads each request head and then runs `respond` on the socket.
async fn raw_upstream<F, Fut>(respond: F) -> SocketAddr
where
    F: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let respond = respond.clone();
            tokio::spawn(async move {
                read_request_head(&mut socket).await;
                respond(socket).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn upstream_that_fails_before_any_body_byte_gets_a_502_not_a_200() {
    const HEADERS: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 500\r\n\r\n";
    let cases: [(&str, Option<&'static [u8]>); 2] = [
        ("upstream closed before response", None),
        ("upstream failed before response body", Some(HEADERS)),
    ];
    for (expected_kind, head) in cases {
        let upstream = raw_upstream(move |mut socket| async move {
            if let Some(head) = head {
                socket.write_all(head).await.unwrap();
                socket.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            reset(socket);
        })
        .await;
        let (relay, log_dir) = spawn_relay(upstream, false).await;

        let resp = post(relay).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY, "{expected_kind}");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains(expected_kind), "body: {body:?}");

        let records = wait_for_records(log_dir.path(), 1).await;
        assert_eq!(records[0]["status"], 502);
        assert!(records[0]["error"].as_str().unwrap().starts_with(expected_kind));
    }
}

#[tokio::test]
async fn upstream_reset_mid_body_aborts_the_client_stream_instead_of_ending_it() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&vec![b'a'; 64 * 1024]).unwrap();
    let full = gz.finish().unwrap();
    let partial = full[..full.len() / 2].to_vec();
    let declared = full.len();

    let upstream = raw_upstream(move |mut socket| {
        let partial = partial.clone();
        async move {
            let head = format!("HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {declared}\r\n\r\n");
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&partial).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            reset(socket);
        }
    })
    .await;
    let (relay, log_dir) = spawn_relay(upstream, false).await;

    let resp = post(relay).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.into_body().collect().await.is_err(),
        "a truncated gzip body must surface as a transport error, not a clean end"
    );

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records[0]["status"], 200);
    assert!(records[0]["error"]
        .as_str()
        .unwrap()
        .starts_with("upstream dropped mid-body"));
}

#[tokio::test]
async fn a_refused_first_connect_is_retried_once_and_the_request_is_sent_exactly_once() {
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = reserved.local_addr().unwrap();
    drop(reserved);
    let (relay, log_dir) = spawn_relay(upstream_addr, false).await;

    let request = tokio::spawn(async move { post(relay).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let listener = TcpListener::bind(upstream_addr).await.unwrap();
    tokio::spawn(async move {
        use hyper::service::service_fn;
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let counter = counter.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |_req: Request<hyper::body::Incoming>| {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async move { Ok::<_, std::convert::Infallible>(Response::new(full_body("ok"))) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), svc)
                    .await;
            });
        }
    });

    let resp = request.await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes().as_ref(), b"ok");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records[0]["status"], 200);
}

#[tokio::test]
async fn connect_that_fails_twice_is_a_502_logged_as_a_connect_failure_after_retry() {
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = reserved.local_addr().unwrap();
    drop(reserved);
    let (relay, log_dir) = spawn_relay(dead, false).await;

    let resp = post(relay).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let records = wait_for_records(log_dir.path(), 1).await;
    assert!(records[0]["error"]
        .as_str()
        .unwrap()
        .starts_with("upstream connect failed after retry"));
}

#[tokio::test]
async fn a_stalled_tls_handshake_times_out_and_is_retried_as_a_connect_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            held.push(socket);
        }
    });
    let (relay, log_dir, _engine) = spawn_relay_to_url(format!("https://{addr}"), false).await;

    let started = std::time::Instant::now();
    let resp = post(relay).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2, "one retry");

    let records = wait_for_records(log_dir.path(), 1).await;
    let error = records[0]["error"].as_str().unwrap();
    assert!(error.starts_with("upstream connect failed after retry"), "{error}");
    assert!(error.contains("timed out"), "{error}");
}

#[tokio::test]
async fn a_client_that_leaves_while_the_first_byte_is_pending_is_still_logged() {
    let upstream = raw_upstream(|mut socket| async move {
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n")
            .await
            .unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(socket);
    })
    .await;
    let (relay, log_dir) = spawn_relay(upstream, false).await;

    let mut client = RawClient::connect(relay).await;
    let req = Request::builder()
        .method("POST")
        .uri("/test/v1/messages")
        .body(Full::new(Bytes::from_static(b"{}")))
        .unwrap();
    let pending = tokio::spawn(client.sender.send_request(req));
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.disconnect();
    pending.abort();

    let records = wait_for_records(log_dir.path(), 1).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["error"], "client disconnected before response");
}
