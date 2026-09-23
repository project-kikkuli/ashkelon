use crate::wake::runner::HttpPoster;

/// `POST /session/:sessionID/message` on a running `opencode serve` control endpoint (route
/// confirmed by literal strings in the installed binary; the exact body shape is not pinned by
/// any embedded schema, so this sends the minimal documented text-part shape and treats any
/// non-2xx reply as "could not wake" rather than an error).
pub async fn send_message(http: &dyn HttpPoster, control: &str, session_id: &str, text: &str) -> anyhow::Result<bool> {
    let url = format!("{}/session/{}/message", control.trim_end_matches('/'), session_id);
    let body = serde_json::json!({
        "parts": [{ "type": "text", "text": text }],
    });
    let status = http.post_json(&url, body.to_string().as_bytes()).await?;
    Ok((200..300).contains(&status))
}
