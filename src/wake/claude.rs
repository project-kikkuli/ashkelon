use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// Writes `text` as one line to the channel MCP server's unix control socket (`ashkelon
/// channel`, spawned by Claude Code itself as an MCP server — see `launch::claude` and
/// `wake::channel`), which relays it into the session as a `notifications/claude/channel` push.
/// Collapses embedded newlines to spaces first: the socket protocol is one message per line, so
/// a literal newline would otherwise split one logical wake into several channel events.
pub async fn send_line(socket_path: &str, text: &str) -> anyhow::Result<bool> {
    let Ok(mut stream) = UnixStream::connect(socket_path).await else {
        return Ok(false);
    };
    let line = format!("{}\n", text.replace(['\n', '\r'], " "));
    Ok(stream.write_all(line.as_bytes()).await.is_ok())
}
