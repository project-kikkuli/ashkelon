//! Shared test fixtures: a fake upstream and a relay instance pointed at it via a configured
//! route, both bound to ephemeral loopback ports so tests can run concurrently.
#![allow(dead_code)]

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use ashkelon::config::{Config, RouteConfig};
use ashkelon::hooks::Engine;
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

pub type FakeBody = BoxBody<Bytes, Infallible>;

pub fn full_body(bytes: impl Into<Bytes>) -> FakeBody {
    Full::new(bytes.into()).map_err(|e: Infallible| match e {}).boxed()
}

/// Starts a bare HTTP/1.1 upstream on an ephemeral loopback port running `handler` for every
/// request. Detached: it lives for the test process's lifetime, same as the relay under test.
pub async fn spawn_fake_upstream<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<FakeBody>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake upstream");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let io = TokioIo::new(stream);
            let handler = handler.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let handler = handler.clone();
                    async move { Ok::<_, Infallible>(handler(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
    addr
}

/// Starts ashkelon's relay on an ephemeral loopback port, with a single configured route named
/// `test` pointed at `upstream`. Returns the relay's address and its log directory (kept alive by
/// the caller for the life of the test).
pub async fn spawn_relay(upstream: SocketAddr, log_bodies: bool) -> (SocketAddr, tempfile::TempDir) {
    let log_dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = Config {
        log_dir: Some(log_dir.path().to_path_buf()),
        log_bodies,
        ..Config::default()
    };
    cfg.routes.push(RouteConfig { name: "test".to_string(), upstream: format!("http://{upstream}") });

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind relay");
    let addr = listener.local_addr().unwrap();
    let cfg = Arc::new(cfg);
    let engine = Engine::new(cfg.clone());
    tokio::spawn(async move {
        let _ = ashkelon::relay::serve(cfg, listener, engine).await;
    });
    (addr, log_dir)
}

/// One line per call in `log_dir`'s daily file, oldest first.
pub fn read_call_records(log_dir: &std::path::Path) -> Vec<serde_json::Value> {
    let date = time::OffsetDateTime::now_utc().date();
    let filename =
        format!("calls-{:04}-{:02}-{:02}.jsonl", date.year(), u8::from(date.month()), date.day());
    let path = log_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents.lines().map(|l| serde_json::from_str(l).unwrap()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Polls `read_call_records` until at least `n` records are present or the deadline passes.
pub async fn wait_for_records(log_dir: &std::path::Path, n: usize) -> Vec<serde_json::Value> {
    for _ in 0..100 {
        let records = read_call_records(log_dir);
        if records.len() >= n {
            return records;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    read_call_records(log_dir)
}

/// A raw HTTP/1.1 client connection the test drives directly, so it can force-close the socket
/// (by aborting the connection driver) to simulate a client disconnecting mid-response.
pub struct RawClient {
    pub sender: SendRequest<Full<Bytes>>,
    driver: JoinHandle<()>,
}

impl RawClient {
    pub async fn connect(addr: SocketAddr) -> RawClient {
        let stream = TcpStream::connect(addr).await.expect("connect to relay");
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await.expect("handshake");
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
        RawClient { sender, driver }
    }

    /// Force-closes the underlying socket, as a client disconnecting abruptly would.
    pub fn disconnect(self) {
        self.driver.abort();
    }
}
