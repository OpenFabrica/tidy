// App state: the two-phase plan registry and the cancellation table.
// plan_* stores a plan under a server-generated id with a TTL; execute_*
// only ever accepts (plan_id, selection ⊆ plan) — never raw paths from the
// frontend. A new plan for the same feature invalidates the old one.

use mole_core::plan::DeletionPlan;
use mole_ops::scanutil::CancelFlag;
use mole_ops::uninstall::AppInfo;
use mole_ops::updates::{AppUpdate, UpdateCatalog};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Plans expire after this long; stale previews must be re-scanned.
const PLAN_TTL: Duration = Duration::from_secs(600);

/// Installed-app rows are served from cache this long; the per-bundle size
/// walk is what makes the Apps view slow to open, so it must not rerun on
/// every mount. Uninstall execution invalidates the cache early.
const APPS_TTL: Duration = Duration::from_secs(300);

/// Update actions must bind to a recent scan; stale source URLs, package
/// manager identities, and app paths are never accepted indefinitely.
const UPDATES_TTL: Duration = Duration::from_secs(900);

/// Reopening the Updates tab should reuse the last complete scan. Manual
/// refresh bypasses this shorter display-cache TTL.
const UPDATES_CACHE_TTL: Duration = Duration::from_secs(300);

/// Unknown cancellations are retained briefly as bounded tombstones so a
/// cancel IPC that wins the race against task registration is not lost.
const MAX_PRE_REGISTERED_CANCELS: usize = 64;

/// Cancellation registry and Analyze ownership live behind one mutex. Keeping
/// them together makes "cancel previous + register replacement" atomic.
#[derive(Default)]
struct CancellationRegistry {
    flags: HashMap<String, CancelFlag>,
    pre_cancelled: VecDeque<String>,
    active_analyze: Option<String>,
}

/// Ownership token for one Analyze generation. Dropping the command future,
/// returning an IPC error, or completing normally all retire the same task id
/// without clearing a newer generation.
pub struct AnalyzeTaskGuard<'a> {
    state: &'a AppState,
    task_id: String,
}

impl Drop for AnalyzeTaskGuard<'_> {
    fn drop(&mut self) {
        self.state.finish_analyze(&self.task_id);
    }
}

impl CancellationRegistry {
    fn take_pre_cancelled(&mut self, task_id: &str) -> bool {
        let Some(index) = self.pre_cancelled.iter().position(|id| id == task_id) else {
            return false;
        };
        self.pre_cancelled.remove(index);
        true
    }

    fn remember_pre_cancelled(&mut self, task_id: &str) {
        if self.pre_cancelled.iter().any(|id| id == task_id) {
            return;
        }
        if self.pre_cancelled.len() == MAX_PRE_REGISTERED_CANCELS {
            self.pre_cancelled.pop_front();
        }
        self.pre_cancelled.push_back(task_id.to_string());
    }

    fn register(&mut self, task_id: &str) -> CancelFlag {
        let was_cancelled = self.take_pre_cancelled(task_id);
        let flag = self
            .flags
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        if was_cancelled {
            flag.store(true, Ordering::Relaxed);
        }
        flag
    }
}

/// One cached inventory. `at == None` marks a stale seed (persisted rows from
/// a previous launch): served instantly for display, but always due for a
/// background refresh.
struct AppsCacheEntry {
    at: Option<Instant>,
    apps: Vec<AppInfo>,
}

/// Cached installed-app inventory, shared (via Arc) with blocking scan
/// threads so startup warmup and view opens serve one scan. Backed by a
/// persisted JSON file so the Apps view opens instantly on relaunch too.
#[derive(Default)]
pub struct AppsInventory {
    cache: Mutex<Option<AppsCacheEntry>>,
    /// Serializes scans: warmup and a first view open must never run the
    /// expensive inventory twice in parallel.
    scan_lock: Mutex<()>,
    /// Persisted-cache location, set once at startup seeding.
    persist_path: Mutex<Option<std::path::PathBuf>>,
}

impl AppsInventory {
    /// Fresh cached rows, if any.
    fn fresh(&self) -> Option<Vec<AppInfo>> {
        match self.cache.lock().unwrap().as_ref() {
            Some(entry) if entry.at.is_some_and(|at| at.elapsed() <= APPS_TTL) => {
                Some(entry.apps.clone())
            }
            _ => None,
        }
    }

    /// Any cached rows (fresh, expired, or persisted seed) plus a freshness
    /// flag. Stale rows are still correct enough to render instantly; the
    /// caller kicks a background refresh when `fresh` is false.
    pub fn cached(&self) -> Option<(Vec<AppInfo>, bool)> {
        self.cache.lock().unwrap().as_ref().map(|entry| {
            let fresh = entry.at.is_some_and(|at| at.elapsed() <= APPS_TTL);
            (entry.apps.clone(), fresh)
        })
    }

    /// Store a completed scan and mirror it to the persisted cache file.
    fn store(&self, apps: &[AppInfo]) {
        *self.cache.lock().unwrap() = Some(AppsCacheEntry {
            at: Some(Instant::now()),
            apps: apps.to_vec(),
        });
        self.persist(apps);
    }

    /// Return fresh cached rows or run `scan` under the scan lock. `scan`
    /// returns (rows, cacheable); a cancelled partial scan reports
    /// cacheable=false so it is returned once but never served again.
    pub fn get_or_scan(&self, scan: impl FnOnce() -> (Vec<AppInfo>, bool)) -> Vec<AppInfo> {
        if let Some(apps) = self.fresh() {
            return apps;
        }
        let _guard = self.scan_lock.lock().unwrap();
        // Re-check: another thread may have filled the cache while we waited.
        if let Some(apps) = self.fresh() {
            return apps;
        }
        let (apps, cacheable) = scan();
        if cacheable {
            self.store(&apps);
        }
        apps
    }

    /// Background refresh: rescan only when no other scan is running and the
    /// cache is still not fresh. Callers already returned stale rows to the
    /// UI, so skipping (lock busy / already refreshed) is always safe.
    pub fn refresh_if_idle(&self, scan: impl FnOnce() -> (Vec<AppInfo>, bool)) {
        let Ok(_guard) = self.scan_lock.try_lock() else {
            return;
        };
        if self.fresh().is_some() {
            return;
        }
        let (apps, cacheable) = scan();
        if cacheable {
            self.store(&apps);
        }
    }

    /// Load the persisted inventory from a previous launch as a stale seed,
    /// so the very first Apps open renders instantly while the startup scan
    /// refreshes in the background. Never overwrites a live cache.
    pub fn seed_from_disk(&self, home: &str) {
        let path = persisted_apps_path(home);
        *self.persist_path.lock().unwrap() = Some(path.clone());
        let Ok(data) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(apps) = serde_json::from_str::<Vec<AppInfo>>(&data) else {
            return;
        };
        // An empty file is the invalidation marker, never a valid seed.
        if apps.is_empty() {
            return;
        }
        let mut cache = self.cache.lock().unwrap();
        if cache.is_none() {
            *cache = Some(AppsCacheEntry { at: None, apps });
        }
    }

    /// Mirror a completed scan to disk (atomic tmp + rename; best-effort).
    fn persist(&self, apps: &[AppInfo]) {
        let Some(path) = self.persist_path.lock().unwrap().clone() else {
            return;
        };
        let Ok(json) = serde_json::to_string(apps) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Drop the cache (the installed set just changed on disk). The persisted
    /// mirror is overwritten with an empty list — same effect as deleting it
    /// (a relaunch must not resurrect uninstalled apps) without needing a
    /// delete call outside the mole-core sink.
    pub fn invalidate(&self) {
        *self.cache.lock().unwrap() = None;
        self.persist(&[]);
    }
}

/// Persisted inventory cache file, kept under the shared config dir (never
/// under ~/Library/Caches, which Mole's own clean sweep targets).
fn persisted_apps_path(home: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(home)
        .join(".config")
        .join(mole_core::brand::CONFIG_DIR)
        .join("desktop_apps_cache.json")
}

#[cfg(test)]
mod apps_inventory_tests {
    use super::*;

    /// Minimal row for cache round-trips.
    fn app(name: &str) -> AppInfo {
        AppInfo {
            name: name.to_string(),
            bundle_id: format!("test.{name}"),
            version: "1.0".to_string(),
            path: format!("/Applications/{name}.app"),
            size_kb: 42,
            protected: false,
            official_uninstaller: None,
            running: false,
        }
    }

    /// A persisted inventory must come back as a STALE seed (served instantly,
    /// still due for refresh), and a live scan must then replace it as fresh.
    #[test]
    fn persisted_seed_round_trip_is_stale_then_fresh() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().to_string_lossy().into_owned();

        let writer = AppsInventory::default();
        writer.seed_from_disk(&home); // records the persist path
        writer.get_or_scan(|| (vec![app("Alpha")], true));

        let reader = AppsInventory::default();
        reader.seed_from_disk(&home);
        let (apps, fresh) = reader.cached().expect("seed must load");
        assert_eq!(apps.len(), 1);
        assert!(!fresh, "a persisted seed is never fresh");

        reader.get_or_scan(|| (vec![app("Alpha"), app("Beta")], true));
        let (apps, fresh) = reader.cached().unwrap();
        assert_eq!(apps.len(), 2);
        assert!(fresh, "a completed scan is fresh");
    }

    /// Invalidation must also poison the persisted mirror so a relaunch
    /// cannot resurrect uninstalled apps from the old file.
    #[test]
    fn invalidate_clears_memory_and_persisted_mirror() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().to_string_lossy().into_owned();

        let inv = AppsInventory::default();
        inv.seed_from_disk(&home);
        inv.get_or_scan(|| (vec![app("Alpha")], true));
        inv.invalidate();
        assert!(inv.cached().is_none(), "memory cache must be gone");

        let relaunch = AppsInventory::default();
        relaunch.seed_from_disk(&home);
        assert!(
            relaunch.cached().is_none(),
            "the empty-marker file must not seed a relaunch"
        );
    }

    /// A cancelled partial scan is returned once but never cached or persisted.
    #[test]
    fn partial_scan_is_never_cached() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().to_string_lossy().into_owned();

        let inv = AppsInventory::default();
        inv.seed_from_disk(&home);
        let apps = inv.get_or_scan(|| (vec![app("Partial")], false));
        assert_eq!(apps.len(), 1);
        assert!(inv.cached().is_none(), "partial results must not be cached");

        let relaunch = AppsInventory::default();
        relaunch.seed_from_disk(&home);
        assert!(
            relaunch.cached().is_none(),
            "nothing may have been persisted"
        );
    }
}

/// How a stored plan executes: log tag, delete mode, policy mode.
#[derive(Clone)]
pub struct PlanConfig {
    pub feature: &'static str,
    pub command: &'static str,
    pub trash: bool,
    pub uninstall_mode: bool,
}

/// One stored plan.
pub struct StoredPlan {
    pub plan: DeletionPlan,
    pub config: PlanConfig,
    created: Instant,
}

/// What a Docker plan item removes; ids come only from the last scan.
#[derive(Clone)]
pub enum DockerTarget {
    Image(String),
    BuildCache,
}

/// Last Docker scan: item id → action. Same boundary rule as updates.
struct StoredDocker {
    plan_id: String,
    targets: HashMap<String, DockerTarget>,
    created: Instant,
}

/// Last tool scan (snapshots / simulators / brew): item id → action.
struct StoredTools {
    plan_id: String,
    targets: HashMap<String, mole_ops::tools::ToolTarget>,
    created: Instant,
}

/// Last update catalog used as the scan-to-action authorization boundary.
struct StoredUpdates {
    items: HashMap<String, AppUpdate>,
    catalog: UpdateCatalog,
    created: Instant,
}

/// Shared Tauri state.
#[derive(Default)]
pub struct AppState {
    plans: Mutex<HashMap<String, StoredPlan>>,
    cancels: Mutex<CancellationRegistry>,
    /// app path → PNG data URL (None = extraction already failed; don't retry).
    icons: Mutex<HashMap<String, Option<String>>>,
    /// Installed-app inventory cache (Arc so blocking threads can own it).
    pub apps: Arc<AppsInventory>,
    /// Latest update scan; actions accept ids from this map, never raw tokens,
    /// paths, or URLs from the webview.
    updates: Mutex<Option<StoredUpdates>>,
    /// Analyze subtree-size cache (Arc so blocking scans can own it).
    pub analyze_cache: Arc<mole_ops::analyze::SizeCache>,
    /// Latest Docker scan; execute_docker accepts ids from this map only.
    docker: Mutex<Option<StoredDocker>>,
    /// Latest tool scan; execute_tools accepts ids from this map only.
    tools: Mutex<Option<StoredTools>>,
    /// Single-flight update mutation guard.
    updates_busy: AtomicBool,
}

impl AppState {
    /// Store a plan, invalidating any previous plan of the same feature.
    /// Returns the new plan id.
    pub fn store_plan(&self, plan: DeletionPlan, config: PlanConfig) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let id = format!(
            "plan-{}-{}",
            config.feature,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let mut plans = self.plans.lock().unwrap();
        plans.retain(|_, p| p.config.feature != config.feature);
        plans.insert(
            id.clone(),
            StoredPlan {
                plan,
                config,
                created: Instant::now(),
            },
        );
        id
    }

    /// Fetch a plan for execution; distinguishes missing from expired so the
    /// error can name its cause.
    pub fn get_plan(&self, id: &str) -> Result<(DeletionPlan, PlanConfig), &'static str> {
        let plans = self.plans.lock().unwrap();
        match plans.get(id) {
            None => Err("plan_not_found"),
            Some(p) if p.created.elapsed() > PLAN_TTL => Err("plan_expired"),
            Some(p) => Ok((p.plan.clone(), p.config.clone())),
        }
    }

    /// Store the Docker scan and hand back its plan id.
    pub fn store_docker(&self, targets: HashMap<String, DockerTarget>) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let plan_id = format!("plan-docker-{}", COUNTER.fetch_add(1, Ordering::Relaxed));
        *self.docker.lock().unwrap() = Some(StoredDocker {
            plan_id: plan_id.clone(),
            targets,
            created: Instant::now(),
        });
        plan_id
    }

    /// Resolve a Docker item id against the stored scan (plan id must match
    /// and the scan must be fresh), or explain why not.
    pub fn docker_target(&self, plan_id: &str, id: &str) -> Result<DockerTarget, &'static str> {
        let docker = self.docker.lock().unwrap();
        match docker.as_ref() {
            None => Err("plan_not_found"),
            Some(d) if d.plan_id != plan_id => Err("plan_not_found"),
            Some(d) if d.created.elapsed() > PLAN_TTL => Err("plan_expired"),
            Some(d) => d.targets.get(id).cloned().ok_or("selection_mismatch"),
        }
    }

    /// Store the tool scan and hand back its plan id.
    pub fn store_tools(&self, targets: HashMap<String, mole_ops::tools::ToolTarget>) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let plan_id = format!("plan-tools-{}", COUNTER.fetch_add(1, Ordering::Relaxed));
        *self.tools.lock().unwrap() = Some(StoredTools {
            plan_id: plan_id.clone(),
            targets,
            created: Instant::now(),
        });
        plan_id
    }

    /// Resolve a tool item id against the stored scan (same freshness and
    /// plan-id rules as Docker), or explain why not.
    pub fn tool_target(
        &self,
        plan_id: &str,
        id: &str,
    ) -> Result<mole_ops::tools::ToolTarget, &'static str> {
        let tools = self.tools.lock().unwrap();
        match tools.as_ref() {
            None => Err("plan_not_found"),
            Some(t) if t.plan_id != plan_id => Err("plan_not_found"),
            Some(t) if t.created.elapsed() > PLAN_TTL => Err("plan_expired"),
            Some(t) => t.targets.get(id).cloned().ok_or("selection_mismatch"),
        }
    }

    /// Drop the tool scan after execution: snapshots and simulators no
    /// longer match what the preview showed.
    pub fn clear_tools(&self) {
        *self.tools.lock().unwrap() = None;
    }

    /// Drop a plan after successful execution (it no longer matches disk).
    pub fn invalidate_plan(&self, id: &str) {
        self.plans.lock().unwrap().remove(id);
    }

    /// Get or create the cancellation flag for a task id.
    pub fn cancel_flag(&self, task_id: &str) -> CancelFlag {
        self.cancels.lock().unwrap().register(task_id)
    }

    /// Atomically replace the active Analyze scan. A page reload may lose the
    /// frontend's in-memory task id, so the backend owns this single-flight
    /// boundary and always cancels the previous generation itself.
    pub fn begin_analyze(&self, task_id: &str) -> (CancelFlag, AnalyzeTaskGuard<'_>) {
        let mut cancels = self.cancels.lock().unwrap();
        if let Some(previous) = cancels.active_analyze.take() {
            if let Some(flag) = cancels.flags.get(&previous) {
                flag.store(true, Ordering::Relaxed);
            }
        }
        let flag = cancels.register(task_id);
        cancels.active_analyze = Some(task_id.to_string());
        (
            flag,
            AnalyzeTaskGuard {
                state: self,
                task_id: task_id.to_string(),
            },
        )
    }

    /// Finish one Analyze generation without disturbing a newer replacement.
    fn finish_analyze(&self, task_id: &str) {
        let mut cancels = self.cancels.lock().unwrap();
        cancels.flags.remove(task_id);
        if cancels.active_analyze.as_deref() == Some(task_id) {
            cancels.active_analyze = None;
        }
    }

    /// Cancel the current Analyze generation after a page reload, WebView
    /// teardown, or progress-channel failure.
    pub fn cancel_analyze(&self) -> bool {
        let cancels = self.cancels.lock().unwrap();
        let Some(task_id) = cancels.active_analyze.as_ref() else {
            return false;
        };
        let Some(flag) = cancels.flags.get(task_id) else {
            return false;
        };
        flag.store(true, Ordering::Relaxed);
        true
    }

    /// Flip a task's cancellation flag. Unknown ids return false but leave a
    /// bounded tombstone so a not-yet-registered task starts cancelled.
    pub fn cancel(&self, task_id: &str) -> bool {
        let mut cancels = self.cancels.lock().unwrap();
        if let Some(flag) = cancels.flags.get(task_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            cancels.remember_pre_cancelled(task_id);
            false
        }
    }

    /// Remove a finished task's flag.
    pub fn clear_task(&self, task_id: &str) {
        let mut cancels = self.cancels.lock().unwrap();
        cancels.flags.remove(task_id);
        cancels.pre_cancelled.retain(|id| id != task_id);
        if cancels.active_analyze.as_deref() == Some(task_id) {
            cancels.active_analyze = None;
        }
    }

    /// Cached icon lookup; Some(inner) when the answer (hit or known-miss)
    /// is already cached.
    pub fn icon_cached(&self, app_path: &str) -> Option<Option<String>> {
        self.icons.lock().unwrap().get(app_path).cloned()
    }

    /// Store an icon extraction result (including a permanent miss).
    pub fn icon_store(&self, app_path: &str, data_url: Option<String>) {
        self.icons
            .lock()
            .unwrap()
            .insert(app_path.to_string(), data_url);
    }

    /// Drop every cached icon so the next render re-extracts them from disk.
    /// Paired with `apps.invalidate()` behind the Apps view's refresh button:
    /// an icon that failed to extract is cached as a permanent miss, so without
    /// this the miss would survive for the whole session.
    pub fn icons_clear(&self) {
        self.icons.lock().unwrap().clear();
    }

    /// Replace the update authorization snapshot after a complete scan.
    pub fn store_updates(&self, catalog: &UpdateCatalog) {
        let items = catalog
            .updates
            .iter()
            .cloned()
            .map(|update| (update.id.clone(), update))
            .collect();
        *self.updates.lock().unwrap() = Some(StoredUpdates {
            items,
            catalog: catalog.clone(),
            created: Instant::now(),
        });
    }

    /// Complete recent catalog for instant tab remounts.
    pub fn cached_updates(&self) -> Option<UpdateCatalog> {
        self.updates
            .lock()
            .unwrap()
            .as_ref()
            .filter(|stored| stored.created.elapsed() <= UPDATES_CACHE_TTL)
            .map(|stored| stored.catalog.clone())
    }

    /// Resolve selected update ids from the recent backend-owned snapshot.
    pub fn get_updates(&self, ids: &[String]) -> Result<Vec<AppUpdate>, &'static str> {
        let updates = self.updates.lock().unwrap();
        let Some(stored) = updates.as_ref() else {
            return Err("update_scan_missing");
        };
        if stored.created.elapsed() > UPDATES_TTL {
            return Err("update_scan_expired");
        }
        let mut selected = Vec::with_capacity(ids.len());
        let mut unique = std::collections::HashSet::new();
        for id in ids {
            if !unique.insert(id) {
                return Err("duplicate_update_id");
            }
            let Some(update) = stored.items.get(id) else {
                return Err("unknown_update_id");
            };
            selected.push(update.clone());
        }
        Ok(selected)
    }

    /// Confirm one ignore/unignore target came from the latest scan.
    pub fn has_update(&self, id: &str) -> bool {
        self.updates.lock().unwrap().as_ref().is_some_and(|stored| {
            stored.created.elapsed() <= UPDATES_TTL && stored.items.contains_key(id)
        })
    }

    /// Keep the memory catalog synchronized after durable ignore changes.
    pub fn set_update_ignored(&self, id: &str, ignored: bool) {
        let mut updates = self.updates.lock().unwrap();
        let Some(stored) = updates.as_mut() else {
            return;
        };
        if let Some(update) = stored.items.get_mut(id) {
            update.ignored = ignored;
        }
        if let Some(update) = stored
            .catalog
            .updates
            .iter_mut()
            .find(|update| update.id == id)
        {
            update.ignored = ignored;
        }
    }

    /// Invalidate update actions after any attempted mutation; the next click
    /// must refresh and bind to current package/app state.
    pub fn clear_updates(&self) {
        *self.updates.lock().unwrap() = None;
    }

    /// Acquire the update mutation lease.
    pub fn begin_updates(&self) -> bool {
        self.updates_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    /// Release the update mutation lease on every completion/failure path.
    pub fn finish_updates(&self) {
        self.updates_busy.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod analyze_state_tests {
    use super::*;

    #[test]
    fn newer_analyze_scan_cancels_the_previous_scan() {
        let state = AppState::default();
        let (first, first_guard) = state.begin_analyze("analyze-first");
        let (second, _second_guard) = state.begin_analyze("analyze-second");

        assert!(first.load(Ordering::Relaxed));
        assert!(!second.load(Ordering::Relaxed));

        // A late completion from the superseded command must not clear the
        // current scan's identity or make page-reload cancellation miss it.
        drop(first_guard);
        assert!(state.cancel_analyze());
        assert!(second.load(Ordering::Relaxed));
    }

    #[test]
    fn cancellation_before_registration_is_preserved() {
        let state = AppState::default();

        assert!(!state.cancel("not-registered-yet"));
        let cancel = state.cancel_flag("not-registered-yet");

        assert!(cancel.load(Ordering::Relaxed));
        state.clear_task("not-registered-yet");
    }
}

#[cfg(test)]
mod update_state_tests {
    use super::*;

    #[test]
    fn complete_catalog_is_cached_and_update_mutation_is_single_flight() {
        let state = AppState::default();
        let catalog = UpdateCatalog {
            updates: Vec::new(),
            up_to_date: Vec::new(),
            warnings: vec!["source:unavailable".into()],
            checked_at: 42,
        };
        state.store_updates(&catalog);
        let cached = state.cached_updates().expect("fresh catalog");
        assert_eq!(cached.checked_at, 42);
        assert_eq!(cached.warnings, ["source:unavailable"]);

        assert!(state.begin_updates());
        assert!(!state.begin_updates(), "second mutation must be refused");
        state.finish_updates();
        assert!(
            state.begin_updates(),
            "lease must be reusable after release"
        );
        state.finish_updates();
    }
}
