use crate::wake::runner::CommandRunner;

/// tmux has no multi-line paste primitive we can rely on across configs; a woken agent only ever
/// gets one short line, so collapsing newlines to spaces loses nothing that matters here.
fn literal(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// `tmux send-keys -t PANE -l TEXT` then `tmux send-keys -t PANE Enter`. Returns `false` (not an
/// error) when either invocation fails, e.g. the pane no longer exists.
pub async fn send(runner: &dyn CommandRunner, pane: &str, text: &str) -> anyhow::Result<bool> {
    let literal_text = literal(text);
    let type_args = vec![
        "send-keys".to_string(),
        "-t".to_string(),
        pane.to_string(),
        "-l".to_string(),
        literal_text,
    ];
    let typed = runner.run("tmux", &type_args).await?;
    if !typed.success {
        return Ok(false);
    }
    let enter_args = vec![
        "send-keys".to_string(),
        "-t".to_string(),
        pane.to_string(),
        "Enter".to_string(),
    ];
    let entered = runner.run("tmux", &enter_args).await?;
    Ok(entered.success)
}
