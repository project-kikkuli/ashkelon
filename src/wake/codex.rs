use crate::wake::runner::CommandRunner;

/// `codex queue --thread <THREAD> --message <TEXT>` delivers to the shared local app-server
/// daemon that every interactive/background Codex session registers with (`codex agents`
/// browses the same daemon). `THREAD` accepts a UUID or an exact session name; `wake` only ever
/// has the id the running session itself reported (`WakeTarget::harness_session_id`), so this
/// is a no-op until something else discovers and records that id.
pub async fn queue_message(runner: &dyn CommandRunner, thread: &str, text: &str) -> anyhow::Result<bool> {
    let args = vec![
        "queue".to_string(),
        "--thread".to_string(),
        thread.to_string(),
        "--message".to_string(),
        text.to_string(),
    ];
    let output = runner.run("codex", &args).await?;
    Ok(output.success)
}
