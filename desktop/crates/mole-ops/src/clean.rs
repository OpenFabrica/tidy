// Clean: catalog-driven cleanup plan builder. Preview and execution consume
// the SAME plan (the CLI's `_safe_clean_impl` parity rule): protected,
// whitelisted, live-owner, and model-cache paths are filtered HERE so the
// preview equals the eligible set, and the sink re-checks policy at the
// deletion boundary anyway.
//
// Guard contract: every process probe is tri-state, and Unknown DENIES —
// a cache whose owner cannot be proven idle stays.

use crate::scanutil::{self, CancelFlag};
use mole_core::plan::{DeletionPlan, Scope};
use mole_core::policy::{self, PolicyCtx};
use mole_core::probes::{LiveProbes, TriState};
use mole_core::state::load_whitelist;
use std::path::{Path, PathBuf};

/// Browser cache targets each carry their own process guard: deleting a
/// Chromium cache under a RUNNING browser corrupts its cache index.
const BROWSER_TARGETS: &[(&str, &[&str], &[&str])] = &[
    // (cache dir under ~/Library/Caches, guard process names, skip-from-sweep names)
    (
        "Google/Chrome",
        &["Google Chrome", "Google Chrome Helper"],
        &["Google"],
    ),
    ("Microsoft Edge", &["Microsoft Edge"], &["Microsoft Edge"]),
    (
        "BraveSoftware/Brave-Browser",
        &["Brave Browser"],
        &["BraveSoftware"],
    ),
    ("Arc", &["Arc", "Arc Helper"], &["Arc"]),
    ("Firefox", &["firefox"], &["Firefox", "Mozilla"]),
];

/// Xcode build tooling names (port of xcode_build_tooling_process_state).
const XCODE_TOOLING: &[&str] = &[
    "Xcode",
    "xcodebuild",
    "xctest",
    "XCTRunner",
    "XCBBuildService",
    "swift-frontend",
];

/// Xcode device-support directories: per-device-OS symbol caches that Xcode
/// re-downloads from a connected device when it needs them again. Routinely
/// the largest thing under ~/Library/Developer.
const XCODE_SUPPORT_DIRS: &[&str] = &[
    "iOS DeviceSupport",
    "watchOS DeviceSupport",
    "tvOS DeviceSupport",
    "visionOS DeviceSupport",
];

/// A running Simulator holds the CoreSimulator dyld cache open.
const SIMULATOR_GUARD: &[&str] = &["Simulator"];

/// Package-manager download/extract caches under $HOME as (relative path,
/// guard processes). Every entry is re-fetched by the next install or build;
/// a running guard may hold entries open, so Active/Unknown keeps it out.
const PKG_CACHES: &[(&str, &[&str])] = &[
    (".npm/_cacache", &["npm"]),
    (".cargo/registry/cache", &["cargo", "rustc"]),
    (".cargo/registry/src", &["cargo", "rustc"]),
    (".cargo/git/checkouts", &["cargo", "rustc"]),
    (".gradle/caches", &["java", "gradle"]),
    (".m2/repository", &["java", "mvn"]),
    ("go/pkg/mod/cache", &["go", "gopls"]),
    ("Library/pnpm/store", &["pnpm"]),
    (".pnpm-store", &["pnpm"]),
    (".yarn/berry/cache", &["yarn"]),
    (".cache/yarn", &["yarn"]),
    (".bun/install/cache", &["bun"]),
    (".cocoapods/repos", &["pod"]),
    (".cache/pip", &["pip", "pip3"]),
    (".cache/uv", &["uv"]),
    (".cache/pypoetry", &["poetry"]),
    (".cache/pre-commit", &["pre-commit"]),
    (".composer/cache", &["composer"]),
    (".nuget/packages", &["dotnet"]),
];

/// Apple sandbox containers whose caches are worth sweeping. Every other
/// com.apple.* container stays untouched: their cache folders double as
/// state for system daemons and the payoff is small.
const APPLE_CONTAINER_ALLOWLIST: &[(&str, &[&str])] = &[("com.apple.Safari", &["Safari"])];

/// Rotated-log suffixes under /private/var/log (the live files stay).
const ROTATED_LOG_EXTS: &[&str] = &["gz", "bz2", "xz", "zst"];

/// ASL archives younger than this may still be referenced by syslogd.
const ASL_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// Cache-owner categories for the preview cards. Keys are stable i18n ids the
/// frontend translates; matching is by lowercase substring of the cache dir
/// name (display grouping only — protection logic is untouched).
const CATEGORY_RULES: &[(&str, &[&str])] = &[
    (
        "ai",
        &[
            "claude",
            "anthropic",
            "openai",
            "chatgpt",
            "cursor",
            "copilot",
            "ollama",
            "gemini",
            "perplexity",
            "codex",
            "midjourney",
        ],
    ),
    (
        "design",
        &[
            "figma",
            "sketch",
            "adobe",
            "canva",
            "pixelmator",
            "zeplin",
            "framer",
            "blender",
        ],
    ),
    (
        "im",
        &[
            "wechat", "tencent", "qq.", ".qq", "telegram", "discord", "slack", "whatsapp",
            "dingtalk", "lark", "feishu", "teams", "zoom", "skype",
        ],
    ),
    (
        "dev",
        &[
            "jetbrains",
            "intellij",
            "pycharm",
            "goland",
            "webstorm",
            "clion",
            "rider",
            "androidstudio",
            "xcode",
            "vscode",
            "visualstudio",
            "sublime",
            "iterm",
            "warp",
            "kitty",
            "docker",
            "npm",
            "yarn",
            "pnpm",
            "gradle",
            "maven",
            "homebrew",
            "cocoapods",
            "electron",
            "rustup",
        ],
    ),
];

/// Section key for one cache dir name; falls back to the generic app bucket.
fn cache_section(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    for (key, needles) in CATEGORY_RULES {
        if needles.iter().any(|n| lower.contains(n)) {
            return key;
        }
    }
    "app_cache"
}

/// Human-ish owner name from a reverse-DNS id: last component, skipping
/// generic tails like "app"/"helper"/"desktop".
fn owner_display(component: &str) -> String {
    const GENERIC: &[&str] = &["app", "helper", "mac", "desktop", "cache", "client"];
    let parts: Vec<&str> = component.split('.').collect();
    let pick = parts
        .iter()
        .rev()
        .find(|p| !GENERIC.contains(&p.to_ascii_lowercase().as_str()) && p.len() > 2)
        .unwrap_or(parts.last().unwrap_or(&""));
    let mut chars = pick.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Caches skipped because their owner is running (or unknowable): surfaced in
/// the preview header so the user knows what closing apps would unlock.
#[derive(Debug, Default, serde::Serialize)]
pub struct BlockedCaches {
    pub owners: Vec<String>,
    pub count: usize,
    pub total_kb: u64,
}

/// One iOS/iPadOS device backup under MobileSync (preview badge data).
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupInfo {
    pub path: String,
    pub device: String,
    pub product: String,
    /// Days since the backup last completed; None when Info.plist is unreadable.
    pub last_backup_days: Option<u64>,
}

/// Clean plan plus the blocked-cache hint and per-backup detail.
pub struct CleanPlanOutput {
    pub plan: DeletionPlan,
    pub blocked: BlockedCaches,
    pub backups: Vec<BackupInfo>,
}

/// Build the full clean plan for one machine. System-scope candidates are
/// discovered and labeled but execution gates them on the privileged helper.
pub fn build_plan(
    home: &str,
    probes: &dyn LiveProbes,
    cancel: &CancelFlag,
    mut progress: impl FnMut(&str, usize),
) -> CleanPlanOutput {
    let mut plan = DeletionPlan::default();
    let mut blocked = BlockedCaches::default();
    let whitelist = load_whitelist(home);
    let policy_ctx = PolicyCtx {
        home: home.to_string(),
        uninstall_mode: false,
    };

    // Names under ~/Library/Caches that a dedicated section owns; the generic
    // sweep must not double-collect them.
    let mut sweep_skip: Vec<&str> = Vec::new();
    for (_, _, skip) in BROWSER_TARGETS {
        sweep_skip.extend_from_slice(skip);
    }

    // --- Section: browser caches (process-guarded) ---
    let caches_root = PathBuf::from(home).join("Library/Caches");
    let mut browser_paths: Vec<PathBuf> = Vec::new();
    for (subdir, guard_procs, _) in BROWSER_TARGETS {
        let dir = caches_root.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        // Active or Unknown both deny: only a provably idle browser's cache
        // may be listed.
        if probes.any_process_running(guard_procs) != TriState::Idle {
            continue;
        }
        browser_paths.push(dir);
    }
    let browser = scanutil::parallel_candidates(&browser_paths, "browser", Scope::User, cancel);
    progress("browser", browser.len());
    plan.candidates.extend(browser);

    // --- Section: user app caches (generic sweep with the full filter set) ---
    let mut sweep_paths: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut blocked_paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&caches_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if sweep_skip.iter().any(|s| *s == name) {
                continue;
            }
            let path_str = path.to_string_lossy().into_owned();
            // Same eligible-set filters the CLI applies before preview:
            if policy::should_protect_path(&path_str, &policy_ctx) {
                continue;
            }
            if policy::is_path_whitelisted(&path_str, &whitelist.patterns) {
                continue;
            }
            if policy::holds_compiled_model_cache(&path_str) {
                continue;
            }
            // Live reverse-DNS owner guard at plan time (the sink re-checks):
            // Active and Unknown both keep the cache out of the preview.
            if name.contains('.')
                && !name.starts_with('.')
                && probes.owner_process_state(&name) != TriState::Idle
            {
                // Surface what closing the owner would unlock.
                let display = owner_display(&name);
                if !display.is_empty() && !blocked.owners.contains(&display) {
                    blocked.owners.push(display);
                }
                blocked_paths.push(path);
                continue;
            }
            let section = cache_section(&name);
            sweep_paths.push((path, section));
        }
    }
    // Sandboxed container caches join the same category groups: the user
    // sees "Messaging" or "Browsers", not where macOS happens to store it.
    sweep_paths.extend(container_cache_entries(
        home,
        probes,
        &whitelist.patterns,
        &policy_ctx,
        &mut blocked,
        &mut blocked_paths,
    ));

    // Group by category so parallel_candidates keeps one section label per call.
    for key in ["app_cache", "ai", "design", "im", "dev"] {
        let paths: Vec<PathBuf> = sweep_paths
            .iter()
            .filter(|(_, k)| *k == key)
            .map(|(p, _)| p.clone())
            .collect();
        if paths.is_empty() {
            continue;
        }
        let group = scanutil::parallel_candidates(&paths, key, Scope::User, cancel);
        progress(key, group.len());
        plan.candidates.extend(group);
    }

    // Measure the blocked caches (hint only, never candidates).
    blocked.count = blocked_paths.len();
    for path in &blocked_paths {
        if let Ok((mole_core::plan::SizeKb::Known(kb), _)) = scanutil::dir_size_kb(path, cancel) {
            blocked.total_kb += kb;
        }
    }
    blocked.owners.sort();

    // --- Section: user logs (mole's own logs are policy-protected) ---
    let logs_root = PathBuf::from(home).join("Library/Logs");
    let mut log_paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&logs_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let path_str = path.to_string_lossy().into_owned();
            if policy::should_protect_path(&path_str, &policy_ctx)
                || policy::is_path_whitelisted(&path_str, &whitelist.patterns)
            {
                continue;
            }
            log_paths.push(path);
        }
    }
    let logs = scanutil::parallel_candidates(&log_paths, "logs", Scope::User, cancel);
    progress("logs", logs.len());
    plan.candidates.extend(logs);

    // --- Section: developer caches (Xcode DerivedData + simulator dyld) ---
    let mut dev_paths: Vec<PathBuf> = Vec::new();
    let xcode_idle = probes.any_process_running(XCODE_TOOLING) == TriState::Idle;
    let developer = PathBuf::from(home).join("Library/Developer");
    // DerivedData only when the whole Xcode build tool family is provably idle.
    if xcode_idle {
        dev_paths.extend(dir_children(&developer.join("Xcode/DerivedData")));
    }
    if probes.any_process_running(SIMULATOR_GUARD) == TriState::Idle {
        dev_paths.extend(dir_children(&developer.join("CoreSimulator/Caches/dyld")));
    }
    let dev = scanutil::parallel_candidates(&dev_paths, "dev", Scope::User, cancel);
    progress("dev", dev.len());
    plan.candidates.extend(dev);

    // --- Section: Xcode device support + archives (both Xcode-idle gated) ---
    if xcode_idle {
        let mut support: Vec<PathBuf> = Vec::new();
        for sub in XCODE_SUPPORT_DIRS {
            support.extend(dir_children(&developer.join("Xcode").join(sub)));
        }
        let support = scanutil::parallel_candidates(&support, "xcode_support", Scope::User, cancel);
        progress("xcode_support", support.len());
        plan.candidates.extend(support);

        // Archives hold shipped builds and their dSYMs: rebuildable only if the
        // source is still around, so the UI leaves them unchecked by default.
        let archives = dir_children(&developer.join("Xcode/Archives"));
        let archives =
            scanutil::parallel_candidates(&archives, "xcode_archives", Scope::User, cancel);
        progress("xcode_archives", archives.len());
        plan.candidates.extend(archives);
    }

    // --- Section: package-manager caches (per-tool process guards) ---
    let pkg_paths = pkg_cache_entries(home, probes, &whitelist.patterns, &policy_ctx);
    let pkg = scanutil::parallel_candidates(&pkg_paths, "pkg_cache", Scope::User, cancel);
    progress("pkg_cache", pkg.len());
    plan.candidates.extend(pkg);

    // --- Section: per-user Darwin cache dir (/var/folders/.../C) ---
    let darwin_paths = darwin_cache_root()
        .map(|root| darwin_cache_candidates(&root, probes, &whitelist.patterns, &policy_ctx))
        .unwrap_or_default();
    let darwin = scanutil::parallel_candidates(&darwin_paths, "darwin_cache", Scope::User, cancel);
    progress("darwin_cache", darwin.len());
    plan.candidates.extend(darwin);

    // --- Section: iOS device backups (report-only detail, unchecked by default) ---
    let (backup_paths, backups) = ios_backups(home, &whitelist.patterns, &policy_ctx);
    let backup_candidates =
        scanutil::parallel_candidates(&backup_paths, "ios_backups", Scope::User, cancel);
    progress("ios_backups", backup_candidates.len());
    // Keep the detail list aligned with what actually became a candidate.
    let backups: Vec<BackupInfo> = backups
        .into_iter()
        .filter(|b| backup_candidates.iter().any(|c| c.path == b.path))
        .collect();
    plan.candidates.extend(backup_candidates);

    // --- Section: temp files (user's $TMPDIR and /private/tmp) ---
    let temp_paths = temp_candidates(&temp_roots(), &whitelist.patterns, &policy_ctx);
    let temp = scanutil::parallel_candidates(&temp_paths, "temp", Scope::User, cancel);
    progress("temp", temp.len());
    plan.candidates.extend(temp);

    // --- Section: system caches/logs (admin — helper-gated at execution) ---
    let mut system_paths: Vec<PathBuf> = Vec::new();
    for root in ["/Library/Caches", "/Library/Logs"] {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let path_str = path.to_string_lossy().into_owned();
                if policy::should_protect_path(&path_str, &policy_ctx)
                    || policy::is_path_whitelisted(&path_str, &whitelist.patterns)
                {
                    continue;
                }
                system_paths.push(path);
            }
        }
    }
    system_paths.extend(rotated_system_logs(&whitelist.patterns, &policy_ctx));
    let system = scanutil::parallel_candidates(&system_paths, "system", Scope::System, cancel);
    progress("system", system.len());
    plan.candidates.extend(system);

    CleanPlanOutput {
        plan,
        blocked,
        backups,
    }
}

/// Temp entries younger than this stay: macOS itself only reaps the per-user
/// temp dir after 3 days without access, so anything newer may be live.
const TEMP_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(3 * 86_400);

/// The per-user Darwin temp dir (what $TMPDIR points to) plus /private/tmp.
/// confstr is used instead of $TMPDIR because the app bundle's environment
/// may not carry the variable.
fn temp_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = confstr_dir(libc::_CS_DARWIN_USER_TEMP_DIR) {
        roots.push(dir);
    } else if let Ok(t) = std::env::var("TMPDIR") {
        roots.push(PathBuf::from(t.trim_end_matches('/')));
    }
    roots.push(PathBuf::from("/private/tmp"));
    roots
}

/// One libc confstr directory (trailing slash trimmed), None when unset.
fn confstr_dir(name: libc::c_int) -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: buf outlives the call and its length is passed exactly.
    let n = unsafe { libc::confstr(name, buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if n > 0 && (n as usize) <= buf.len() {
        let s = String::from_utf8_lossy(&buf[..n as usize - 1]).into_owned();
        Some(PathBuf::from(s.trim_end_matches('/')))
    } else {
        None
    }
}

/// The per-user Darwin cache dir ($HOME-independent, /var/folders/.../C).
/// Unit tests build plans against a temp home and must stay hermetic, so the
/// real machine's cache dir is never consulted under cfg(test); the entry
/// filter itself is tested directly against a fixture root.
fn darwin_cache_root() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    confstr_dir(libc::_CS_DARWIN_USER_CACHE_DIR)
}

/// Direct children of a directory (empty when it does not exist).
fn dir_children(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Days since a modification time, None when the clock disagrees.
fn days_since(modified: std::time::SystemTime) -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|d| d.as_secs() / 86_400)
}

/// Strip a team-id prefix from a group-container name ("ABCDE12345.com.x.y"
/// → "com.x.y"); anything else comes back unchanged.
fn strip_team_prefix(id: &str) -> &str {
    match id.split_once('.') {
        Some((team, rest))
            if team.len() >= 8
                && team
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && rest.contains('.') =>
        {
            rest
        }
        _ => id,
    }
}

/// Sandboxed app caches: `Containers/<id>/Data/Library/Caches/*` and
/// `Group Containers/<id>/Library/Caches/*`. The ~/Library/Caches sweep
/// never sees these, yet chat and browser apps keep most of their cache
/// here. Same filter set as the sweep plus the owner guard on the container
/// id (Unknown denies) and an in-use SQLite check on each entry; blocked
/// owners are surfaced exactly like the sweep's.
fn container_cache_entries(
    home: &str,
    probes: &dyn LiveProbes,
    whitelist: &[String],
    ctx: &PolicyCtx,
    blocked: &mut BlockedCaches,
    blocked_paths: &mut Vec<PathBuf>,
) -> Vec<(PathBuf, &'static str)> {
    let lib = PathBuf::from(home).join("Library");
    let roots = [
        (lib.join("Containers"), "Data/Library/Caches"),
        (lib.join("Group Containers"), "Library/Caches"),
    ];
    let mut out = Vec::new();
    for (root, cache_sub) in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let owner = strip_team_prefix(&id).to_string();
            // Apple containers only through the explicit allowlist (their
            // caches double as daemon state); the allowlist carries its own
            // app-name guard because the bundle id rarely appears in `ps`.
            let mut apple_guard: Option<&[&str]> = None;
            if owner.starts_with("com.apple.") || owner.starts_with("group.com.apple.") {
                match APPLE_CONTAINER_ALLOWLIST.iter().find(|(b, _)| *b == owner) {
                    Some((_, guard)) => apple_guard = Some(guard),
                    None => continue,
                }
            }
            let caches = entry.path().join(cache_sub);
            let Ok(children) = std::fs::read_dir(&caches) else {
                continue;
            };
            let mut listed: Vec<PathBuf> = Vec::new();
            for child in children.flatten() {
                let path = child.path();
                let path_str = path.to_string_lossy().into_owned();
                if policy::should_protect_path(&path_str, ctx)
                    || policy::is_path_whitelisted(&path_str, whitelist)
                    || policy::holds_compiled_model_cache(&path_str)
                {
                    continue;
                }
                listed.push(path);
            }
            if listed.is_empty() {
                continue;
            }
            // Owner guard: every identity the container answers to must be
            // provably idle. Group containers are probed under both the
            // prefixed and the bare id.
            let mut idle = probes.owner_process_state(&owner) == TriState::Idle;
            if idle && owner != id {
                idle = probes.owner_process_state(&id) == TriState::Idle;
            }
            if idle {
                if let Some(guard) = apple_guard {
                    idle = probes.any_process_running(guard) == TriState::Idle;
                }
            }
            // A cache database another process still holds open is live even
            // when the owner name is not in the process table.
            if idle {
                idle = !listed.iter().any(|p| sqlite_held_open(p, probes));
            }
            if !idle {
                let display = owner_display(&owner);
                if !display.is_empty() && !blocked.owners.contains(&display) {
                    blocked.owners.push(display);
                }
                blocked_paths.extend(listed);
                continue;
            }
            let section = cache_section(&owner);
            out.extend(listed.into_iter().map(|p| (p, section)));
        }
    }
    out
}

/// True when `path` (a .db file, or a directory holding Cache.db / *.db)
/// is positively held open by some process. Mirrors the sink's live-cache
/// SQLite check for the ~/Library/Caches tree.
fn sqlite_held_open(path: &Path, probes: &dyn LiveProbes) -> bool {
    let mut dbs: Vec<PathBuf> = Vec::new();
    if path.is_dir() {
        dbs.push(path.join("Cache.db"));
        if let Ok(entries) = std::fs::read_dir(path) {
            dbs.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("db"))),
            );
        }
    } else if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("db"))
    {
        dbs.push(path.to_path_buf());
    }
    dbs.iter()
        .filter(|p| p.is_file())
        .any(|p| probes.sqlite_in_use(p) == TriState::Active)
}

/// Existing PKG_CACHES entries whose guard processes are provably idle and
/// which neither policy nor the whitelist protects.
fn pkg_cache_entries(
    home: &str,
    probes: &dyn LiveProbes,
    whitelist: &[String],
    ctx: &PolicyCtx,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (rel, guards) in PKG_CACHES {
        let path = PathBuf::from(home).join(rel);
        if !path.is_dir() {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        if policy::should_protect_path(&path_str, ctx)
            || policy::is_path_whitelisted(&path_str, whitelist)
            || policy::holds_compiled_model_cache(&path_str)
        {
            continue;
        }
        if probes.any_process_running(guards) != TriState::Idle {
            continue;
        }
        out.push(path);
    }
    out
}

/// Eligible entries of the per-user Darwin cache dir: reverse-DNS named,
/// non-Apple, owned by this user, untouched for TEMP_MIN_AGE, owner provably
/// idle, and not protected (the endpoint-security rule lives in policy).
/// Plain names are skipped: without an owner concept nothing can vouch for
/// them being idle.
fn darwin_cache_candidates(
    root: &Path,
    probes: &dyn LiveProbes,
    whitelist: &[String],
    ctx: &PolicyCtx,
) -> Vec<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.contains('.') || name.starts_with('.') || name.starts_with("com.apple.") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.uid() != uid || meta.file_type().is_socket() {
            continue;
        }
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_none_or(|age| age < TEMP_MIN_AGE);
        if fresh {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        if policy::should_protect_path(&path_str, ctx)
            || policy::is_path_whitelisted(&path_str, whitelist)
            || policy::is_endpoint_security_cache_path(&path_str)
        {
            continue;
        }
        if probes.owner_process_state(&name) != TriState::Idle {
            continue;
        }
        out.push(path);
    }
    out
}

/// Rotated system logs under /private/var/log: compressed archives, numbered
/// rotations, and ASL archives older than a week. Live log files stay.
fn rotated_system_logs(whitelist: &[String], ctx: &PolicyCtx) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let eligible = |path: &Path| {
        let s = path.to_string_lossy().into_owned();
        !policy::should_protect_path(&s, ctx) && !policy::is_path_whitelisted(&s, whitelist)
    };
    let log_root = Path::new("/private/var/log");
    for path in dir_children(log_root) {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let numbered = !ext.is_empty() && ext.chars().all(|c| c.is_ascii_digit());
        if (ROTATED_LOG_EXTS.contains(&ext.as_str()) || numbered) && eligible(&path) {
            out.push(path);
        }
    }
    let now = std::time::SystemTime::now();
    for path in dir_children(&log_root.join("asl")) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let Some(name) = name else { continue };
        if !name.ends_with(".asl") || name == "StoreData" {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= ASL_MIN_AGE);
        if meta.is_file() && old && eligible(&path) {
            out.push(path);
        }
    }
    out
}

/// iOS/iPadOS backups under MobileSync, each with the device detail read
/// from its Info.plist. Reading the folder needs Full Disk Access; without
/// it the section is simply absent.
fn ios_backups(
    home: &str,
    whitelist: &[String],
    ctx: &PolicyCtx,
) -> (Vec<PathBuf>, Vec<BackupInfo>) {
    let root = PathBuf::from(home).join("Library/Application Support/MobileSync/Backup");
    let mut paths = Vec::new();
    let mut infos = Vec::new();
    for path in dir_children(&root) {
        if !path.is_dir() {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        if policy::should_protect_path(&path_str, ctx)
            || policy::is_path_whitelisted(&path_str, whitelist)
        {
            continue;
        }
        let plist = plist::Value::from_file(path.join("Info.plist")).ok();
        let dict = plist.as_ref().and_then(|v| v.as_dictionary());
        let text = |key: &str| {
            dict.and_then(|d| d.get(key))
                .and_then(|v| v.as_string())
                .unwrap_or("")
                .to_string()
        };
        let last_backup_days = dict
            .and_then(|d| d.get("Last Backup Date"))
            .and_then(|v| v.as_date())
            .and_then(|d| days_since(d.into()));
        infos.push(BackupInfo {
            path: path_str,
            device: text("Device Name"),
            product: text("Product Name"),
            last_backup_days,
        });
        paths.push(path);
    }
    (paths, infos)
}

/// Eligible temp entries: owned by this user, untouched for TEMP_MIN_AGE,
/// not a socket, not an Apple/launchd runtime entry, not protected or
/// whitelisted. Everything else is somebody's live scratch space.
fn temp_candidates(roots: &[PathBuf], whitelist: &[String], ctx: &PolicyCtx) -> Vec<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with("com.apple.") {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.uid() != uid || meta.file_type().is_socket() {
                continue;
            }
            let fresh = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_none_or(|age| age < TEMP_MIN_AGE);
            if fresh {
                continue;
            }
            let path_str = path.to_string_lossy().into_owned();
            if policy::should_protect_path(&path_str, ctx)
                || policy::is_path_whitelisted(&path_str, whitelist)
                || policy::is_endpoint_security_cache_path(&path_str)
            {
                continue;
            }
            out.push(path);
        }
    }
    out
}

/// Preview-parity check used by tests: no candidate in a built plan may be
/// protected or whitelisted (the filters must have run at plan time).
#[cfg(test)]
pub fn assert_plan_is_eligible(plan: &DeletionPlan, home: &str) {
    let whitelist = load_whitelist(home);
    let ctx = PolicyCtx {
        home: home.to_string(),
        uninstall_mode: false,
    };
    for c in &plan.candidates {
        assert!(
            !policy::should_protect_path(&c.path, &ctx),
            "protected path leaked into plan: {}",
            c.path
        );
        assert!(
            !policy::is_path_whitelisted(&c.path, &whitelist.patterns),
            "whitelisted path leaked into plan: {}",
            c.path
        );
    }
}

/// True when the given path exists and is a directory (helper for callers
/// composing extra sections).
pub fn dir_exists(path: &Path) -> bool {
    path.is_dir()
}

#[cfg(test)]
mod real_machine {
    use super::*;
    use mole_core::probes::SystemProbes;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Read-only smoke on the REAL home: build a clean plan with live probes
    /// and re-assert eligibility. Never deletes. Run explicitly with
    /// `cargo test -p mole-ops real_home -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_home_plan_is_eligible() {
        let home = std::env::var("HOME").unwrap();
        let probes = SystemProbes::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let out = build_plan(&home, &probes, &cancel, |section, count| {
            eprintln!("{section}: {count} candidates");
        });
        eprintln!(
            "total: {} candidates, {} KB known; blocked: {} owners / {} KB",
            out.plan.candidates.len(),
            out.plan.known_total_kb(),
            out.blocked.owners.len(),
            out.blocked.total_kb
        );
        crate::clean::assert_plan_is_eligible(&out.plan, &home);
    }
}

#[cfg(test)]
mod tests {
    /// Old, user-owned, non-Apple temp entries are eligible; fresh, hidden
    /// and com.apple.* entries are not.
    #[test]
    fn temp_candidates_skip_fresh_and_runtime_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86_400);
        for name in ["old-scratch", "com.apple.launchd.xyz", ".hidden-old"] {
            let p = root.join(name);
            std::fs::write(&p, b"x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        std::fs::write(root.join("fresh"), b"x").unwrap();
        let ctx = super::PolicyCtx {
            home: root.to_string_lossy().into_owned(),
            uninstall_mode: false,
        };
        let got = super::temp_candidates(std::slice::from_ref(&root), &[], &ctx);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["old-scratch".to_string()]);
    }

    use super::*;
    use mole_core::probes::StubProbes;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Build a fake home with caches; verify filters and guards shape the plan.
    #[test]
    fn plan_filters_protected_whitelisted_and_live() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let caches = tmp.path().join("Library/Caches");
        std::fs::create_dir_all(caches.join("com.idle.app")).unwrap();
        std::fs::create_dir_all(caches.join("com.live.app")).unwrap();
        std::fs::create_dir_all(caches.join("plaindir")).unwrap();
        std::fs::create_dir_all(caches.join("keepme")).unwrap();
        std::fs::create_dir_all(caches.join("com.apple.FontRegistry.cache")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Library/Logs/SomeApp")).unwrap();
        let config_dir = tmp
            .path()
            .join(".config")
            .join(mole_core::brand::CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("whitelist"),
            format!("{}\n", caches.join("keepme").display()),
        )
        .unwrap();

        // Owner probe: everything idle except... StubProbes is uniform, so run
        // twice — once idle (com.live.app included), once unknown (excluded).
        let cancel = Arc::new(AtomicBool::new(false));

        let idle = StubProbes::idle();
        let out = build_plan(&home, &idle, &cancel, |_, _| {});
        let plan = &out.plan;
        let paths: Vec<&str> = plan.candidates.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("com.idle.app")));
        assert!(paths.iter().any(|p| p.ends_with("plaindir")));
        assert!(paths.iter().any(|p| p.ends_with("Logs/SomeApp")));
        assert!(
            !paths.iter().any(|p| p.ends_with("keepme")),
            "whitelist filter"
        );
        assert_plan_is_eligible(&plan, &home);

        let unknown = StubProbes {
            owner_state: mole_core::probes::TriState::Unknown,
            sqlite_state: mole_core::probes::TriState::Idle,
        };
        let out2 = build_plan(&home, &unknown, &cancel, |_, _| {});
        let paths2: Vec<&str> = out2
            .plan
            .candidates
            .iter()
            .map(|c| c.path.as_str())
            .collect();
        // Unknown owner state DENIES: reverse-DNS caches drop out of the plan,
        // plain-named dirs (no owner concept) stay.
        assert!(!paths2.iter().any(|p| p.ends_with("com.idle.app")));
        assert!(paths2.iter().any(|p| p.ends_with("plaindir")));
        assert!(out2.blocked.count >= 2, "blocked caches must be surfaced");
        assert!(!out2.blocked.owners.is_empty());
    }

    /// Team-id prefixes come off group-container names; everything else stays.
    #[test]
    fn team_prefix_is_stripped_only_when_it_looks_like_one() {
        assert_eq!(
            strip_team_prefix("ABCDE12345.com.tencent.xinWeChat"),
            "com.tencent.xinWeChat"
        );
        assert_eq!(
            strip_team_prefix("group.com.apple.notes"),
            "group.com.apple.notes"
        );
        assert_eq!(strip_team_prefix("com.example.app"), "com.example.app");
        assert_eq!(strip_team_prefix("plain"), "plain");
    }

    /// Container caches: children are listed under their owner's category,
    /// Apple containers only via the allowlist, Unknown owners are blocked.
    #[test]
    fn container_caches_follow_owner_guard_and_apple_allowlist() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let containers = tmp.path().join("Library/Containers");
        std::fs::create_dir_all(containers.join("com.tencent.xinWeChat/Data/Library/Caches/blobs"))
            .unwrap();
        std::fs::create_dir_all(containers.join("com.apple.Safari/Data/Library/Caches/WebKit"))
            .unwrap();
        std::fs::create_dir_all(containers.join("com.apple.mail/Data/Library/Caches/x")).unwrap();
        let groups = tmp.path().join("Library/Group Containers");
        std::fs::create_dir_all(groups.join("ABCDE12345.com.example.team/Library/Caches/tiles"))
            .unwrap();
        let ctx = PolicyCtx {
            home: home.clone(),
            uninstall_mode: false,
        };

        let idle = StubProbes::idle();
        let mut blocked = BlockedCaches::default();
        let mut blocked_paths = Vec::new();
        let got =
            container_cache_entries(&home, &idle, &[], &ctx, &mut blocked, &mut blocked_paths);
        let mut sections: Vec<(String, &str)> = got
            .iter()
            .map(|(p, s)| (p.file_name().unwrap().to_string_lossy().into_owned(), *s))
            .collect();
        sections.sort();
        assert!(
            sections.contains(&("blobs".to_string(), "im")),
            "WeChat → messaging: {sections:?}"
        );
        assert!(
            sections.contains(&("WebKit".to_string(), "app_cache")),
            "Safari via allowlist"
        );
        assert!(
            sections.contains(&("tiles".to_string(), "app_cache")),
            "group container"
        );
        assert!(
            !sections.iter().any(|(n, _)| n == "x"),
            "com.apple.mail is not on the allowlist"
        );
        assert!(blocked_paths.is_empty());

        let unknown = StubProbes {
            owner_state: mole_core::probes::TriState::Unknown,
            sqlite_state: mole_core::probes::TriState::Idle,
        };
        let mut blocked = BlockedCaches::default();
        let mut blocked_paths = Vec::new();
        let got =
            container_cache_entries(&home, &unknown, &[], &ctx, &mut blocked, &mut blocked_paths);
        assert!(got.is_empty(), "Unknown owner state denies every container");
        assert_eq!(blocked_paths.len(), 3);
        assert!(!blocked.owners.is_empty());
    }

    /// Package caches: only existing entries with idle guards; Xcode support
    /// and archives land in their own sections when Xcode is idle.
    #[test]
    fn pkg_caches_and_xcode_dirs_become_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        std::fs::create_dir_all(tmp.path().join(".cargo/registry/cache/index")).unwrap();
        // (.gradle/caches is default-whitelisted, so it must NOT show up.)
        std::fs::create_dir_all(tmp.path().join(".gradle/caches/modules-2")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".m2/repository/org")).unwrap();
        let xcode = tmp.path().join("Library/Developer/Xcode");
        std::fs::create_dir_all(xcode.join("iOS DeviceSupport/17.0 (21A329)")).unwrap();
        std::fs::create_dir_all(xcode.join("Archives/2024-01-01/App.xcarchive")).unwrap();
        std::fs::create_dir_all(xcode.join("DerivedData/App-abc")).unwrap();
        let ctx = PolicyCtx {
            home: home.clone(),
            uninstall_mode: false,
        };

        let idle = StubProbes::idle();
        let wl = mole_core::state::load_whitelist(&home);
        let pkg = pkg_cache_entries(&home, &idle, &wl.patterns, &ctx);
        assert_eq!(pkg.len(), 2, "cargo + m2, gradle whitelisted: {pkg:?}");

        let cancel = Arc::new(AtomicBool::new(false));
        let out = build_plan(&home, &idle, &cancel, |_, _| {});
        let by_section = |key: &str| -> Vec<String> {
            out.plan
                .candidates
                .iter()
                .filter(|c| c.section == key)
                .map(|c| c.path.clone())
                .collect()
        };
        assert_eq!(by_section("pkg_cache").len(), 2);
        assert!(by_section("xcode_support")[0].ends_with("17.0 (21A329)"));
        assert!(by_section("xcode_archives")[0].ends_with("2024-01-01"));
        assert!(by_section("dev")[0].ends_with("App-abc"));
        assert_plan_is_eligible(&out.plan, &home);

        // Active tooling (Unknown here) withholds every Xcode section and
        // every guarded package cache.
        let unknown = StubProbes {
            owner_state: mole_core::probes::TriState::Unknown,
            sqlite_state: mole_core::probes::TriState::Idle,
        };
        assert!(pkg_cache_entries(&home, &unknown, &wl.patterns, &ctx).is_empty());
        let out2 = build_plan(&home, &unknown, &cancel, |_, _| {});
        assert!(out2
            .plan
            .candidates
            .iter()
            .all(|c| !c.section.starts_with("xcode") && c.section != "dev"));
    }

    /// iOS backups: each backup folder becomes a candidate with its device
    /// detail read from Info.plist; an unreadable plist still lists the folder.
    #[test]
    fn ios_backups_carry_device_detail() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let root = tmp
            .path()
            .join("Library/Application Support/MobileSync/Backup");
        let a = root.join("00008030-000A1B2C3D4E5F60");
        std::fs::create_dir_all(&a).unwrap();
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "Device Name".into(),
            plist::Value::String("Mike's iPhone".into()),
        );
        dict.insert(
            "Product Name".into(),
            plist::Value::String("iPhone 15".into()),
        );
        let ninety_days_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
        dict.insert(
            "Last Backup Date".into(),
            plist::Value::Date(plist::Date::from(ninety_days_ago)),
        );
        plist::Value::Dictionary(dict)
            .to_file_xml(a.join("Info.plist"))
            .unwrap();
        let b = root.join("no-plist");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(root.join("stray-file"), b"x").unwrap();
        let ctx = PolicyCtx {
            home: home.clone(),
            uninstall_mode: false,
        };

        let (paths, infos) = ios_backups(&home, &[], &ctx);
        assert_eq!(paths.len(), 2, "only directories: {paths:?}");
        let iphone = infos.iter().find(|i| i.device == "Mike's iPhone").unwrap();
        assert_eq!(iphone.product, "iPhone 15");
        assert!(matches!(iphone.last_backup_days, Some(89..=91)));
        let bare = infos.iter().find(|i| i.path.ends_with("no-plist")).unwrap();
        assert!(bare.device.is_empty() && bare.last_backup_days.is_none());
    }

    /// Darwin cache dir: reverse-DNS, old, non-Apple, idle-owner entries only.
    #[test]
    fn darwin_cache_entries_need_owner_and_age() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86_400);
        for name in [
            "com.example.old",
            "com.apple.old",
            "plain-old",
            "com.example.fresh",
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        for name in ["com.example.old", "com.apple.old", "plain-old"] {
            std::fs::File::open(root.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let ctx = PolicyCtx {
            home: root.to_string_lossy().into_owned(),
            uninstall_mode: false,
        };
        let got = darwin_cache_candidates(&root, &StubProbes::idle(), &[], &ctx);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["com.example.old".to_string()]);

        let unknown = StubProbes {
            owner_state: mole_core::probes::TriState::Unknown,
            sqlite_state: mole_core::probes::TriState::Idle,
        };
        assert!(darwin_cache_candidates(&root, &unknown, &[], &ctx).is_empty());
    }
}
