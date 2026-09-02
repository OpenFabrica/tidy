// Trash: what is sitting in the user's Trash right now. Items a user already
// threw away do not free space until the Trash is emptied, so the clean flow
// surfaces them as their own section. Execution is ALWAYS permanent (the IPC
// layer registers this plan without a Trash route): moving Trash contents to
// the Trash is a no-op, not a delete.

use crate::scanutil::{self, CancelFlag};
use mole_core::plan::{DeletionPlan, Scope};
use mole_core::policy::{self, PolicyCtx};
use mole_core::state::load_whitelist;
use std::path::PathBuf;

/// Every Trash directory this user owns: ~/.Trash plus the per-volume
/// `.Trashes/<uid>` folders of mounted external disks (symlinked volumes such
/// as the boot volume alias are skipped so nothing is listed twice).
fn trash_roots(home: &str) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(home).join(".Trash")];
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if let Ok(volumes) = std::fs::read_dir("/Volumes") {
        for entry in volumes.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue;
            }
            roots.push(entry.path().join(".Trashes").join(uid.to_string()));
        }
    }
    roots
}

/// Direct children of the given Trash roots, minus anything policy or the
/// whitelist protects (preview parity with the clean sweep).
fn trash_entries(roots: &[PathBuf], whitelist: &[String], ctx: &PolicyCtx) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            // Finder bookkeeping (.DS_Store) is not the user's garbage.
            if name == ".DS_Store" {
                continue;
            }
            let path_str = path.to_string_lossy().into_owned();
            if policy::should_protect_path(&path_str, ctx)
                || policy::is_path_whitelisted(&path_str, whitelist)
            {
                continue;
            }
            out.push(path);
        }
    }
    out
}

/// Build the Trash plan: one candidate per top-level Trash entry.
pub fn build_plan(home: &str, cancel: &CancelFlag) -> DeletionPlan {
    let whitelist = load_whitelist(home);
    let ctx = PolicyCtx {
        home: home.to_string(),
        uninstall_mode: false,
    };
    let paths = trash_entries(&trash_roots(home), &whitelist.patterns, &ctx);
    DeletionPlan {
        candidates: scanutil::parallel_candidates(&paths, "trash", Scope::User, cancel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trash children become candidates; .DS_Store and protected names do not.
    #[test]
    fn trash_children_are_candidates_except_bookkeeping() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let trash = tmp.path().join(".Trash");
        std::fs::create_dir_all(trash.join("old-project")).unwrap();
        std::fs::write(trash.join("photo.jpg"), b"x").unwrap();
        std::fs::write(trash.join(".DS_Store"), b"x").unwrap();
        std::fs::create_dir_all(trash.join("com.apple.finder.cache")).unwrap();

        let ctx = PolicyCtx {
            home: home.clone(),
            uninstall_mode: false,
        };
        let got = trash_entries(std::slice::from_ref(&trash), &[], &ctx);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"old-project".to_string()));
        assert!(names.contains(&"photo.jpg".to_string()));
        assert!(!names.contains(&".DS_Store".to_string()));
        assert!(
            !names.contains(&"com.apple.finder.cache".to_string()),
            "policy still applies inside the Trash"
        );
    }

    /// A missing Trash directory yields an empty plan, never an error.
    #[test]
    fn missing_trash_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ctx = PolicyCtx {
            home: tmp.path().to_string_lossy().into_owned(),
            uninstall_mode: false,
        };
        let got = trash_entries(&[tmp.path().join(".Trash")], &[], &ctx);
        assert!(got.is_empty());
        let _ = cancel;
    }
}
