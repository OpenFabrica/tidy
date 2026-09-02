// Uninstall: app inventory plus exact-evidence leftover discovery. The
// matching rules are deliberately boring (project contract): bundle-id or
// exact app-name evidence only, short-name floors, common-word rejection, and
// a shared-bundle-id sibling guard that withholds leftovers when another
// installed copy shares the identity.

use crate::scanutil::{self, CancelFlag};
use mole_core::plan::{DeletionPlan, Scope};
use mole_core::policy;
use mole_core::probes::{LiveProbes, TriState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One installed application row. Deserialize backs the desktop app's
/// persisted inventory cache (instant Apps view on relaunch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: String,
    pub version: String,
    pub path: String,
    pub size_kb: u64,
    /// System-critical: not selectable for uninstall.
    pub protected: bool,
    /// Vendor name when the app must use its official uninstaller.
    pub official_uninstaller: Option<String>,
    /// Whether the app currently appears in the process table (Unknown → true,
    /// so a doubtful state reads as "maybe running", never as "safe").
    pub running: bool,
}

/// Read Info.plist fields from an .app bundle; None when unreadable.
pub(crate) fn read_app_info(app_path: &Path) -> Option<(String, String, String)> {
    let info = app_path.join("Contents/Info.plist");
    let value = plist::Value::from_file(&info).ok()?;
    let dict = value.as_dictionary()?;
    let get = |key: &str| {
        dict.get(key)
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_string()
    };
    let name = {
        let display = get("CFBundleDisplayName");
        if display.is_empty() {
            get("CFBundleName")
        } else {
            display
        }
    };
    let name = if name.is_empty() {
        app_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        name
    };
    Some((
        name,
        get("CFBundleIdentifier"),
        get("CFBundleShortVersionString"),
    ))
}

/// Scan /Applications and ~/Applications for app bundles. Metadata (plist,
/// policy, process state) is cheap and collected first; the expensive
/// per-bundle size walks then run on a bounded utility-QoS worker pool so the
/// scan is fast without competing with the UI thread (same shape as
/// `scanutil::parallel_candidates`).
pub fn inventory(home: &str, probes: &dyn LiveProbes, cancel: &CancelFlag) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let roots = [
        PathBuf::from("/Applications"),
        PathBuf::from(home).join("Applications"),
    ];
    let mut paths: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "app") {
                continue;
            }
            let Some((name, bundle_id, version)) = read_app_info(&path) else {
                continue;
            };
            let protected =
                !bundle_id.is_empty() && policy::should_protect_from_uninstall(&bundle_id);
            let official =
                policy::official_uninstaller_vendor(&bundle_id, &name, &path.to_string_lossy())
                    .map(String::from);
            // Idle is the only state that reads as "not running".
            let running = probes.any_process_running(&[&name]) != TriState::Idle;
            paths.push(path.clone());
            apps.push(AppInfo {
                name,
                bundle_id,
                version,
                path: path.to_string_lossy().into_owned(),
                size_kb: 0,
                protected,
                official_uninstaller: official,
                running,
            });
        }
    }

    // Parallel size measurement: order-preserving, cancellation leaves the
    // affected rows at size 0 (display-only lower bound, never a plan input).
    let workers = scanutil::scan_workers(paths.len());
    let queue = std::sync::Mutex::new(paths.iter().cloned().enumerate().collect::<Vec<_>>());
    let sizes = std::sync::Mutex::new(vec![0u64; paths.len()]);
    std::thread::scope(|scope_| {
        for _ in 0..workers {
            scope_.spawn(|| {
                scanutil::set_scan_thread_qos();
                loop {
                    let job = queue.lock().unwrap().pop();
                    let Some((idx, path)) = job else { break };
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if let Ok((mole_core::plan::SizeKb::Known(kb), _)) =
                        scanutil::dir_size_kb(&path, cancel)
                    {
                        sizes.lock().unwrap()[idx] = kb;
                    }
                }
            });
        }
    });
    let sizes = sizes.into_inner().unwrap();
    for (app, size_kb) in apps.iter_mut().zip(sizes) {
        app.size_kb = size_kb;
    }

    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Lightweight (path, bundle_id) listing for the sibling guard — reads
/// Info.plist only, never measures sizes.
pub(crate) fn bundle_listing(home: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let roots = [
        PathBuf::from("/Applications"),
        PathBuf::from(home).join("Applications"),
    ];
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "app") {
                continue;
            }
            if let Some((_, bundle_id, _)) = read_app_info(&path) {
                out.push((path.to_string_lossy().into_owned(), bundle_id));
            }
        }
    }
    out
}

/// Why leftovers were withheld for an app (surfaced in the preview).
#[derive(Debug, Clone, Serialize)]
pub struct UninstallNote {
    pub bundle_id: String,
    pub note: String,
}

/// Is a display name too generic for name-based leftover matching?
/// Floor of 3 characters plus the shared common-word list from the data file.
fn name_is_generic(name: &str) -> bool {
    if name.len() < 3 {
        return true;
    }
    policy::data::LAUNCH_AGENT_NAME_COMMON_WORDS
        .iter()
        .any(|w| w.eq_ignore_ascii_case(name))
}

/// Collect exact-evidence leftover paths for one app under the user Library.
fn leftover_paths(home: &str, bundle_id: &str, app_name: &str) -> Vec<PathBuf> {
    let lib = PathBuf::from(home).join("Library");
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push_if_exists = |p: PathBuf| {
        if std::fs::symlink_metadata(&p).is_ok() {
            out.push(p);
        }
    };

    if !bundle_id.is_empty() {
        for sub in [
            "Application Support",
            "Caches",
            "Logs",
            "Containers",
            "WebKit",
            "HTTPStorages",
        ] {
            push_if_exists(lib.join(sub).join(bundle_id));
        }
        push_if_exists(lib.join("Preferences").join(format!("{bundle_id}.plist")));
        push_if_exists(
            lib.join("Saved Application State")
                .join(format!("{bundle_id}.savedState")),
        );
        push_if_exists(
            lib.join("Cookies")
                .join(format!("{bundle_id}.binarycookies")),
        );
        // LaunchAgents: exact bundle-id prefix only (never name globs).
        if let Ok(entries) = std::fs::read_dir(lib.join("LaunchAgents")) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().into_owned();
                if fname == format!("{bundle_id}.plist")
                    || (fname.starts_with(&format!("{bundle_id}.")) && fname.ends_with(".plist"))
                {
                    out.push(entry.path());
                }
            }
        }
        // Group Containers carry a team-id prefix; exact suffix evidence only.
        if let Ok(entries) = std::fs::read_dir(lib.join("Group Containers")) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().into_owned();
                if fname == bundle_id || fname.ends_with(&format!(".{bundle_id}")) {
                    out.push(entry.path());
                }
            }
        }
    }

    // Name-based evidence: exact directory name matches only, and never for
    // generic names that collide with unrelated software.
    if !name_is_generic(app_name) {
        for sub in ["Application Support", "Caches", "Logs"] {
            let p = lib.join(sub).join(app_name);
            if std::fs::symlink_metadata(&p).is_ok() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The uninstall plan: app bundles plus per-app leftovers, with notes for
/// anything withheld (shared identity, protected, official uninstaller).
#[derive(Debug, Serialize)]
pub struct UninstallPlanOutput {
    pub plan: DeletionPlan,
    pub notes: Vec<UninstallNote>,
}

/// Build a plan for the selected app paths. The sibling guard withholds
/// leftovers when ANOTHER installed app shares the bundle id.
pub fn plan_uninstall(
    home: &str,
    probes: &dyn LiveProbes,
    app_paths: &[String],
    cancel: &CancelFlag,
) -> UninstallPlanOutput {
    let _ = probes; // reserved for a future running-app execution guard
    let all = bundle_listing(home);
    let mut plan = DeletionPlan::default();
    let mut notes = Vec::new();

    for app_path in app_paths {
        let Some((name, bundle_id, _)) = read_app_info(Path::new(app_path)) else {
            notes.push(UninstallNote {
                bundle_id: String::new(),
                note: format!("unreadable app bundle: {app_path}"),
            });
            continue;
        };

        // Refusals name their cause (project gate rule).
        if !bundle_id.is_empty() && policy::should_protect_from_uninstall(&bundle_id) {
            notes.push(UninstallNote {
                bundle_id: bundle_id.clone(),
                note: "protected system component — not uninstallable".into(),
            });
            continue;
        }
        if let Some(vendor) = policy::official_uninstaller_vendor(&bundle_id, &name, app_path) {
            notes.push(UninstallNote {
                bundle_id: bundle_id.clone(),
                note: format!("must be removed with the official {vendor} uninstaller"),
            });
            continue;
        }
        // Running-app guard would go through probes at the sink; the bundle
        // itself is Trash-routed so the removal stays recoverable.
        if let Some(c) = scanutil::make_candidate(
            Path::new(app_path),
            &format!("App: {name}"),
            Scope::User,
            cancel,
        ) {
            plan.candidates.push(c);
        }

        // Sibling guard: a second installed copy sharing the bundle id means
        // the leftovers are still owned; withhold them.
        let siblings = all
            .iter()
            .filter(|(path, id)| !bundle_id.is_empty() && *id == bundle_id && path != app_path)
            .count();
        if siblings > 0 {
            notes.push(UninstallNote {
                bundle_id: bundle_id.clone(),
                note: format!(
                    "leftovers kept: {siblings} other installed copy shares this bundle id"
                ),
            });
            continue;
        }

        for leftover in leftover_paths(home, &bundle_id, &name) {
            if let Some(c) = scanutil::make_candidate(
                &leftover,
                &format!("Leftovers: {name}"),
                Scope::User,
                cancel,
            ) {
                plan.candidates.push(c);
            }
        }
    }

    UninstallPlanOutput { plan, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Create a minimal .app bundle with an XML Info.plist.
    fn make_app(dir: &Path, name: &str, bundle_id: &str) -> PathBuf {
        let app = dir.join(format!("{name}.app"));
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundleIdentifier</key><string>{bundle_id}</string>
<key>CFBundleShortVersionString</key><string>1.0</string>
</dict></plist>"#
        );
        std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        app
    }

    #[test]
    fn leftovers_use_exact_evidence_and_generic_name_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let lib = tmp.path().join("Library");
        std::fs::create_dir_all(lib.join("Application Support/com.example.myapp")).unwrap();
        std::fs::create_dir_all(lib.join("Application Support/MyApp")).unwrap();
        std::fs::create_dir_all(lib.join("Application Support/Music")).unwrap();
        std::fs::create_dir_all(lib.join("Preferences")).unwrap();
        std::fs::write(lib.join("Preferences/com.example.myapp.plist"), b"x").unwrap();
        std::fs::create_dir_all(lib.join("LaunchAgents")).unwrap();
        std::fs::write(lib.join("LaunchAgents/com.example.myapp.agent.plist"), b"x").unwrap();
        std::fs::write(lib.join("LaunchAgents/com.example.myapp2.plist"), b"x").unwrap();

        let found = leftover_paths(&home, "com.example.myapp", "MyApp");
        let names: Vec<String> = found
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(names
            .iter()
            .any(|p| p.ends_with("Application Support/com.example.myapp")));
        assert!(names
            .iter()
            .any(|p| p.ends_with("Application Support/MyApp")));
        assert!(names.iter().any(|p| p.ends_with("com.example.myapp.plist")));
        assert!(names
            .iter()
            .any(|p| p.ends_with("com.example.myapp.agent.plist")));
        // Prefix must be component-exact: myapp2 is a different id.
        assert!(!names
            .iter()
            .any(|p| p.ends_with("com.example.myapp2.plist")));

        // Generic display names never produce name-based evidence.
        let generic = leftover_paths(&home, "", "Music");
        assert!(generic.is_empty());
    }

    #[test]
    fn sibling_guard_withholds_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        // Two copies with the same bundle id inside ~/Applications.
        let user_apps = tmp.path().join("Applications");
        let a1 = make_app(&user_apps, "Dup", "com.example.dup");
        let _a2 = make_app(&user_apps.join("Old"), "Dup", "com.example.dup");
        // (nested dir keeps distinct paths; only ~/Applications root is
        // scanned, so plant the second copy there under another name)
        let a2 = make_app(&user_apps, "Dup Copy", "com.example.dup");
        let _ = a2;
        std::fs::create_dir_all(tmp.path().join("Library/Caches/com.example.dup")).unwrap();

        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let probes = mole_core::probes::StubProbes::idle();
        let out = plan_uninstall(
            &home,
            &probes,
            &[a1.to_string_lossy().into_owned()],
            &cancel,
        );
        // The bundle itself is in the plan; the leftover cache is withheld.
        assert!(out
            .plan
            .candidates
            .iter()
            .any(|c| c.path.ends_with("Dup.app")));
        assert!(!out
            .plan
            .candidates
            .iter()
            .any(|c| c.path.contains("Caches/com.example.dup")));
        assert!(out.notes.iter().any(|n| n.note.contains("leftovers kept")));
    }

    #[test]
    fn protected_and_official_apps_are_refused_with_cause() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let apps = tmp.path().join("Applications");
        let finder = make_app(&apps, "Fake Finder", "com.apple.finder");
        let falcon = make_app(&apps, "Falcon Sensor", "com.crowdstrike.falcon.Agent");

        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let probes = mole_core::probes::StubProbes::idle();
        let out = plan_uninstall(
            &home,
            &probes,
            &[
                finder.to_string_lossy().into_owned(),
                falcon.to_string_lossy().into_owned(),
            ],
            &cancel,
        );
        assert!(out.plan.candidates.is_empty());
        assert!(out
            .notes
            .iter()
            .any(|n| n.note.contains("protected system component")));
        assert!(out.notes.iter().any(|n| n.note.contains("CrowdStrike")));
    }
}
