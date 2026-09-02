// mole-macos: production implementations of mole-core's provider traits.
// Trash goes through NSFileManager (via the `trash` crate). Privileged
// (system-scope) removals elevate through `osascript ... with administrator
// privileges`, which shows the native macOS auth dialog and runs as root.
// The sink has already validated the path, checked the mutable-ancestor
// invariant, and re-snapshotted identity before either privileged call, so the
// elevated command receives a vetted target.

use mole_core::providers::{PrivilegedRunner, TrashError, TrashProvider};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Finder-equivalent Trash provider backed by NSFileManager trashItemAtURL.
pub struct FinderTrash;

impl TrashProvider for FinderTrash {
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        trash::delete(path).map_err(|e| {
            let msg = e.to_string();
            // macOS privacy (TCC) refusals surface as permission errors; map
            // them to the distinct status the sink logs as privacy-denied.
            if msg.contains("Operation not permitted") || msg.contains("-5000") {
                TrashError::PrivacyDenied
            } else {
                TrashError::Unavailable(msg)
            }
        })
    }
}

/// Privileged runner using osascript admin elevation. Available on macOS; the
/// user sees the system auth prompt on first use per session.
pub struct AdminRunner;

/// Escape a path for embedding inside an AppleScript double-quoted string,
/// which is then embedded in a shell `do shell script`. Both layers need their
/// own quoting; single-quoting the shell argument makes shell metacharacters
/// literal, and we escape `\` and `"` for the AppleScript string.
fn applescript_shell_quoted(path: &Path) -> String {
    // Single-quote for the shell: only ' needs escaping (close-quote, escaped
    // quote, reopen).
    let shell = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    // Then escape for the AppleScript string literal.
    shell.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Strict `YYYY-MM-DD-HHMMSS` check for a Time Machine snapshot stamp: digits
/// and dashes at fixed positions only, so the stamp can be interpolated into
/// the elevated command without any quoting concern.
fn is_valid_snapshot_stamp(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    if bytes.len() != 17 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        4 | 7 | 10 => *b == b'-',
        _ => b.is_ascii_digit(),
    })
}

/// Run one shell command as root via osascript. Refuses in any test mode so a
/// test can never trigger a real authorization prompt.
fn run_as_admin(shell_cmd: &str) -> io::Result<()> {
    if std::env::var("MOLE_TEST_MODE").as_deref() == Ok("1")
        || std::env::var("MOLE_TEST_NO_AUTH").as_deref() == Ok("1")
    {
        return Err(io::Error::other("blocked in test mode"));
    }
    let script = format!("do shell script \"{shell_cmd}\" with administrator privileges");
    let output = Command::new("osascript").args(["-e", &script]).output()?;
    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        // User cancelled the auth dialog → distinct, quiet error.
        if err.contains("-128") || err.contains("User canceled") {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        } else {
            Err(io::Error::other(err.trim().to_string()))
        }
    }
}

impl PrivilegedRunner for AdminRunner {
    fn available(&self) -> bool {
        // Elevation is available whenever we are not in a test guard.
        std::env::var("MOLE_TEST_MODE").as_deref() != Ok("1")
            && std::env::var("MOLE_TEST_NO_AUTH").as_deref() != Ok("1")
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        // `rm -rf --` on the pre-validated absolute path.
        run_as_admin(&format!(
            "/bin/rm -rf -- {}",
            applescript_shell_quoted(path)
        ))
    }

    fn stage_to_trash(&self, path: &Path) -> io::Result<()> {
        // Privileged Trash: move the root-owned item into the invoking user's
        // Trash and hand ownership back, so it stays recoverable. This command
        // runs as ROOT, so HOME (an attacker-influenceable env var) must never
        // reach the shell string unescaped — it goes through the exact same
        // quoting as `path`, and is rejected unless it is an absolute path.
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.starts_with('/') {
            return Err(io::Error::other("invalid HOME"));
        }
        let trash_path = PathBuf::from(&home).join(".Trash");
        let trash_q = applescript_shell_quoted(&trash_path);
        let quoted = applescript_shell_quoted(path);
        // getuid() is a numeric literal — no quoting needed.
        let uid = unsafe { libc::getuid() };
        // mkdir the Trash, mv the item in, then chown back to the invoking user
        // so they can empty/restore it without another prompt.
        let cmd = format!(
            "/bin/mkdir -p {trash_q} && /bin/mv -f -- {quoted} {trash_q}/ && /usr/sbin/chown -R {uid} {trash_q}"
        );
        run_as_admin(&cmd)
    }

    fn delete_local_snapshot(&self, stamp: &str) -> io::Result<()> {
        // Validate BEFORE the test-mode guard inside run_as_admin: a malformed
        // stamp must be refused on its own merits, never because elevation
        // happened to be unavailable.
        if !is_valid_snapshot_stamp(stamp) {
            return Err(io::Error::other("invalid snapshot stamp"));
        }
        run_as_admin(&format!("/usr/bin/tmutil deletelocalsnapshots {stamp}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_the_real_applescript_shell_pipeline() {
        // The real security property: a hostile path must reach the elevated
        // command as ONE literal argument through BOTH layers (AppleScript
        // string parse + shell parse). Run the exact `do shell script` pipeline
        // WITHOUT admin (no prompt, runs as the user) and assert it echoes the
        // original — proving no metacharacter takes effect at either layer.
        for evil in [
            "/tmp/a'; rm -rf /'b",
            "/tmp/$(whoami)",
            "/tmp/`id`",
            "/tmp/a b;c&d|e",
            "/tmp/with\"quote",
            "/tmp/with\\back",
        ] {
            let q = applescript_shell_quoted(Path::new(evil));
            let script = format!("do shell script \"/bin/echo -n {q}\"");
            let out = Command::new("osascript")
                .args(["-e", &script])
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'),
                evil,
                "quoting leaked at some layer: {evil}"
            );
        }
    }

    /// A malformed stamp is refused by shape, with the test guard still on:
    /// validation runs before elevation is even considered.
    #[test]
    fn malformed_snapshot_stamps_never_reach_elevation() {
        std::env::set_var("MOLE_TEST_NO_AUTH", "1");
        for bad in [
            "2024-05-01;123456",
            "2024-05-01-12345a",
            "2024-05-01-12345",
            "2024-05-01-1234567",
            "",
        ] {
            let err = AdminRunner.delete_local_snapshot(bad).unwrap_err();
            assert_eq!(err.to_string(), "invalid snapshot stamp", "stamp {bad:?}");
        }
    }

    #[test]
    fn env_guarded_behaviors() {
        // Env vars are process-global; keep every env mutation in one
        // (non-parallel) test to avoid racing another test's reads.
        std::env::set_var("MOLE_TEST_NO_AUTH", "1");
        let r = AdminRunner;
        assert!(!r.available());
        assert!(r.remove(Path::new("/private/var/x")).is_err());
        assert!(r.stage_to_trash(Path::new("/private/var/x")).is_err());
        assert!(r.delete_local_snapshot("2024-05-01-123456").is_err());

        // With the guard off, a relative HOME is rejected before elevation.
        std::env::remove_var("MOLE_TEST_NO_AUTH");
        std::env::remove_var("MOLE_TEST_MODE");
        std::env::set_var("HOME", "not/absolute");
        let err = AdminRunner
            .stage_to_trash(Path::new("/private/var/x"))
            .unwrap_err();
        assert!(err.to_string().contains("invalid HOME"));
        std::env::set_var("MOLE_TEST_NO_AUTH", "1");
    }
}
