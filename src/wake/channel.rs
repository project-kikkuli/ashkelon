use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};

/// Runs the stdio MCP server ashkelon launches itself as (Claude Code spawns it per
/// `--mcp-config`, per `launch::claude::plan`). Speaks just enough MCP to satisfy Claude's
/// handshake — `initialize` (declaring `experimental: {"claude/channel": {}}`, the capability
/// key Claude's own gate checks for — confirmed via `strings` on the installed binary),
/// `notifications/initialized`, `ping`, `tools/list` (always empty) — then relays each line read
/// from `socket_path` onto stdout as an unsolicited `notifications/claude/channel` push. The
/// notification shape (`{method, params: {content, meta?}}`) is likewise pinned from the
/// installed binary's embedded schema. Returns once stdin closes (Claude tore the subprocess
/// down).
pub async fn run(socket_path: &Path) -> anyhow::Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let tx = tx.clone();
            tokio::spawn(relay_socket_lines(stream, tx));
        }
    });

    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let notify_stdout = stdout.clone();
    let notify_task = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/claude/channel",
                "params": { "content": line },
            });
            let mut out = notify_stdout.lock().await;
            let _ = write_line(&mut *out, &notification).await;
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(response) = handle_request(&message) {
            let mut out = stdout.lock().await;
            write_line(&mut *out, &response).await?;
        }
    }

    accept_task.abort();
    notify_task.abort();
    Ok(())
}

async fn relay_socket_lines(stream: UnixStream, tx: mpsc::UnboundedSender<String>) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).is_err() {
            break;
        }
    }
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(out: &mut W, value: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    out.write_all(&bytes).await?;
    out.flush().await?;
    Ok(())
}

/// Returns `Some(response)` for a request that needs one (carries an `id`); `None` for
/// notifications (`notifications/initialized`) and anything malformed.
fn handle_request(message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method")?.as_str()?;
    match method {
        "initialize" => {
            // Echoing the client's requested version back is a spec-compliant negotiation
            // (the alternative is pinning a literal date string scraped from `strings`, which
            // would need updating every time Claude Code bumps its protocol revision); this
            // server only ever speaks the tiny fixed subset below, which is stable across
            // revisions.
            let protocol_version =
                message.get("params").and_then(|p| p.get("protocolVersion")).and_then(|v| v.as_str()).unwrap_or("2026-06-18");
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": protocol_version,
                    "serverInfo": { "name": "ashkelon", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": {
                        "tools": {},
                        "experimental": { "claude/channel": {} },
                    },
                    "instructions": "Relays lines written to a local unix socket into this session as channel events.",
                },
            }))
        }
        "ping" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
        "tools/list" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [] } })),
        "notifications/initialized" => None,
        _ if id.is_some() => {
            Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } }))
        }
        _ => None,
    }
}
