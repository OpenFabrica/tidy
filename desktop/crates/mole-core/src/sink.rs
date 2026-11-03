// The single deletion funnel, ported from mole_delete (lib/core/file_ops.sh).
// Every removal in the desktop app goes through `delete`; the clippy
// disallowed-methods gate bans the raw primitives everywhere else. Order of
// operations is contractual: validate → privileged-ancestor guard → size
// capture → identity rebind → dry-run → Trash/permanent, logging every outcome.

use crate::identity::{self, PathIdentity};
use crate::logging::{DeletionsLog, OperationsLog};
use crate::plan::{Scope, SizeKb};
use crate::providers::{PrivilegedRunner, TrashError, TrashProvider};
use crate::validate::{validate_path_for_deletion, RejectReason, ValidationCtx};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Trash (recoverable, default for user-facing surfaces) or permanent removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    Trash,
    Permanent,
}

impl DeleteMode {
    /// The mode field written into deletions.log ("trash" / "permanent").
    fn log_name(&self) -> &'static str {
        match self {
            DeleteMode::Trash => "trash",
            DeleteMode::Permanent => "permanent",
        }
    }
}

/// Everything one delete call needs: policy, probes, loggers, providers.
pub struct OpContext<'a> {
    pub validation: ValidationCtx<'a>,
    pub deletions_log: &'a DeletionsLog,
    pub oplog: &'a OperationsLog,
    /// Command tag for operations.log lines ("clean", "uninstall", ...).
    pub command: String,
    pub dry_run: bool,
    pub trash: &'a dyn TrashProvider,
    pub privileged: &'a dyn PrivilegedRunner,
}

/// What actually happened to the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    Removed {
        size_kb: SizeKb,
    },
    Trashed {
        size_kb: SizeKb,
    },
    DryRun {
        size_kb: SizeKb,
    },
    /// Path was already gone — success, nothing logged (CLI parity).
    Missing,
}

/// Why the delete was refused or failed; each maps to a distinct log status.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error("empty path")]
    EmptyPath,
    #[error("rejected by validation: {0:?}")]
    Rejected(RejectReason),
    #[error("privileged path has a mutable ancestor")]
    MutableParent,
    #[error("path identity changed since planning")]
    IdentityChanged,
    #[error("privileged helper unavailable")]
    HelperUnavailable,
    #[error("macOS privacy permission denied")]
    PrivacyDenied,
    /// Carries the provider's message so the UI can say why.
    #[error("trash move failed: {0}")]
    TrashFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Delete one path through the full guard stack. `expected_identity` binds the
/// call to the plan-time snapshot; None skips the rebind (ad-hoc deletes).
pub fn delete(
    path: &str,
    scope: Scope,
    mode: DeleteMode,
    expected_identity: Option<&PathIdentity>,
    ctx: &OpContext,
) -> Result<DeleteOutcome, DeleteError> {
    if path.is_empty() {
        return Err(DeleteError::EmptyPath);
    }
    let p = Path::new(path);
    let needs_sudo = scope == Scope::System;

    // Nothing to do if the path is gone (a broken symlink still counts).
    if fs::symlink_metadata(p).is_err() {
        return Ok(DeleteOutcome::Missing);
    }

    // Refusals are recorded in the forensic log so audit trails distinguish
    // refused-by-policy from never-attempted.
    if let Err(reason) = validate_path_for_deletion(path, &ctx.validation) {
        ctx.deletions_log
            .record_raw_size(mode.log_name(), "0", "rejected", path);
        return Err(DeleteError::Rejected(reason));
    }

    if needs_sudo && privileged_path_has_mutable_ancestor(path) {
        // A privileged delete through a user-mutable ancestor hands root a
        // pathname the invoking user can swap after validation. Fail closed.
        ctx.deletions_log
            .record_raw_size(mode.log_name(), "unknown", "mutable-parent", path);
        return Err(DeleteError::MutableParent);
    }

    // Size before the delete so the log stays useful after the path is gone.
    let size_kb = measure_size_kb(p);

    // Identity rebind at the sink: refuse when the target moved or was swapped
    // between planning and execution.
    if let Some(expected) = expected_identity {
        let now = identity::snapshot(path);
        let ok = now.as_ref().is_some_and(|n| {
            n.parent == expected.parent
                && n.parent_id == expected.parent_id
                && n.target_id == expected.target_id
                && n.full_identity == expected.full_identity
        });
        if !ok {
            ctx.deletions_log
                .record(mode.log_name(), &size_kb, "identity-changed", path);
            return Err(DeleteError::IdentityChanged);
        }
    }

    if ctx.dry_run {
        ctx.deletions_log
            .record(mode.log_name(), &size_kb, "dry-run", path);
        return Ok(DeleteOutcome::DryRun { size_kb });
    }

    if needs_sudo && !ctx.privileged.available() {
        ctx.deletions_log
            .record(mode.log_name(), &size_kb, "helper-unavailable", path);
        return Err(DeleteError::HelperUnavailable);
    }

    match mode {
        DeleteMode::Trash => {
            // Trash mode is a recoverable-delete contract: on failure, fail
            // closed instead of silently switching to permanent removal.
            let result = if needs_sudo {
                ctx.privileged
                    .stage_to_trash(p)
                    .map_err(|e| TrashError::Unavailable(e.to_string()))
            } else {
                ctx.trash.trash(p)
            };
            match result {
                Ok(()) => {
                    ctx.deletions_log.record("trash", &size_kb, "ok", path);
                    ctx.oplog.operation(
                        &ctx.command,
                        "TRASHED",
                        path,
                        &format!("{}KB", size_kb.log_field()),
                    );
                    Ok(DeleteOutcome::Trashed { size_kb })
                }
                Err(TrashError::PrivacyDenied) => {
                    ctx.deletions_log
                        .record("trash", &size_kb, "privacy-denied", path);
                    ctx.oplog
                        .operation(&ctx.command, "SKIPPED", path, "privacy permission denied");
                    Err(DeleteError::PrivacyDenied)
                }
                Err(TrashError::Unavailable(detail)) => {
                    ctx.deletions_log
                        .record("trash", &size_kb, "trash-failed", path);
                    ctx.oplog
                        .operation(&ctx.command, "SKIPPED", path, "trash-failed");
                    Err(DeleteError::TrashFailed(detail))
                }
            }
        }
        DeleteMode::Permanent => {
            // Recheck at the permanent-delete sink: the parent may have become
            // mutable while size accounting was running.
            if needs_sudo && privileged_path_has_mutable_ancestor(path) {
                ctx.deletions_log
                    .record("permanent", &size_kb, "mutable-parent", path);
                return Err(DeleteError::MutableParent);
            }
            let result = if needs_sudo {
                ctx.privileged.remove(p)
            } else {
                remove_local(p)
            };
            match result {
                Ok(()) => {
                    ctx.deletions_log.record("permanent", &size_kb, "ok", path);
                    ctx.oplog.operation(
                        &ctx.command,
                        "REMOVED",
                        path,
                        &format!("{}KB", size_kb.log_field()),
                    );
                    Ok(DeleteOutcome::Removed { size_kb })
                }
                Err(e) => {
                    ctx.deletions_log
                        .record("permanent", &size_kb, "error", path);
                    ctx.oplog.operation(&ctx.command, "FAILED", path, "error");
                    Err(DeleteError::Io(e))
                }
            }
        }
    }
}

/// Port of `_mole_privileged_path_has_mutable_ancestor`: true when any parent
/// component is a symlink, not root-owned, group/other-writable, unstatable,
/// or writable by the invoking (non-root) user. True = refuse.
fn privileged_path_has_mutable_ancestor(path: &str) -> bool {
    let invoking_uid = unsafe { libc::getuid() };
    let mut probe = crate::fsutil::lexical_parent(path);
    loop {
        let p = Path::new(&probe);
        let meta = match fs::symlink_metadata(p) {
            Ok(m) => m,
            // Unstatable ancestors are unknown, and unknown must refuse.
            Err(_) => return true,
        };
        if meta.file_type().is_symlink() {
            return true;
        }
        if meta.uid() != 0 || (meta.mode() & 0o022) != 0 {
            return true;
        }
        if invoking_uid != 0 {
            // The desktop app runs as the invoking user, so a direct
            // access(W_OK) probe answers for ACL grants too.
            let c_path = match std::ffi::CString::new(probe.as_bytes()) {
                Ok(c) => c,
                Err(_) => return true,
            };
            if unsafe { libc::access(c_path.as_ptr(), libc::W_OK) } == 0 {
                return true;
            }
        }
        if probe == "/" {
            break;
        }
        probe = crate::fsutil::lexical_parent(&probe);
    }
    false
}

/// Physical (no-follow) allocated size of a tree in KB, `du -skP`-style;
/// Unknown on any traversal error rather than a lying partial number.
fn measure_size_kb(path: &Path) -> SizeKb {
    fn walk(path: &Path, blocks: &mut u64) -> std::io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        *blocks += meta.blocks();
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                walk(&entry?.path(), blocks)?;
            }
        }
        Ok(())
    }
    let mut blocks = 0u64;
    match walk(path, &mut blocks) {
        Ok(()) => SizeKb::Known(blocks * 512 / 1024),
        Err(_) => SizeKb::Unknown,
    }
}

/// The one place raw deletion primitives are allowed: local (user-scope)
/// permanent removal, leaf symlinks removed without following.
#[allow(clippy::disallowed_methods)]
fn remove_local(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        // Files and symlinks: unlink the leaf, never the target.
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{DeletionsLog, OperationsLog};
    use crate::policy::PolicyCtx;
    use crate::probes::StubProbes;
    use crate::providers::{DeniedPrivilegedRunner, TempDirTrash};

    /// Test harness: temp HOME, temp trash, scripted probes.
    struct Harness {
        _tmp: tempfile::TempDir,
        root: std::path::PathBuf,
        trash_dir: std::path::PathBuf,
        deletions: DeletionsLog,
        oplog: OperationsLog,
    }

    impl Harness {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            Self {
                trash_dir: root.join("Trash"),
                deletions: DeletionsLog::at(root.join("deletions.log")),
                oplog: OperationsLog::at(root.join("operations.log")),
                root,
                _tmp: tmp,
            }
        }

        fn ctx<'a>(
            &'a self,
            probes: &'a StubProbes,
            trash: &'a TempDirTrash,
            privileged: &'a DeniedPrivilegedRunner,
            dry_run: bool,
        ) -> OpContext<'a> {
            OpContext {
                validation: ValidationCtx {
                    policy: PolicyCtx {
                        home: self.root.to_string_lossy().into_owned(),
                        uninstall_mode: false,
                    },
                    probes,
                },
                deletions_log: &self.deletions,
                oplog: &self.oplog,
                command: "clean".into(),
                dry_run,
                trash,
                privileged,
            }
        }

        fn read_deletions(&self) -> String {
            std::fs::read_to_string(self.root.join("deletions.log")).unwrap_or_default()
        }
    }

    #[test]
    fn trash_routes_and_logs_ok() {
        let h = Harness::new();
        let victim = h.root.join("cache-blob");
        std::fs::write(&victim, b"data").unwrap();
        let probes = StubProbes::idle();
        let trash = TempDirTrash {
            dir: h.trash_dir.clone(),
        };
        let denied = DeniedPrivilegedRunner;
        let ctx = h.ctx(&probes, &trash, &denied, false);

        let outcome = delete(
            victim.to_str().unwrap(),
            Scope::User,
            DeleteMode::Trash,
            None,
            &ctx,
        )
        .unwrap();
        assert!(matches!(outcome, DeleteOutcome::Trashed { .. }));
        assert!(!victim.exists());
        assert!(h.trash_dir.join("cache-blob").exists());
        let log = h.read_deletions();
        assert!(log.contains("\ttrash\t"), "log: {log}");
        assert!(log.contains("\tok\t"), "log: {log}");
    }

    #[test]
    fn dry_run_mutates_nothing_and_logs() {
        let h = Harness::new();
        let victim = h.root.join("keep-me");
        std::fs::write(&victim, b"data").unwrap();
        let probes = StubProbes::idle();
        let trash = TempDirTrash {
            dir: h.trash_dir.clone(),
        };
        let denied = DeniedPrivilegedRunner;
        let ctx = h.ctx(&probes, &trash, &denied, true);

        let outcome = delete(
            victim.to_str().unwrap(),
            Scope::User,
            DeleteMode::Trash,
            None,
            &ctx,
        )
        .unwrap();
        assert!(matches!(outcome, DeleteOutcome::DryRun { .. }));
        assert!(victim.exists());
        assert!(h.read_deletions().contains("\tdry-run\t"));
    }

    #[test]
    fn rejected_path_is_logged_and_refused() {
        let h = Harness::new();
        let probes = StubProbes::idle();
        let trash = TempDirTrash {
            dir: h.trash_dir.clone(),
        };
        let denied = DeniedPrivilegedRunner;
        let ctx = h.ctx(&probes, &trash, &denied, false);

        let err = delete(
            "/etc/passwd",
            Scope::User,
            DeleteMode::Permanent,
            None,
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, DeleteError::Rejected(_)));
        assert!(std::path::Path::new("/etc/passwd").exists());
        let log = h.read_deletions();
        assert!(log.contains("\t0\trejected\t/etc/passwd"), "log: {log}");
    }

    #[test]
    fn identity_swap_is_refused() {
        let h = Harness::new();
        let victim = h.root.join("swap-me");
        std::fs::write(&victim, b"one").unwrap();
        let snapshot = identity::snapshot(victim.to_str().unwrap()).unwrap();
        // Swap the inode between planning and execution.
        std::fs::remove_file(&victim).unwrap();
        std::fs::write(&victim, b"two").unwrap();

        let probes = StubProbes::idle();
        let trash = TempDirTrash {
            dir: h.trash_dir.clone(),
        };
        let denied = DeniedPrivilegedRunner;
        let ctx = h.ctx(&probes, &trash, &denied, false);
        let err = delete(
            victim.to_str().unwrap(),
            Scope::User,
            DeleteMode::Trash,
            Some(&snapshot),
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, DeleteError::IdentityChanged));
        assert!(victim.exists());
        assert!(h.read_deletions().contains("\tidentity-changed\t"));
    }

    #[test]
    fn system_scope_without_helper_is_refused() {
        let h = Harness::new();
        let probes = StubProbes::idle();
        let trash = TempDirTrash {
            dir: h.trash_dir.clone(),
        };
        let denied = DeniedPrivilegedRunner;
        let ctx = h.ctx(&probes, &trash, &denied, false);
        // A real root-owned path that passes validation (log dir allowlisted).
        let err = delete(
            "/private/var/log/install.log",
            Scope::System,
            DeleteMode::Permanent,
            None,
            &ctx,
        )
        .unwrap_err();
        // Either the mutable-ancestor guard or the helper gate must stop it —
        // never an actual removal.
        assert!(matches!(
            err,
            DeleteError::HelperUnavailable | DeleteError::MutableParent
        ));
        assert!(std::path::Path::new("/private/var/log/install.log").exists());
    }

    #[test]
    fn missing_path_is_quiet_success() {
        let h = Harness::new();
        let probes = StubProbes::idle();
        let trash = TempDirTrash {
            dir: h.trash_dir.clone(),
        };
        let denied = DeniedPrivilegedRunner;
        let ctx = h.ctx(&probes, &trash, &denied, false);
        let gone = h.root.join("never-existed");
        let outcome = delete(
            gone.to_str().unwrap(),
            Scope::User,
            DeleteMode::Trash,
            None,
            &ctx,
        )
        .unwrap();
        assert_eq!(outcome, DeleteOutcome::Missing);
        assert!(h.read_deletions().is_empty());
    }

    #[test]
    fn unknown_size_never_becomes_zero() {
        assert_eq!(SizeKb::Unknown.log_field(), "unknown");
        assert_eq!(SizeKb::Known(0).log_field(), "0");
    }
}
