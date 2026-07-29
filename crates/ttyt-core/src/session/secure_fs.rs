//! Filesystem helpers shared by anything under `session/` that writes
//! content derived from a live device session to disk (recorder, command
//! history) — both need the same `0600`/`0700` guarantee, set at creation
//! time rather than via a follow-up `chmod` that would leave a window
//! where the file/dir exists at the process umask's default permissions.

use std::fs::File;
use std::path::Path;

use crate::error::CoreError;

/// Ensures `path` (e.g. a configured `log_dir` or history directory)
/// exists and is `0700`.
///
/// Only `path` itself is tightened, never its ancestors: this process
/// doesn't own directories above its own data directory (they might be
/// the system temp dir, `~/Library/Application Support`, or similar
/// shared paths), so `chmod`-ing them would be both wrong and liable to
/// fail with a permission error in exactly the cases where it matters
/// least. Ancestors are created with ordinary (unrestricted) permissions
/// via `create_dir_all`, matching how most per-user app-data tooling
/// behaves.
#[cfg(unix)]
pub fn create_secure_dir_all(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => tighten_dir_permissions(path),
        Err(e) => Err(CoreError::Io(e)),
    }
}

#[cfg(not(unix))]
pub fn create_secure_dir_all(path: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(path).map_err(CoreError::Io)
}

/// Creates a single directory (whose parent is assumed to already exist,
/// e.g. a day-directory under an already-secured `log_dir`) at `0700`.
#[cfg(unix)]
pub fn create_secure_dir(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => tighten_dir_permissions(path),
        Err(e) => Err(CoreError::Io(e)),
    }
}

#[cfg(not(unix))]
pub fn create_secure_dir(path: &Path) -> Result<(), CoreError> {
    std::fs::create_dir(path).map_err(CoreError::Io)
}

#[cfg(unix)]
fn tighten_dir_permissions(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(CoreError::Io)
}

/// Opens (creating if needed) a file at `0600` for appending.
#[cfg(unix)]
pub fn open_secure_file(path: &Path) -> Result<File, CoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(CoreError::Io)
}

#[cfg(not(unix))]
pub fn open_secure_file(path: &Path) -> Result<File, CoreError> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(CoreError::Io)
}
