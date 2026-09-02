// Provider traits the sink depends on. Production implementations live in
// mole-macos (NSFileManager Trash, SMAppService helper); tests use the stubs
// here. Making privileged operations a trait is the typed form of the
// MOLE_TEST_NO_AUTH contract: tests simply cannot construct the real runner.

use std::io;
use std::path::Path;

/// Moves a path to the user's Trash (recoverable-delete contract).
pub trait TrashProvider: Send + Sync {
    /// Move `path` into the Trash; must NOT fall back to permanent deletion.
    fn trash(&self, path: &Path) -> Result<(), TrashError>;
}

/// Why a Trash move failed; the sink maps these to distinct log statuses.
#[derive(Debug, thiserror::Error)]
pub enum TrashError {
    #[error("macOS privacy permission denied")]
    PrivacyDenied,
    #[error("trash unavailable: {0}")]
    Unavailable(String),
}

/// Runs privileged operations through the signed helper. Every call re-validates
/// inside the helper; this trait is only the transport.
pub trait PrivilegedRunner: Send + Sync {
    /// Whether the helper is installed and authorized right now.
    fn available(&self) -> bool;
    /// Permanently remove a path as root (helper re-validates before acting).
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Stage a path into root-owned staging under /Library, then hand it to
    /// the invoking user's Trash (the privileged-Trash contract).
    fn stage_to_trash(&self, path: &Path) -> io::Result<()>;
    /// Delete one Time Machine local snapshot by stamp (`YYYY-MM-DD-HHMMSS`)
    /// as root. Implementations MUST re-validate the stamp shape before
    /// touching a shell.
    fn delete_local_snapshot(&self, stamp: &str) -> io::Result<()>;
}

/// Test/M1 Trash provider: moves into a designated directory (the shell's
/// MOLE_TEST_TRASH_DIR equivalent), colliding names get a numeric suffix.
pub struct TempDirTrash {
    pub dir: std::path::PathBuf,
}

impl TrashProvider for TempDirTrash {
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        let name = path
            .file_name()
            .ok_or_else(|| TrashError::Unavailable("path has no file name".into()))?;
        std::fs::create_dir_all(&self.dir).map_err(|e| TrashError::Unavailable(e.to_string()))?;
        let mut dest = self.dir.join(name);
        let mut counter = 1;
        while dest.exists() {
            dest = self
                .dir
                .join(format!("{} {counter}", name.to_string_lossy()));
            counter += 1;
        }
        std::fs::rename(path, &dest).map_err(|e| TrashError::Unavailable(e.to_string()))
    }
}

/// Privileged runner that refuses everything: v1 default until the SMAppService
/// helper is installed, and the only runner tests may use.
pub struct DeniedPrivilegedRunner;

impl PrivilegedRunner for DeniedPrivilegedRunner {
    fn available(&self) -> bool {
        false
    }
    fn remove(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::other("privileged helper not available"))
    }
    fn stage_to_trash(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::other("privileged helper not available"))
    }
    fn delete_local_snapshot(&self, _stamp: &str) -> io::Result<()> {
        Err(io::Error::other("privileged helper not available"))
    }
}
