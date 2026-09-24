use std::path::{Path, PathBuf};

/// Ensures a socket path named by an `/internal/claude-channel` registration is one only
/// `ashkelon channel` (or `install`, running as the same user) could have arranged: it resolves
/// inside ashkelon's own canonicalized state dir, and its nearest existing ancestor is owned by
/// whichever user this daemon runs as. Loopback binding alone doesn't establish that — any local
/// process can reach this endpoint — so without this check a registration could point a live wake
/// at an arbitrary unix socket path and receive pings meant for a real Claude Code session.
///
/// The socket file itself usually does not exist yet at registration time
/// (`wake::channel::run` registers before it binds its listener), so this walks up to the
/// nearest path component that does exist, canonicalizes that (resolving any symlink), and
/// rebuilds the rest lexically before comparing against the canonicalized state dir.
pub fn trusted_channel_socket(state_dir: &Path, socket: &Path) -> Result<(), String> {
    if !socket.is_absolute() {
        return Err("socket path must be absolute".to_string());
    }

    crate::fsperm::create_dir_private(state_dir).map_err(|e| format!("creating state dir: {e}"))?;
    let canonical_state_dir = std::fs::canonicalize(state_dir).map_err(|e| format!("canonicalizing state dir: {e}"))?;

    let (existing_ancestor, suffix) = nearest_existing_ancestor(socket);
    let canonical_ancestor = std::fs::canonicalize(&existing_ancestor)
        .map_err(|e| format!("canonicalizing {}: {e}", existing_ancestor.display()))?;

    let mut resolved = canonical_ancestor.clone();
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    if !resolved.starts_with(&canonical_state_dir) {
        return Err(format!("{} is outside ashkelon's state dir", socket.display()));
    }

    let owner = owner_uid(&canonical_ancestor).map_err(|e| format!("stat {}: {e}", canonical_ancestor.display()))?;
    if owner != process_uid() {
        return Err(format!(
            "{} is not owned by the current user",
            canonical_ancestor.display()
        ));
    }
    Ok(())
}

/// Walks `path` up to the nearest component that exists on disk, returning that existing prefix
/// and the (leaf-first) components stripped off to get there.
fn nearest_existing_ancestor(path: &Path) -> (PathBuf, Vec<std::ffi::OsString>) {
    let mut current = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        if current.exists() {
            return (current, suffix);
        }
        let Some(name) = current.file_name().map(|n| n.to_os_string()) else {
            // Ran out of components without finding anything that exists (shouldn't happen on
            // unix, where "/" always exists); return as-is so the caller surfaces a canonicalize
            // error instead of looping forever.
            return (current, suffix);
        };
        suffix.push(name);
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return (current, suffix),
        }
    }
}

#[cfg(unix)]
fn process_uid() -> u32 {
    // SAFETY: getuid() takes no arguments, performs no I/O, and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn process_uid() -> u32 {
    0
}

#[cfg(unix)]
fn owner_uid(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.uid())
}

#[cfg(not(unix))]
fn owner_uid(_path: &Path) -> std::io::Result<u32> {
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn accepts_a_not_yet_created_socket_under_an_existing_state_dir() {
        let root = tempdir().unwrap();
        let state_dir = root.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("channel").join("sess-1.sock");

        assert!(trusted_channel_socket(&state_dir, &socket).is_ok());
    }

    #[test]
    fn accepts_an_already_existing_socket_under_the_state_dir() {
        let root = tempdir().unwrap();
        let state_dir = root.path().join("state");
        std::fs::create_dir_all(state_dir.join("channel")).unwrap();
        let socket = state_dir.join("channel").join("sess-1.sock");
        std::fs::write(&socket, b"").unwrap();

        assert!(trusted_channel_socket(&state_dir, &socket).is_ok());
    }

    #[test]
    fn rejects_a_socket_outside_the_state_dir() {
        let root = tempdir().unwrap();
        let state_dir = root.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let socket = elsewhere.join("sess-1.sock");

        assert!(trusted_channel_socket(&state_dir, &socket).is_err());
    }

    #[test]
    fn rejects_a_traversal_that_escapes_the_state_dir() {
        let root = tempdir().unwrap();
        let state_dir = root.path().join("state");
        std::fs::create_dir_all(state_dir.join("channel")).unwrap();
        let socket = state_dir.join("channel").join("..").join("..").join("escaped.sock");

        assert!(trusted_channel_socket(&state_dir, &socket).is_err());
    }

    #[test]
    fn rejects_a_relative_path() {
        let root = tempdir().unwrap();
        let state_dir = root.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        assert!(trusted_channel_socket(&state_dir, Path::new("relative/sess.sock")).is_err());
    }

    #[test]
    fn rejects_a_symlink_that_resolves_outside_the_state_dir() {
        let root = tempdir().unwrap();
        let state_dir = root.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let link = state_dir.join("channel");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        let socket = link.join("sess-1.sock");

        assert!(trusted_channel_socket(&state_dir, &socket).is_err());
    }
}
