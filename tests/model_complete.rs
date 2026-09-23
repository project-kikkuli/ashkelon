use std::sync::{Arc, Mutex};

use ashkelon::config::{Config, ModelConfig};
use ashkelon::model::complete;

type CapturedHeaders = Arc<Mutex<Option<Vec<(String, String)>>>>;

struct FakeServer {
    addr: std::net::SocketAddr,
    captured_headers: CapturedHeaders,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start(response_body: String, status: u16) -> FakeServer {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind fake http server");
        let addr = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            tiny_http::ListenAddr::Unix(_) => unreachable!("bound a TCP address"),
        };
        let captured = Arc::new(Mutex::new(None));
        let captured2 = captured.clone();
        let handle = std::thread::spawn(move || {
            if let Ok(mut request) = server.recv() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let headers: Vec<(String, String)> = request
                    .headers()
                    .iter()
                    .map(|h| (h.field.as_str().as_str().to_string(), h.value.as_str().to_string()))
                    .collect();
                *captured2.lock().unwrap() = Some(headers);
                let response = tiny_http::Response::from_string(response_body).with_status_code(status);
                let _ = request.respond(response);
            }
        });
        FakeServer { addr, captured_headers: captured, handle: Some(handle) }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn wait_for_request(&self) {
        for _ in 0..200 {
            if self.captured_headers.lock().unwrap().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn header(&self, name: &str) -> Option<String> {
        self.wait_for_request();
        self.captured_headers.lock().unwrap().as_ref()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn cfg_with_model(model: ModelConfig) -> Config {
    Config { models: vec![model], ..Config::default() }
}

fn set_env(name: &str, value: &str) {
    // Safety: test-only, single-threaded with respect to this variable (each test uses its own
    // unique env var name), and reqwest performs no concurrent env reads during the call.
    unsafe { std::env::set_var(name, value) };
}

#[tokio::test]
async fn anthropic_extracts_text_and_sends_api_key_header() {
    let server = FakeServer::start(r#"{"content":[{"type":"text","text":"hello from anthropic"}]}"#.into(), 200);
    set_env("ASHKELON_TEST_ANTHROPIC_KEY", "sk-ant-test");
    let cfg = cfg_with_model(ModelConfig {
        name: "claude".into(),
        api: "anthropic".into(),
        base_url: server.base_url(),
        model: "claude-x".into(),
        api_key_env: Some("ASHKELON_TEST_ANTHROPIC_KEY".into()),
    });

    let out = complete(&cfg, "claude", Some("be terse"), "hi").await.unwrap();
    assert_eq!(out, "hello from anthropic");
    assert_eq!(server.header("x-api-key"), Some("sk-ant-test".to_string()));
}

#[tokio::test]
async fn anthropic_without_api_key_env_sends_no_auth_header() {
    let server = FakeServer::start(r#"{"content":[{"type":"text","text":"ok"}]}"#.into(), 200);
    let cfg = cfg_with_model(ModelConfig {
        name: "claude".into(),
        api: "anthropic".into(),
        base_url: server.base_url(),
        model: "claude-x".into(),
        api_key_env: None,
    });

    let out = complete(&cfg, "claude", None, "hi").await.unwrap();
    assert_eq!(out, "ok");
    assert!(server.header("x-api-key").is_none());
}

#[tokio::test]
async fn openai_responses_extracts_output_text_shortcut() {
    let server = FakeServer::start(r#"{"output_text":"hi from responses"}"#.into(), 200);
    set_env("ASHKELON_TEST_OPENAI_KEY", "sk-openai-test");
    let cfg = cfg_with_model(ModelConfig {
        name: "gpt".into(),
        api: "openai".into(),
        base_url: server.base_url(),
        model: "gpt-x".into(),
        api_key_env: Some("ASHKELON_TEST_OPENAI_KEY".into()),
    });

    let out = complete(&cfg, "gpt", None, "hi").await.unwrap();
    assert_eq!(out, "hi from responses");
    assert_eq!(server.header("authorization"), Some("Bearer sk-openai-test".to_string()));
}

#[tokio::test]
async fn openai_responses_extracts_from_output_array_when_no_shortcut_field() {
    let body = r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"assembled"}]}]}"#;
    let server = FakeServer::start(body.into(), 200);
    let cfg = cfg_with_model(ModelConfig {
        name: "gpt".into(),
        api: "openai".into(),
        base_url: server.base_url(),
        model: "gpt-x".into(),
        api_key_env: None,
    });

    let out = complete(&cfg, "gpt", None, "hi").await.unwrap();
    assert_eq!(out, "assembled");
}

#[tokio::test]
async fn openai_chat_extracts_message_content() {
    let server = FakeServer::start(r#"{"choices":[{"message":{"content":"hi chat"}}]}"#.into(), 200);
    set_env("ASHKELON_TEST_CHAT_KEY", "sk-chat-test");
    let cfg = cfg_with_model(ModelConfig {
        name: "local".into(),
        api: "openai_chat".into(),
        base_url: server.base_url(),
        model: "local-x".into(),
        api_key_env: Some("ASHKELON_TEST_CHAT_KEY".into()),
    });

    let out = complete(&cfg, "local", Some("system prompt"), "hi").await.unwrap();
    assert_eq!(out, "hi chat");
    assert_eq!(server.header("authorization"), Some("Bearer sk-chat-test".to_string()));
}

#[tokio::test]
async fn openai_chat_without_api_key_env_sends_no_auth_header() {
    let server = FakeServer::start(r#"{"choices":[{"message":{"content":"ok"}}]}"#.into(), 200);
    let cfg = cfg_with_model(ModelConfig {
        name: "local".into(),
        api: "openai_chat".into(),
        base_url: server.base_url(),
        model: "local-x".into(),
        api_key_env: None,
    });

    let out = complete(&cfg, "local", None, "hi").await.unwrap();
    assert_eq!(out, "ok");
    assert!(server.header("authorization").is_none());
}

#[tokio::test]
async fn unknown_model_name_is_an_error() {
    let cfg = Config::default();
    let err = complete(&cfg, "nope", None, "hi").await.unwrap_err();
    assert!(err.to_string().contains("nope"));
}

#[tokio::test]
async fn http_error_status_propagates_as_an_error() {
    let server = FakeServer::start(r#"{"error":"boom"}"#.into(), 500);
    let cfg = cfg_with_model(ModelConfig {
        name: "claude".into(),
        api: "anthropic".into(),
        base_url: server.base_url(),
        model: "claude-x".into(),
        api_key_env: None,
    });

    let err = complete(&cfg, "claude", None, "hi").await.unwrap_err();
    assert!(err.to_string().contains("500"));
}

#[tokio::test]
async fn unknown_api_kind_is_an_error() {
    let cfg = cfg_with_model(ModelConfig {
        name: "weird".into(),
        api: "carrier-pigeon".into(),
        base_url: "http://127.0.0.1:1".into(),
        model: "x".into(),
        api_key_env: None,
    });

    let err = complete(&cfg, "weird", None, "hi").await.unwrap_err();
    assert!(err.to_string().contains("carrier-pigeon"));
}
