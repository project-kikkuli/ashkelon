use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};

/// How this process learns which unix socket to bind.
pub enum ChannelMode {
    /// `ashkelon run claude` (`launch::claude::plan`) already knows the exact per-launch path and
    /// has told the engine about it directly (`Engine::register_launch`); this process just binds
    /// it.
    Fixed(PathBuf),
    /// `ashkelon install`'s Claude Code MCP registration: a single, fixed command line reused by
    /// every Claude Code session, so it can't carry a per-session path as an argument. This
    /// process instead mints its own socket under `socket_dir` and tells the daemon about it by
    /// POSTing to `register_url`, keyed by its own `CLAUDE_CODE_SESSION_ID` — which is exactly the
    /// session id Claude Code puts on the wire (`metadata.user_id.session_id`), confirmed by
    /// direct comparison against a live process's environment and its own request body.
    SelfRegister { socket_dir: PathBuf, register_url: String },
}

/// Runs the stdio MCP server ashkelon launches itself as (Claude Code spawns it per
/// `--mcp-config`, per `launch::claude::plan` or `install`'s persistent registration). Speaks
/// just enough MCP to satisfy Claude's handshake — `initialize` (declaring `experimental:
/// {"claude/channel": {}}`, the capability key Claude's own gate checks for — confirmed via
/// `strings` on the installed binary), `notifications/initialized`, `ping`, `tools/list` (always
/// empty) — then relays each line read from its socket onto stdout as an unsolicited
/// `notifications/claude/channel` push. The notification shape (`{method, params: {content,
/// meta?}}`) is likewise pinned from the installed binary's embedded schema. Returns once stdin
/// closes (Claude tore the subprocess down).
pub async fn run(mode: ChannelMode) -> anyhow::Result<()> {
    let socket_path = match mode {
        ChannelMode::Fixed(path) => path,
        ChannelMode::SelfRegister {
            socket_dir,
            register_url,
        } => {
            let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok().filter(|s| !s.is_empty());
            let file_stem = session_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let socket_path = socket_dir.join(format!("{file_stem}.sock"));
            match session_id {
                Some(id) => {
                    if let Err(e) = register(&register_url, &id, &socket_path).await {
                        // Non-fatal: the channel still runs and still answers Claude's MCP
                        // handshake normally; it just never receives a wake until some later
                        // registration attempt (there is none today) or restart succeeds. Wake
                        // falls back to tmux/pinning for this session in the meantime.
                        tracing::warn!(error = %e, "registering channel socket with the daemon");
                    }
                }
                None => tracing::warn!(
                    "CLAUDE_CODE_SESSION_ID not set; this channel cannot be correlated to a relay session"
                ),
            }
            socket_path
        }
    };
    let socket_path = socket_path.as_path();

    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        crate::fsperm::create_dir_private(parent)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
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
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(response) = handle_request(&message) {
            let mut out = stdout.lock().await;
            write_line(&mut *out, &response).await?;
        }
    }

    accept_task.abort();
    notify_task.abort();
    Ok(())
}

/// POSTs `{"session_id", "socket"}` to the daemon's registration endpoint
/// (`relay::pipeline::CHANNEL_REGISTER_PATH`). A plain `reqwest` call rather than the injectable
/// `wake::runner::HttpPoster`: this runs inside the short-lived `ashkelon channel` process, which
/// has no test double to inject it through — its behavior is exercised by the live serve-mode
/// test instead (a real channel process registering with a real running daemon).
async fn register(register_url: &str, session_id: &str, socket_path: &Path) -> anyhow::Result<()> {
    let body = json!({
        "session_id": session_id,
        "socket": socket_path.to_string_lossy(),
    });
    let client = reqwest::Client::new();
    let resp = client.post(register_url).json(&body).send().await?;
    anyhow::ensure!(resp.status().is_success(), "registration rejected: {}", resp.status());
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
            let protocol_version = message
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("2026-06-18");
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
        _ if id.is_some() => Some(
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } }),
        ),
        _ => None,
    }
}
