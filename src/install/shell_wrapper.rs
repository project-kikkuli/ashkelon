use std::path::Path;

use super::state::InstallState;

const BEGIN: &str = "# >>> ashkelon channel wrapper >>>";
const END: &str = "# <<< ashkelon channel wrapper <<<";

/// Wraps the `claude` command so every interactive launch carries
/// `--dangerously-load-development-channels server:ashkelon`, needed for Claude Code to admit the
/// channel MCP server `install`'s [`super::claude_channel`] registered at user scope. Detected
/// from `$SHELL`; unrecognized shells are reported, not guessed at.
pub fn apply(home: &Path, shell: &str, state: &mut InstallState) -> anyhow::Result<String> {
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match name {
        "fish" => apply_fish(home, state),
        "zsh" => apply_rc_block(home, ".zshrc", state),
        "bash" => apply_rc_block(home, ".bashrc", state),
        other => Ok(format!(
            "shell {other:?} has no wrapper installed; run claude with \
             --dangerously-load-development-channels server:ashkelon yourself"
        )),
    }
}

fn apply_fish(home: &Path, state: &mut InstallState) -> anyhow::Result<String> {
    let path = home.join(".config/fish/functions/claude.fish");
    state.track_file(&path)?;
    let content =
        "function claude\n    command claude --dangerously-load-development-channels server:ashkelon $argv\nend\n";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(format!("{}: claude fish function installed", path.display()))
}

fn apply_rc_block(home: &Path, rc_name: &str, state: &mut InstallState) -> anyhow::Result<String> {
    let path = home.join(rc_name);
    state.track_file(&path)?;
    let block = format!(
        "{BEGIN}\nclaude() {{\n  command claude --dangerously-load-development-channels server:ashkelon \"$@\"\n}}\n{END}\n"
    );
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = upsert_block(&existing, &block);
    std::fs::write(&path, updated)?;
    Ok(format!("{}: claude() wrapper block installed", path.display()))
}

/// Replaces an existing marked block in place (a rerun), or appends a new one.
fn upsert_block(existing: &str, block: &str) -> String {
    if let (Some(start), Some(end_idx)) = (existing.find(BEGIN), existing.find(END)) {
        let after_end = end_idx + END.len();
        let after_end = existing[after_end..]
            .strip_prefix('\n')
            .map_or(after_end, |_| after_end + 1);
        format!("{}{}{}", &existing[..start], block, &existing[after_end..])
    } else {
        let mut out = existing.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(block);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn fish_writes_function_file() {
        let home = tempdir().unwrap();
        let mut state = InstallState::default();
        apply(home.path(), "/opt/homebrew/bin/fish", &mut state).unwrap();
        let content = std::fs::read_to_string(home.path().join(".config/fish/functions/claude.fish")).unwrap();
        assert!(content.contains("--dangerously-load-development-channels server:ashkelon"));
    }

    #[test]
    fn zsh_appends_marked_block_preserving_rest_of_rc() {
        let home = tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), "export PATH=/x:$PATH\n").unwrap();
        let mut state = InstallState::default();
        apply(home.path(), "/bin/zsh", &mut state).unwrap();
        let content = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
        assert!(content.starts_with("export PATH=/x:$PATH\n"));
        assert!(content.contains(BEGIN));
        assert!(content.contains("server:ashkelon"));
    }

    #[test]
    fn rerun_replaces_block_instead_of_duplicating() {
        let home = tempdir().unwrap();
        let mut state = InstallState::default();
        apply(home.path(), "/bin/bash", &mut state).unwrap();
        apply(home.path(), "/bin/bash", &mut state).unwrap();
        let content = std::fs::read_to_string(home.path().join(".bashrc")).unwrap();
        assert_eq!(content.matches(BEGIN).count(), 1);
    }

    #[test]
    fn unknown_shell_is_reported_not_guessed() {
        let home = tempdir().unwrap();
        let mut state = InstallState::default();
        let summary = apply(home.path(), "/usr/bin/tcsh", &mut state).unwrap();
        assert!(summary.contains("tcsh"));
        assert!(state.files.is_empty());
    }
}
