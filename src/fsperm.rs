//! Every directory and file ashkelon writes that can carry conversation content (session state,
//! request/response bodies, hook logs, per-launch config overlays) must be owner-only from the
//! instant it's created — never briefly world-readable while content is written, then narrowed.

use std::io::Write;
use std::path::Path;

#[cfg(unix)]
pub fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.exists() {
        return Ok(());
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
pub fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
pub fn set_private_file(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn set_private_file(_file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}

/// Creates (or truncates) `path` and writes `bytes`, with owner-only permissions applied to the
/// file descriptor before any content is written — not a chmod after the fact.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    // Belt and suspenders: `mode()` only governs a *new* file, so a pre-existing file at this
    // path (e.g. left over from before this code existed) still gets narrowed here.
    set_private_file(&file)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Async counterpart for tokio call sites; runs the same blocking logic on the blocking pool.
pub async fn create_dir_private_async(dir: &Path) -> std::io::Result<()> {
    let dir = dir.to_path_buf();
    match tokio::task::spawn_blocking(move || create_dir_private(&dir)).await {
        Ok(result) => result,
        Err(e) => Err(std::io::Error::other(e)),
    }
}

pub async fn write_private_file_async(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    match tokio::task::spawn_blocking(move || write_private_file(&path, &bytes)).await {
        Ok(result) => result,
        Err(e) => Err(std::io::Error::other(e)),
    }
}

/// Narrows an already-open append/create-mode file to owner-only before its caller writes to it,
/// so appended content never lands while the file is still at the creation-time default mode.
pub async fn set_private_file_async(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        tokio::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).await
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn create_dir_private_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("nested").join("dir");
        create_dir_private(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_file_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("secret.json");
        write_private_file(&path, b"conversation content").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"conversation content");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_file_narrows_a_pre_existing_looser_file() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("existing.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private_file(&path, b"new content").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"new content");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn async_helpers_set_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("sessions").join("abc123");
        create_dir_private_async(&dir).await.unwrap();
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let path = dir.join("request.json");
        write_private_file_async(&path, b"{}").await.unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
    }
}
