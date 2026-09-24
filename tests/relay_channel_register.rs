//! `POST /internal/claude-channel`: the wire contract a serve-mode `ashkelon channel` process
//! relies on to tell the daemon which socket to reach a Claude Code session through
//! (`hooks::Engine::register_claude_channel`, exercised directly — including its session-state
//! side effects — by `hooks::tests` in `src/hooks/mod.rs`).
mod relay_support;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};

use relay_support::{full_body, spawn_fake_upstream, spawn_relay_with_engine};

async fn post(addr: std::net::SocketAddr, path: &str, body: &str) -> (StatusCode, String) {
    let client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{addr}{path}"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    let status = resp.status();
    let text = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    (status, text)
}

#[tokio::test]
async fn valid_registration_is_accepted() {
    let upstream =
        spawn_fake_upstream(|_req| async move { Response::builder().status(200).body(full_body("{}")).unwrap() }).await;
    let (addr, log_dir, _engine) = spawn_relay_with_engine(upstream, false).await;

    let socket = log_dir.path().join("state").join("channel").join("sess-1.sock");
    let (status, body) = post(
        addr,
        "/internal/claude-channel",
        &format!(r#"{{"session_id":"sess-1","socket":"{}"}}"#, socket.display()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"ok":true}"#);
}

#[tokio::test]
async fn registration_outside_the_state_dir_is_rejected() {
    let upstream =
        spawn_fake_upstream(|_req| async move { Response::builder().status(200).body(full_body("{}")).unwrap() }).await;
    let (addr, _log_dir, _engine) = spawn_relay_with_engine(upstream, false).await;

    let (status, _body) = post(
        addr,
        "/internal/claude-channel",
        r#"{"session_id":"sess-1","socket":"/tmp/sess-1.sock"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn registration_with_a_relative_socket_path_is_rejected() {
    let upstream =
        spawn_fake_upstream(|_req| async move { Response::builder().status(200).body(full_body("{}")).unwrap() }).await;
    let (addr, _log_dir, _engine) = spawn_relay_with_engine(upstream, false).await;

    let (status, _body) = post(
        addr,
        "/internal/claude-channel",
        r#"{"session_id":"sess-1","socket":"sess-1.sock"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn malformed_registration_is_rejected_without_touching_the_route_table() {
    let upstream =
        spawn_fake_upstream(|_req| async move { Response::builder().status(200).body(full_body("{}")).unwrap() }).await;
    let (addr, _log_dir, _engine) = spawn_relay_with_engine(upstream, false).await;

    let (status, _body) = post(addr, "/internal/claude-channel", r#"{"socket":"/tmp/x.sock"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_to_the_registration_path_is_not_handled_as_registration() {
    // Only POST is special-cased; GET falls through to normal route resolution and 404s like any
    // other unconfigured path (`/internal` is not a route name).
    let upstream =
        spawn_fake_upstream(|_req| async move { Response::builder().status(200).body(full_body("{}")).unwrap() }).await;
    let (addr, _log_dir, _engine) = spawn_relay_with_engine(upstream, false).await;

    let client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<Full<Bytes>>();
    let req = Request::builder()
        .method("GET")
        .uri(format!("http://{addr}/internal/claude-channel"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
