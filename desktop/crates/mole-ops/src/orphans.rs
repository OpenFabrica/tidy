// Orphans: leftovers under ~/Library that belong to applications which are
// no longer installed. Evidence is deliberately narrow — reverse-DNS names
// only, cross-checked against the installed bundle set, Spotlight, the
// process table, and a 30-day idle floor — and every miss fails closed
// (withheld, never "safe"). The plan is Trash-routed by the IPC layer.

use crate::scanutil::{self, CancelFlag};
use mole_core::plan::{DeletionPlan, Scope};
use mole_core::policy::{self, PolicyCtx};
use mole_core::probes::{LiveProbes, TriState};
use mole_core::state::load_whitelist;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

/// Leftovers touched more recently than this stay: a CLI tool or shared
/// component without an .app bundle may still be writing its state.
const IDLE_FLOOR: Duration = Duration::from_secs(30 * 86_400);

/// Spotlight confirmation must answer quickly; a stalled mdfind is treated as
/// "unknown" and the candidate is withheld.
const MDFIND_TIMEOUT: Duration = Duration::from_secs(5);

/// One orphan leftover row (UI badge data).
#[derive(Debug, Clone, Serialize)]
pub struct OrphanInfo {
    pub path: String,
    pub bundle_id: String,
    /// Days since the newest modification; None when it could not be read.
    pub idle_days: Option<u64>,
}

/// The orphan plan plus the per-path explanations aligned with it.
#[derive(Debug, Default)]
pub struct OrphanPlanOutput {
    pub plan: DeletionPlan,
    pub orphans: Vec<OrphanInfo>,
}

/// Where leftovers live, and how the bundle id is spelled there
/// (directory named by id, or a file with a fixed suffix).
const ROOTS: &[(&str, &str)] = &[
    ("Library/Application Support", ""),
    ("Library/Caches", ""),
    ("Library/Preferences", ".plist"),
    ("Library/Containers", ""),
    ("Library/Saved Application State", ".savedState"),
    ("Library/HTTPStorages", ""),
    ("Library/WebKit", ""),
    ("Library/Logs", ""),
    ("Library/Cookies", ".binarycookies"),
];

/// Extra app roots beyond /Applications and ~/Applications (which
/// `uninstall::bundle_listing` already covers). Relative entries hang off
/// the home directory.
const SYSTEM_APP_ROOTS: &[&str] = &[
    "/System/Applications",
    "/System/Applications/Utilities",
    "/Applications/Utilities",
    "/Applications/Setapp",
    "/Library/Application Support/JetBrains/Toolbox/apps",
];
const HOME_APP_ROOTS: &[&str] = &["Applications/JetBrains Toolbox"];

/// Reverse-DNS shape check: ≥3 dot-separated components, each made only of
/// `[A-Za-z0-9_-]`. This is also the injection guard for the mdfind query.
fn is_reverse_dns(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    if parts.len() < 3 {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// Apple's own identifiers are never orphans: system components keep state
/// under ~/Library without a matching .app bundle.
fn is_apple_id(id: &str) -> bool {
    id.starts_with("com.apple.") || id.starts_with("group.com.apple.")
}

/// Recursively collect bundle ids of `*.app` bundles under `root`, at most
/// `depth` levels deep (Toolbox keeps apps nested one level per tool).
fn collect_app_ids(root: &Path, depth: usize, out: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "app") {
            if let Some((_, bundle_id, _)) = crate::uninstall::read_app_info(&path) {
                if !bundle_id.is_empty() {
                    out.insert(bundle_id);
                }
            }
            // Never descend into a bundle: nested helper apps belong to it.
            continue;
        }
        if depth > 0 && path.is_dir() {
            collect_app_ids(&path, depth - 1, out);
        }
    }
}

/// The installed bundle-id set: standard app folders plus the system and
/// third-party launcher roots.
fn installed_bundle_ids(home: &str) -> HashSet<String> {
    let mut ids: HashSet<String> = crate::uninstall::bundle_listing(home)
        .into_iter()
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty())
        .collect();
    for root in SYSTEM_APP_ROOTS {
        collect_app_ids(Path::new(root), 3, &mut ids);
    }
    for rel in HOME_APP_ROOTS {
        collect_app_ids(&PathBuf::from(home).join(rel), 3, &mut ids);
    }
    ids
}

/// Ownership by identity: equal ids, a dotted prefix relation in either
/// direction (com.foo.App owns com.foo.App.helper, and com.foo owns both),
/// or an installed id embedded in the name ("bugsnag-shared-com.foo.App":
/// vendor SDKs key their shared dirs by the host app's bundle id).
fn owned_by_installed(id: &str, installed: &HashSet<String>) -> bool {
    installed.iter().any(|app| {
        app == id
            || id.starts_with(&format!("{app}."))
            || app.starts_with(&format!("{id}."))
            || (app.matches('.').count() >= 2 && id.contains(app.as_str()))
    })
}

/// Newest modification across the path and its first-level children, as
/// whole days before now. None when nothing could be read: the top-level
/// mtime of a directory only moves when a direct child is added or removed,
/// so the children are consulted too before calling something idle.
fn idle_days(path: &Path) -> Option<u64> {
    let mut newest: Option<SystemTime> = None;
    let mut consider = |p: &Path| {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            if let Ok(m) = meta.modified() {
                newest = Some(newest.map_or(m, |n| n.max(m)));
            }
        }
    };
    consider(path);
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            consider(&entry.path());
        }
    }
    let age = SystemTime::now().duration_since(newest?).ok()?;
    Some(age.as_secs() / 86_400)
}

/// Spotlight confirmation through mdfind: Some(true) when an app with that
/// bundle id still exists anywhere on disk, Some(false) when none is indexed,
/// None when the tool could not answer (callers fail closed).
fn spotlight_has_app(id: &str) -> Option<bool> {
    // Re-check the character set right before building the query string so
    // this function stays safe even if a caller skips the shape filter.
    if !is_reverse_dns(id) {
        return None;
    }
    let argv = [
        "/usr/bin/mdfind".to_string(),
        format!("kMDItemCFBundleIdentifier == '{id}'"),
    ];
    let result = crate::optimize::run_bounded(&argv, MDFIND_TIMEOUT);
    if !result.success() {
        return None;
    }
    Some(result.output.lines().any(|line| !line.trim().is_empty()))
}

/// Build the orphan plan with the real Spotlight probe.
pub fn build_plan(home: &str, probes: &dyn LiveProbes, cancel: &CancelFlag) -> OrphanPlanOutput {
    build_plan_with(home, probes, cancel, &spotlight_has_app)
}

/// Build the orphan plan with an injectable Spotlight probe (tests cannot
/// rely on mdfind being available or indexed).
pub fn build_plan_with(
    home: &str,
    probes: &dyn LiveProbes,
    cancel: &CancelFlag,
    spotlight: &dyn Fn(&str) -> Option<bool>,
) -> OrphanPlanOutput {
    let installed = installed_bundle_ids(home);
    let whitelist = load_whitelist(home);
    let ctx = PolicyCtx {
        home: home.to_string(),
        uninstall_mode: false,
    };
    // Spotlight answers are per id, and one id usually shows up under
    // several roots; remember them so mdfind runs once per id.
    let mut spotlight_seen: std::collections::HashMap<String, Option<bool>> =
        std::collections::HashMap::new();

    let mut paths: Vec<PathBuf> = Vec::new();
    let mut infos: Vec<OrphanInfo> = Vec::new();

    for (rel, suffix) in ROOTS {
        let root = PathBuf::from(home).join(rel);
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if cancel.load(Ordering::Relaxed) {
                // A cancelled scan must never yield a partial plan.
                return OrphanPlanOutput::default();
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            // File-style roots carry a fixed suffix; directory roots use the
            // bare id. A mismatch means the entry is not id-shaped here.
            let id = if suffix.is_empty() {
                // Directory roots: a dotted FILE here ("xterminal.windows.json")
                // is some tool's data, not an app's container.
                if !path.is_dir() {
                    continue;
                }
                name.clone()
            } else {
                match name.strip_suffix(suffix) {
                    Some(stem) => stem.to_string(),
                    None => continue,
                }
            };
            if !is_reverse_dns(&id) || is_apple_id(&id) {
                continue;
            }
            if policy::is_critical_system_component(&id) || policy::should_protect_data(&id) {
                continue;
            }
            let path_str = path.to_string_lossy().into_owned();
            if policy::should_protect_path(&path_str, &ctx)
                || policy::is_path_whitelisted(&path_str, &whitelist.patterns)
                || policy::holds_compiled_model_cache(&path_str)
            {
                continue;
            }
            if owned_by_installed(&id, &installed) {
                continue;
            }
            // Idle floor: anything touched within 30 days may be live state
            // of a CLI tool or a shared vendor component (the "same vendor
            // as an installed app and recently active" rule is a subset of
            // this floor, so one check covers both).
            let Some(days) = idle_days(&path) else {
                continue;
            };
            if Duration::from_secs(days * 86_400) < IDLE_FLOOR {
                continue;
            }
            // Process guard: Active and Unknown both withhold.
            if probes.owner_process_state(&id) != TriState::Idle {
                continue;
            }
            // Spotlight: an app installed anywhere else (external volume,
            // custom folder) still owns its leftovers. No answer → withhold.
            let verdict = *spotlight_seen
                .entry(id.clone())
                .or_insert_with(|| spotlight(&id));
            if verdict != Some(false) {
                continue;
            }
            paths.push(path);
            infos.push(OrphanInfo {
                path: path_str,
                bundle_id: id,
                idle_days: Some(days),
            });
        }
    }

    let candidates = scanutil::parallel_candidates(&paths, "orphans", Scope::User, cancel);
    if cancel.load(Ordering::Relaxed) {
        return OrphanPlanOutput::default();
    }
    // Keep only the explanations whose path survived candidate binding
    // (a leftover may vanish between listing and measurement).
    let bound: HashSet<&str> = candidates.iter().map(|c| c.path.as_str()).collect();
    let orphans = infos
        .into_iter()
        .filter(|info| bound.contains(info.path.as_str()))
        .collect();

    OrphanPlanOutput {
        plan: DeletionPlan { candidates },
        orphans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mole_core::probes::StubProbes;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Push a path's mtime back by `days` days.
    fn age(path: &Path, days: u64) {
        let old = SystemTime::now() - Duration::from_secs(days * 86_400);
        std::fs::File::options()
            .write(true)
            .open(path)
            .or_else(|_| std::fs::File::open(path))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    /// Create a leftover directory with one file inside, both aged.
    fn leftover_dir(home: &Path, rel: &str, days: u64) -> PathBuf {
        let dir = home.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        let inner = dir.join("state.db");
        std::fs::write(&inner, b"x").unwrap();
        age(&inner, days);
        age(&dir, days);
        dir
    }

    /// Minimal .app bundle with the given identifier under ~/Applications.
    fn install_app(home: &Path, name: &str, bundle_id: &str) {
        let contents = home.join(format!("Applications/{name}.app/Contents"));
        std::fs::create_dir_all(&contents).unwrap();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{bundle_id}</string>
<key>CFBundleName</key><string>{name}</string>
</dict></plist>"#
        );
        std::fs::write(contents.join("Info.plist"), plist).unwrap();
    }

    fn cancel_flag() -> CancelFlag {
        Arc::new(AtomicBool::new(false))
    }

    fn names(out: &OrphanPlanOutput) -> Vec<String> {
        out.plan
            .candidates
            .iter()
            .map(|c| c.path.rsplit('/').next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn reverse_dns_shape_is_strict() {
        assert!(is_reverse_dns("com.example.App"));
        assert!(is_reverse_dns("io.some-vendor.tool_x"));
        assert!(!is_reverse_dns("com.example"));
        assert!(!is_reverse_dns("Plain Name"));
        assert!(!is_reverse_dns("com.example.App With Space"));
        assert!(!is_reverse_dns("com..example"));
        assert!(!is_reverse_dns("com.example.a'b"));
    }

    /// Old reverse-DNS leftovers with no installed owner are candidates;
    /// installed, Apple, plain-named and recently-touched entries are not.
    #[test]
    fn plan_keeps_only_idle_unowned_reverse_dns_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        install_app(home, "Keeper", "com.keeper.App");

        leftover_dir(home, "Library/Application Support/com.gone.App", 60);
        leftover_dir(home, "Library/Caches/com.gone.App", 60);
        leftover_dir(home, "Library/Application Support/com.keeper.App", 60);
        leftover_dir(
            home,
            "Library/Application Support/com.keeper.App.helper",
            60,
        );
        leftover_dir(home, "Library/Application Support/com.apple.Something", 60);
        leftover_dir(home, "Library/Application Support/PlainName", 60);
        leftover_dir(home, "Library/Application Support/com.recent.App", 5);
        // Preferences are files with a fixed suffix.
        let prefs = home.join("Library/Preferences");
        std::fs::create_dir_all(&prefs).unwrap();
        let old_plist = prefs.join("com.gone.App.plist");
        std::fs::write(&old_plist, b"x").unwrap();
        age(&old_plist, 60);
        let keep_plist = prefs.join("com.keeper.App.plist");
        std::fs::write(&keep_plist, b"x").unwrap();
        age(&keep_plist, 60);

        let probes = StubProbes::idle();
        let out = build_plan_with(&home.to_string_lossy(), &probes, &cancel_flag(), &|_| {
            Some(false)
        });
        let got = names(&out);
        assert_eq!(
            got.iter().filter(|n| *n == "com.gone.App").count(),
            2,
            "both leftovers of the gone app: {got:?}"
        );
        assert!(got.contains(&"com.gone.App.plist".to_string()));
        assert!(!got.iter().any(|n| n.starts_with("com.keeper.App")));
        assert!(!got.contains(&"com.keeper.App.plist".to_string()));
        assert!(!got.contains(&"com.apple.Something".to_string()));
        assert!(!got.contains(&"PlainName".to_string()));
        assert!(!got.contains(&"com.recent.App".to_string()));
        assert_eq!(out.orphans.len(), out.plan.candidates.len());
        assert!(out
            .orphans
            .iter()
            .all(|o| o.bundle_id == "com.gone.App" && o.idle_days.unwrap() >= 30));
    }

    /// A Spotlight hit, an unanswerable Spotlight, or a non-idle process
    /// state each withhold the candidate.
    #[test]
    fn spotlight_and_process_guards_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        leftover_dir(home, "Library/Application Support/com.gone.App", 60);
        let home_str = home.to_string_lossy().into_owned();

        let idle = StubProbes::idle();
        let hit = build_plan_with(&home_str, &idle, &cancel_flag(), &|_| Some(true));
        assert!(
            hit.plan.candidates.is_empty(),
            "spotlight hit must withhold"
        );
        let unknown = build_plan_with(&home_str, &idle, &cancel_flag(), &|_| None);
        assert!(
            unknown.plan.candidates.is_empty(),
            "no spotlight answer must withhold"
        );

        let busy = StubProbes {
            owner_state: TriState::Unknown,
            sqlite_state: TriState::Idle,
        };
        let out = build_plan_with(&home_str, &busy, &cancel_flag(), &|_| Some(false));
        assert!(
            out.plan.candidates.is_empty(),
            "unknown process state must withhold"
        );
    }

    /// Cancellation yields an empty output, never a partial plan.
    #[test]
    fn cancelled_scan_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        leftover_dir(home, "Library/Application Support/com.gone.App", 60);
        let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
        let out = build_plan_with(
            &home.to_string_lossy(),
            &StubProbes::idle(),
            &cancel,
            &|_| Some(false),
        );
        assert!(out.plan.candidates.is_empty());
        assert!(out.orphans.is_empty());
    }
}

#[cfg(test)]
mod real_machine {
    /// Manual, read-only: cargo test -p mole-ops real_orphans_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_orphans_scan() {
        let home = std::env::var("HOME").unwrap();
        let probes = mole_core::probes::SystemProbes::new();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = super::build_plan(&home, &probes, &cancel);
        eprintln!(
            "{} orphan paths, {} KB known",
            out.plan.candidates.len(),
            out.plan.known_total_kb()
        );
        for o in &out.orphans {
            eprintln!("{} | {} | idle {:?}d", o.bundle_id, o.path, o.idle_days);
        }
    }
}
