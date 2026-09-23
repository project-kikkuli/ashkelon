use crate::wake::runner::HttpPoster;

/// `POST /session/:sessionID/message` on a running `opencode serve` control endpoint (route
/// confirmed by literal strings in the installed binary; the exact body shape is not pinned by
/// any embedded schema, so this sends the minimal documented text-part shape and treats any
/// non-2xx reply as "could not wake" rather than an error).
pub async fn send_message(
    http: &dyn HttpPoster,
    control: &str,
    session_id: &str,
    text: &str,
    authorization: Option<&str>,
) -> anyhow::Result<bool> {
    let url = format!("{}/session/{}/message", control.trim_end_matches('/'), session_id);
    let body = serde_json::json!({
        "parts": [{ "type": "text", "text": text }],
    });
    let status = http.post_json(&url, body.to_string().as_bytes(), authorization).await?;
    Ok((200..300).contains(&status))
}

/// `GET /session` lists every session on the server (route confirmed by literal strings in the
/// installed binary; the response's exact field names are not pinned by an embedded schema).
/// Looks for a numeric `time.updated` (falling back to `time.created`, then a bare `updated`) on
/// each entry and returns the `id` of whichever is highest, so a caller with no better signal
/// still reaches whichever session was interacted with most recently. `None` on anything that
/// doesn't parse as expected, so a schema surprise falls through to tmux rather than erroring.
pub async fn latest_session_id(http: &dyn HttpPoster, control: &str, authorization: Option<&str>) -> anyhow::Result<Option<String>> {
    let url = format!("{}/session", control.trim_end_matches('/'));
    let response = http.get_json(&url, authorization).await?;
    if !(200..300).contains(&response.status) {
        return Ok(None);
    }
    let Ok(sessions) = serde_json::from_slice::<Vec<serde_json::Value>>(&response.body) else {
        return Ok(None);
    };

    let mut best: Option<(f64, String)> = None;
    for session in &sessions {
        let Some(id) = session.get("id").and_then(|v| v.as_str()) else { continue };
        let timestamp = session
            .get("time")
            .and_then(|t| t.get("updated").or_else(|| t.get("created")))
            .and_then(|v| v.as_f64())
            .or_else(|| session.get("updated").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        if best.as_ref().map(|(best_ts, _)| timestamp > *best_ts).unwrap_or(true) {
            best = Some((timestamp, id.to_string()));
        }
    }
    Ok(best.map(|(_, id)| id))
}
