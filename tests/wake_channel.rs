//! Exercises the real `ashkelon channel --socket <path>` subcommand as Claude Code itself would:
//! a JSON-RPC line-delimited stdio MCP client sends the handshake, then a line written to the
//! control socket must arrive as a `notifications/claude/channel` push on stdout. This spawns
//! ashkelon's own binary (never a real harness), so it carries no credential risk.
use std::io::Write;
use std::process::Stdio;

use serde_json::{json, Value};

struct Child {
    process: std::process::Child,
    stdout: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
}

impl Child {
    fn spawn(socket_path: &std::path::Path) -> Child {
        let mut process = std::process::Command::new(env!("CARGO_BIN_EXE_ashkelon"))
            .args(["channel", "--socket"])
            .arg(socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawning ashkelon channel");
        let stdout = std::io::BufReader::new(process.stdout.take().unwrap());
        use std::io::BufRead;
        Child { process, stdout: stdout.lines() }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.process.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
    }

    fn recv(&mut self) -> Value {
        let line = self.stdout.next().expect("stream ended").expect("reading stdout line");
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Short: a unix socket path is capped at ~104 bytes (`sockaddr_un.sun_path`), and
/// `std::env::temp_dir()` on macOS is already a long `/var/folders/.../T/` path.
fn socket_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ash-ch-{}-{n}", std::process::id()))
}

#[test]
fn handshake_declares_the_channel_capability() {
    let dir = socket_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("test.sock");
    let mut child = Child::spawn(&socket_path);

    child.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2026-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } },
    }));
    let response = child.recv();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], "2026-06-18");
    assert_eq!(response["result"]["capabilities"]["experimental"]["claude/channel"], json!({}));
    assert_eq!(response["result"]["serverInfo"]["name"], "ashkelon");

    child.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    child.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }));
    assert_eq!(child.recv(), json!({ "jsonrpc": "2.0", "id": 2, "result": {} }));

    child.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }));
    assert_eq!(child.recv(), json!({ "jsonrpc": "2.0", "id": 3, "result": { "tools": [] } }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_request_gets_a_method_not_found_error() {
    let dir = socket_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("test.sock");
    let mut child = Child::spawn(&socket_path);

    child.send(&json!({ "jsonrpc": "2.0", "id": 9, "method": "something/unsupported" }));
    let response = child.recv();
    assert_eq!(response["id"], 9);
    assert_eq!(response["error"]["code"], -32601);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn socket_line_is_relayed_as_a_channel_notification() {
    let dir = socket_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("test.sock");
    let mut child = Child::spawn(&socket_path);

    // Wait for the socket file to exist before connecting (the server creates it right away,
    // but a fresh subprocess needs a moment to schedule).
    for _ in 0..200 {
        if socket_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(socket_path.exists(), "socket was never created");

    let mut stream = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();
    writeln!(stream, "hello from the wake adapter").unwrap();

    let notification = child.recv();
    assert_eq!(notification["method"], "notifications/claude/channel");
    assert_eq!(notification["params"]["content"], "hello from the wake adapter");

    let _ = std::fs::remove_dir_all(&dir);
}
