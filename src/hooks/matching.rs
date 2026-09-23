use std::path::{Path, PathBuf};

use crate::config::{HookConfig, HookEvent};

/// Whether `hook` should fire for `event` in a session with the given harness and cwd.
/// `harnesses`/`projects` empty means "any"; a session with no cwd only matches a hook whose
/// `projects` list is itself empty (there is nothing to test a path-prefix against).
pub fn matches(hook: &HookConfig, event: HookEvent, harness: Option<&str>, cwd: Option<&Path>) -> bool {
    if !hook.on.contains(&event) {
        return false;
    }
    if !hook.harnesses.is_empty() {
        match harness {
            Some(h) if hook.harnesses.iter().any(|x| x == h) => {}
            _ => return false,
        }
    }
    if !hook.projects.is_empty() {
        match cwd {
            Some(dir) => {
                if !hook.projects.iter().any(|p| dir.starts_with(expand_tilde(p))) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// Expands a leading `~` (or bare `~`) to the current user's home directory. Any other prefix
/// is returned unchanged.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

