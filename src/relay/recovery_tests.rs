use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use h2::Reason;
use hyper::service::service_fn;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

use crate::config::{Config, RouteConfig};
use crate::hooks::Engine;
use crate::telemetry::{CallRecord, Writer};

use super::{pipeline, Tracker};

struct Upstream {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<(usize, String)>>>,
    finish_held: Arc<Notify>,
    reset_stream: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Upstream {
    async fn start(reason: Reason, after_headers: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let finish_held = Arc::new(Notify::new());
        let reset_stream = Arc::new(Notify::new());
        let counter = connections.clone();
        let observed = requests.clone();
        let held = finish_held.clone();
        let reset = reset_stream.clone();
        let task = tokio::spawn(async move {
            let mut peers = JoinSet::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let connection_id = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let observed = observed.clone();
                let held = held.clone();
                let reset = reset.clone();
                peers.spawn(async move {
                    let mut connection = h2::server::handshake(socket).await.unwrap();
                    let mut streams = JoinSet::new();
                    while let Some(Ok((request, mut response))) = connection.accept().await {
                        let path = request.uri().path().to_string();
                        observed.lock().unwrap().push((connection_id, path.clone()));
                        let held = held.clone();
                        let reset = reset.clone();
                        streams.spawn(async move {
                            let mut body = request.into_body();
                            while let Some(chunk) = body.data().await {
                                let chunk = chunk.unwrap();
                                body.flow_control().release_capacity(chunk.len()).unwrap();
                            }
                            let failing = connection_id == 1 && path == "/reset";
                            if failing && !after_headers {
                                response.send_reset(reason);
                                return;
                            }
                            let status = match path.as_str() {
                                "/invalid" => 400,
                                "/limited" => 429,
                                _ => 200,
                            };
                            let headers = http::Response::builder()
                                .status(status)
                                .header("x-test-connection", connection_id.to_string())
                                .body(())
                                .unwrap();
                            let mut stream = response.send_response(headers, false).unwrap();
                            if failing {
                                stream.send_data(Bytes::from_static(b"partial"), false).unwrap();
                                reset.notified().await;
                                stream.send_reset(reason);
                            } else if path == "/held" {
                                stream.send_data(Bytes::from_static(b"head"), false).unwrap();
                                held.notified().await;
                                stream.send_data(Bytes::from_static(b"tail"), true).unwrap();
                            } else {
                                stream.send_data(Bytes::from_static(b"ok"), true).unwrap();
                            }
                        });
                    }
                });
            }
        });
        Self {
            address,
            connections,
            requests,
            finish_held,
            reset_stream,
            task,
        }
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn retires_only_protocol_failures_without_replaying_or_interrupting_other_streams() {
    for (reason, after_headers, retired) in [
        (Reason::PROTOCOL_ERROR, false, true),
        (Reason::PROTOCOL_ERROR, true, true),
        (Reason::CANCEL, false, false),
        (Reason::CANCEL, true, false),
        (Reason::REFUSED_STREAM, false, false),
    ] {
        tokio::time::timeout(Duration::from_secs(10), recovery_case(reason, after_headers, retired))
            .await
            .unwrap_or_else(|_| panic!("timed out: {reason:?}, after_headers={after_headers}"));
    }
}

async fn recovery_case(reason: Reason, after_headers: bool, retired: bool) {
    let upstream = Upstream::start(reason, after_headers).await;
    let other_upstream = Upstream::start(reason, after_headers).await;
    let log_dir = tempfile::tempdir().unwrap();
    let cfg = Arc::new(Config {
        log_dir: Some(log_dir.path().to_path_buf()),
        state_dir: Some(log_dir.path().join("state")),
        routes: vec![
            RouteConfig {
                name: "test".into(),
                upstream: format!("http://{}", upstream.address),
            },
            RouteConfig {
                name: "other".into(),
                upstream: format!("http://{}", other_upstream.address),
            },
        ],
        ..Config::default()
    });
    let engine = Engine::new(cfg.clone());
    let telemetry = Arc::new(Writer::new(cfg.log_dir()).unwrap());
    let tracker = Arc::new(Tracker::default());
    let connector = HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http2()
        .build();
    let upstream_client = Client::builder(TokioExecutor::new()).http2_only(true).build(connector);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_address = listener.local_addr().unwrap();
    let relay_tracker = tracker.clone();
    let relay_task = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let cfg = cfg.clone();
            let engine = engine.clone();
            let telemetry = telemetry.clone();
            let tracker = relay_tracker.clone();
            let client = upstream_client.clone();
            clients.spawn(async move {
                let service = service_fn(move |request| {
                    pipeline::handle(
                        request,
                        cfg.clone(),
                        engine.clone(),
                        client.clone(),
                        telemetry.clone(),
                        tracker.clone(),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    let client = reqwest::Client::new();
    let url = |route: &str, path: &str| format!("http://{relay_address}/{route}/{path}");
    let mut held = client.get(url("test", "held")).send().await.unwrap();
    assert_eq!(held.chunk().await.unwrap().unwrap(), "head");
    for route in ["test", "other"] {
        let response = client.get(url(route, "probe")).send().await.unwrap();
        assert_eq!(response.headers()["x-test-connection"], "1");
        assert_eq!(response.text().await.unwrap(), "ok");
    }
    for (path, status) in [("invalid", 400), ("limited", 429)] {
        let response = client.get(url("test", path)).send().await.unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()["x-test-connection"], "1");
        assert_eq!(response.text().await.unwrap(), "ok");
    }
    let mut failure = client.post(url("test", "reset")).body("{}").send().await.unwrap();
    if after_headers {
        assert_eq!(failure.status(), 200);
        assert_eq!(failure.chunk().await.unwrap().unwrap(), "partial");
        upstream.reset_stream.notify_one();
        assert!(failure.chunk().await.unwrap().is_none());
    } else {
        assert_eq!(failure.status(), 502);
        assert!(failure.text().await.unwrap().contains("stream error received"));
    }
    let response = client.get(url("test", "probe")).send().await.unwrap();
    assert_eq!(
        response.headers()["x-test-connection"],
        if retired { "2" } else { "1" },
        "{reason:?}, after_headers={after_headers}"
    );
    assert_eq!(response.text().await.unwrap(), "ok");
    let response = client.get(url("other", "probe")).send().await.unwrap();
    assert_eq!(response.headers()["x-test-connection"], "1");
    assert_eq!(response.text().await.unwrap(), "ok");
    upstream.finish_held.notify_one();
    assert_eq!(held.chunk().await.unwrap().unwrap(), "tail");
    assert!(held.chunk().await.unwrap().is_none());
    assert_eq!(upstream.connections.load(Ordering::SeqCst), if retired { 2 } else { 1 });
    assert_eq!(other_upstream.connections.load(Ordering::SeqCst), 1);
    assert_eq!(
        upstream
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path)| path == "/reset")
            .count(),
        1,
        "the failed POST is never replayed"
    );
    tracker.wait_idle(Duration::from_secs(5)).await;
    let records: Vec<CallRecord> = std::fs::read_dir(log_dir.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jsonl"))
        .flat_map(|entry| {
            std::fs::read_to_string(entry.path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<CallRecord>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(records.len(), 8);
    let failure = records.iter().find(|record| record.path == "/test/reset").unwrap();
    assert_eq!(failure.connection_retired, retired);
    assert!(failure.error.as_ref().unwrap().contains("stream error received"));
    assert!(records
        .iter()
        .filter(|record| record.path != "/test/reset")
        .all(|record| { !record.connection_retired && record.error.is_none() }));
    relay_task.abort();
}
