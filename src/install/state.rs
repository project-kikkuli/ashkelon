use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// What `install` changed, so `uninstall` can reverse exactly that and nothing else.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstallState {
    pub files: Vec<FileChange>,
    pub launch_agent_loaded: bool,
}

/// One file `install` wrote to. `prior_content` is the file's content the *first* time install
/// touched it (a rerun never overwrites this with ashkelon's own prior write, so `install` stays
/// idempotent and `uninstall` still restores the true original either way).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub existed_before: bool,
    pub prior_content: Option<String>,
}

fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("install-state.json")
}

impl InstallState {
    pub fn load(state_dir: &Path) -> InstallState {
        std::fs::read_to_string(state_path(state_dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, state_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(state_path(state_dir), serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", state_path(state_dir).display()))
    }

    /// Snapshots `path`'s current content before install modifies it, unless already recorded by
    /// an earlier run.
    pub fn track_file(&mut self, path: &Path) -> anyhow::Result<()> {
        if self.files.iter().any(|f| f.path == path) {
            return Ok(());
        }
        let (existed_before, prior_content) = match std::fs::read_to_string(path) {
            Ok(content) => (true, Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        self.files.push(FileChange {
            path: path.to_path_buf(),
            existed_before,
            prior_content,
        });
        Ok(())
    }

    /// Restores every tracked file to what it held before `install` ever touched it: rewritten if
    /// it existed, removed if `install` created it from nothing.
    pub fn restore_files(&self) -> Vec<String> {
        let mut report = Vec::new();
        for change in &self.files {
            let result = if change.existed_before {
                std::fs::write(&change.path, change.prior_content.as_deref().unwrap_or_default())
            } else {
                match std::fs::remove_file(&change.path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                }
            };
            match result {
                Ok(()) if change.existed_before => report.push(format!("{}: restored", change.path.display())),
                Ok(()) => report.push(format!("{}: removed", change.path.display())),
                Err(e) => report.push(format!("{}: FAILED ({e})", change.path.display())),
            }
        }
        report
    }
}
