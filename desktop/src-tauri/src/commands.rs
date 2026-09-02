// IPC commands: thin translations from the frontend to mole-ops, enforcing
// the two-phase contract — plan_* returns a stored preview, execute_plan only
// accepts (plan_id, selection ⊆ plan). Heavy work runs on blocking threads
// with progress channels and cooperative cancellation.

use crate::dto::{summarize, PlanItem, PlanSection, PlanSummary, ScanEvent};
use crate::error::IpcError;
use crate::state::{AppState, PlanConfig};
use mole_core::probes::SystemProbes;
use mole_core::sink::DeleteMode;
use mole_macos::{AdminRunner, FinderTrash};
use mole_ops::engine::{self, ExecItem, ExecOptions, ExecReport, Providers};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{Manager, State};

/// App/build metadata for the About row.
#[derive(Serialize)]
pub struct AppMeta {
    pub app_version: String,
    /// SHA-256 of the protection data the core was generated from — visible
    /// proof of which policy snapshot this build enforces.
    pub protection_data_sha256: String,
    /// Whether the privileged helper is installed and usable.
    pub helper_available: bool,
    /// The invoking user's home directory (analyze's default root).
    pub home: String,
}

/// Version and policy provenance.
#[tauri::command]
pub fn app_meta() -> AppMeta {
    use mole_core::providers::PrivilegedRunner;
    AppMeta {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        protection_data_sha256: mole_core::policy::data::DATA_SHA256.to_string(),
        helper_available: AdminRunner.available(),
        home: crate::home(),
    }
}

/// One-shot system snapshot for the dashboard.
#[tauri::command]
pub async fn status_snapshot() -> Result<mole_ops::status::StatusSnapshot, IpcError> {
    tauri::async_runtime::spawn_blocking(mole_ops::status::snapshot)
        .await
        .map_err(|e| IpcError::new("io", e.to_string()))
}

/// Whitelist payload: valid patterns plus the warnings invalid lines produced.
#[derive(Serialize)]
pub struct WhitelistPayload {
    pub patterns: Vec<String>,
    pub warnings: Vec<String>,
}

/// Load the user's cleanup whitelist (same file the CLI reads).
#[tauri::command]
pub fn whitelist_get() -> WhitelistPayload {
    let wl = mole_ops::whitelist::get(&crate::home());
    WhitelistPayload {
        patterns: wl.patterns,
        warnings: wl.warnings,
    }
}

/// Save the whitelist; returns validation warnings for rejected entries.
#[tauri::command]
pub fn whitelist_set(patterns: Vec<String>) -> Result<Vec<String>, IpcError> {
    mole_ops::whitelist::set(&crate::home(), &patterns).map_err(IpcError::from)
}

/// Purge scan directories (None = defaults in effect).
#[tauri::command]
pub fn purge_paths_get() -> Option<Vec<String>> {
    mole_core::state::load_purge_paths(&crate::home())
}

/// Touch ID for sudo: read-only status.
#[tauri::command]
pub fn touchid_status() -> mole_ops::touchid::TouchIdStatus {
    mole_ops::touchid::status()
}

// ---------------------------------------------------------------------------
// analyze
// ---------------------------------------------------------------------------

/// One-level directory listing with parallel physical sizes. `fresh`
/// (manual rescan) drops the subtree-size cache first.
#[tauri::command]
pub async fn analyze_scan(
    state: State<'_, AppState>,
    root: String,
    task_id: String,
    fresh: bool,
    on_progress: Channel<ScanEvent>,
) -> Result<mole_ops::analyze::DirListing, IpcError> {
    let _busy = crate::tray_anim::busy();
    let (cancel, _task_guard) = state.begin_analyze(&task_id);
    let progress_cancel = cancel.clone();
    let cache = state.analyze_cache.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        mole_ops::analyze::scan_dir(&root, &cancel, &cache, fresh, |done, path| {
            // The label carries the directory currently being read: on a big
            // tree the count alone moves too slowly to look alive.
            if on_progress
                .send(ScanEvent {
                    label: path.to_string_lossy().into_owned(),
                    count: done,
                })
                .is_err()
            {
                // A failed WebView eval means the JavaScript channel no longer
                // has a receiver. Stop the scan instead of orphaning its walk.
                progress_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    Ok(result)
}

/// Ad-hoc deletion plan from analyze-picked paths (Trash-only by contract).
#[tauri::command]
pub async fn plan_delete_paths(
    state: State<'_, AppState>,
    paths: Vec<String>,
    task_id: String,
) -> Result<PlanSummary, IpcError> {
    // Deletion is imminent: every cached subtree size below is about to be wrong.
    state.analyze_cache.clear();
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let plan = tauri::async_runtime::spawn_blocking(move || {
        mole_ops::analyze::plan_delete_paths(&paths, &cancel)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    let id = state.store_plan(
        plan.clone(),
        PlanConfig {
            feature: "adhoc",
            command: "analyze",
            trash: true,
            uninstall_mode: false,
        },
    );
    Ok(summarize(id, &plan))
}

// ---------------------------------------------------------------------------
// clean / purge / installer / uninstall plans
// ---------------------------------------------------------------------------

/// Clean plan payload: preview plus the blocked-by-running-apps hint.
#[derive(Serialize)]
pub struct CleanPlanPayload {
    pub summary: PlanSummary,
    pub blocked: mole_ops::clean::BlockedCaches,
    /// Per-backup device detail for the iOS backups section badges.
    pub backups: Vec<mole_ops::clean::BackupInfo>,
}

/// Full clean plan; sections stream through the progress channel.
#[tauri::command]
pub async fn plan_clean(
    state: State<'_, AppState>,
    task_id: String,
    on_progress: Channel<ScanEvent>,
) -> Result<CleanPlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let cancel_probe = cancel.clone();
    let home = crate::home();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let probes = SystemProbes::new();
        mole_ops::clean::build_plan(&home, &probes, &cancel, |section, count| {
            let _ = on_progress.send(ScanEvent {
                label: section.to_string(),
                count,
            });
        })
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    // Cancelled scans must not become executable plans.
    if cancel_probe.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }
    let id = state.store_plan(
        out.plan.clone(),
        PlanConfig {
            feature: "clean",
            command: "clean",
            trash: false,
            uninstall_mode: false,
        },
    );
    Ok(CleanPlanPayload {
        summary: summarize(id, &out.plan),
        blocked: out.blocked,
        backups: out.backups,
    })
}

/// Trash contents plan. Stored WITHOUT a Trash route: emptying the Trash is
/// permanent by definition, and execute_plan only honors trash_override for
/// the clean feature, so nothing can turn this into a no-op move.
#[tauri::command]
pub async fn plan_trash(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<PlanSummary, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let cancel_probe = cancel.clone();
    let home = crate::home();
    let plan =
        tauri::async_runtime::spawn_blocking(move || mole_ops::trash::build_plan(&home, &cancel))
            .await
            .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    // Cancelled scans must not become executable plans.
    if cancel_probe.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }
    let id = state.store_plan(
        plan.clone(),
        PlanConfig {
            feature: "trash",
            command: "trash",
            trash: false,
            uninstall_mode: false,
        },
    );
    Ok(summarize(id, &plan))
}

/// Orphan plan payload: preview plus per-path bundle id / idle days.
#[derive(Serialize)]
pub struct OrphanPlanPayload {
    pub summary: PlanSummary,
    pub orphans: Vec<mole_ops::orphans::OrphanInfo>,
}

/// Leftovers of apps that are no longer installed. Trash-routed: the
/// evidence is strong but indirect, so every removal stays recoverable.
#[tauri::command]
pub async fn plan_orphans(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrphanPlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let cancel_probe = cancel.clone();
    let home = crate::home();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let probes = SystemProbes::new();
        mole_ops::orphans::build_plan(&home, &probes, &cancel)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    if cancel_probe.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }
    let id = state.store_plan(
        out.plan.clone(),
        PlanConfig {
            feature: "orphans",
            command: "orphans",
            trash: true,
            uninstall_mode: false,
        },
    );
    Ok(OrphanPlanPayload {
        summary: summarize(id, &out.plan),
        orphans: out.orphans,
    })
}

/// Tool plan payload: preview plus the raw tool rows (kind / admin / detail).
#[derive(Serialize)]
pub struct ToolPlanPayload {
    pub summary: PlanSummary,
    pub items: Vec<mole_ops::tools::ToolItem>,
}

/// Command-backed reclaimables (Time Machine local snapshots, unavailable
/// simulators, Homebrew cleanup) as one "tools" section. Same boundary rule
/// as Docker: execute_tools accepts only ids from this scan.
#[tauri::command]
pub async fn plan_tools(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<ToolPlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let cancel_probe = cancel.clone();
    let home = crate::home();
    let items = tauri::async_runtime::spawn_blocking(move || {
        let probes = SystemProbes::new();
        mole_ops::tools::scan(&home, &probes, &cancel)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    if cancel_probe.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }

    let mut targets = std::collections::HashMap::new();
    let mut rows = Vec::new();
    for item in &items {
        targets.insert(item.id.clone(), item.target.clone());
        rows.push(PlanItem {
            id: item.id.clone(),
            path: item.label.clone(),
            size_kb: item.size_kb,
            item_count: 1,
            // Snapshots elevate through the admin runner; the UI gates them
            // on helper availability exactly like system-scope files.
            scope: if item.requires_admin {
                "system"
            } else {
                "user"
            },
        });
    }
    let total_kb = rows.iter().map(|i| i.size_kb.unwrap_or(0)).sum();
    let count = rows.len();
    let plan_id = state.store_tools(targets);
    Ok(ToolPlanPayload {
        summary: PlanSummary {
            plan_id,
            sections: vec![PlanSection {
                title: "tools".into(),
                items: rows,
                total_kb,
            }],
            total_kb,
            count,
        },
        items,
    })
}

/// Run the selected tool items through their own CLIs. Each id must come
/// from the stored scan; snapshots go through the privileged runner.
#[tauri::command]
pub async fn execute_tools(
    state: State<'_, AppState>,
    plan_id: String,
    selection: Vec<String>,
    on_progress: Channel<ExecItem>,
) -> Result<ExecReport, IpcError> {
    let _busy = crate::tray_anim::busy();
    let mut targets = Vec::new();
    for id in &selection {
        let target = state.tool_target(&plan_id, id).map_err(|code| {
            IpcError::new(code_static(code), "tool scan unavailable — re-run the scan")
        })?;
        targets.push((id.clone(), target));
    }
    let home = crate::home();
    let report = tauri::async_runtime::spawn_blocking(move || {
        let privileged = AdminRunner;
        let mut report = ExecReport {
            items: Vec::new(),
            total_freed_kb: 0,
            skipped: 0,
            failed: 0,
        };
        for (id, target) in targets {
            let result = mole_ops::tools::execute(&target, &home, &privileged);
            let item = match result {
                Ok(()) => ExecItem {
                    id: id.clone(),
                    path: id,
                    outcome: "removed".into(),
                    size_kb: None,
                    error: None,
                },
                Err(e) => {
                    report.failed += 1;
                    ExecItem {
                        id: id.clone(),
                        path: id,
                        outcome: "failed".into(),
                        size_kb: None,
                        error: Some(e),
                    }
                }
            };
            let _ = on_progress.send(item.clone());
            report.items.push(item);
        }
        report
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    // Any real run invalidates the scan (disk no longer matches).
    state.clear_tools();
    Ok(report)
}

/// Reveal a path in Finder (read-only convenience).
#[tauri::command]
pub fn reveal_in_finder(path: String) -> Result<(), IpcError> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| IpcError::new("io", e.to_string()))
}

/// Installed app inventory. Never blocks when any cache exists (fresh,
/// expired, or the persisted seed from a previous launch): stale rows render
/// instantly and a background rescan refreshes them for the next mount. Only
/// a truly cold start (first launch ever, warmup not done) pays the scan
/// inline. Uninstall execution invalidates both cache layers.
#[tauri::command]
pub async fn list_apps(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<mole_ops::uninstall::AppInfo>, IpcError> {
    let _busy = crate::tray_anim::busy();
    let inventory = state.apps.clone();
    let home = crate::home();

    if let Some((apps, fresh)) = inventory.cached() {
        if !fresh {
            // Stale-while-revalidate: return the stale rows now, rescan in
            // the background (skipped when another scan already runs).
            tauri::async_runtime::spawn_blocking(move || {
                let cancel: mole_ops::scanutil::CancelFlag =
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let probes = SystemProbes::new();
                inventory.refresh_if_idle(|| {
                    (
                        mole_ops::uninstall::inventory(&home, &probes, &cancel),
                        true,
                    )
                });
            });
        }
        state.clear_task(&task_id);
        return Ok(apps);
    }

    // Cold start: no cache at all — scan inline (cancellable).
    let cancel = state.cancel_flag(&task_id);
    let apps = tauri::async_runtime::spawn_blocking(move || {
        inventory.get_or_scan(|| {
            let probes = SystemProbes::new();
            let apps = mole_ops::uninstall::inventory(&home, &probes, &cancel);
            // A cancelled scan may be partial: return it, never cache it.
            let cacheable = !cancel.load(std::sync::atomic::Ordering::Relaxed);
            (apps, cacheable)
        })
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    Ok(apps)
}

/// Uninstall plan payload: preview plus withheld-leftover notes.
#[derive(Serialize)]
pub struct UninstallPlanPayload {
    pub summary: PlanSummary,
    pub notes: Vec<mole_ops::uninstall::UninstallNote>,
}

/// Uninstall plan for the selected app bundle paths.
#[tauri::command]
pub async fn plan_uninstall(
    state: State<'_, AppState>,
    app_paths: Vec<String>,
    task_id: String,
) -> Result<UninstallPlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let home = crate::home();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let probes = SystemProbes::new();
        mole_ops::uninstall::plan_uninstall(&home, &probes, &app_paths, &cancel)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    let id = state.store_plan(
        out.plan.clone(),
        PlanConfig {
            feature: "uninstall",
            command: "uninstall",
            trash: true,
            uninstall_mode: true,
        },
    );
    Ok(UninstallPlanPayload {
        summary: summarize(id, &out.plan),
        notes: out.notes,
    })
}

/// Purge plan payload: preview plus per-project blocker badges.
#[derive(Serialize)]
pub struct PurgePlanPayload {
    pub summary: PlanSummary,
    pub projects: Vec<mole_ops::purge::ProjectReport>,
}

/// Purge plan across the configured project containers.
#[tauri::command]
pub async fn plan_purge(
    state: State<'_, AppState>,
    task_id: String,
    on_progress: Channel<ScanEvent>,
) -> Result<PurgePlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let cancel_probe = cancel.clone();
    let home = crate::home();
    let out = tauri::async_runtime::spawn_blocking(move || {
        mole_ops::purge::build_plan(&home, &cancel, |project| {
            let _ = on_progress.send(ScanEvent {
                label: project.to_string(),
                count: 0,
            });
        })
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    // Cancelled scans must not become executable plans.
    if cancel_probe.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }
    let id = state.store_plan(
        out.plan.clone(),
        PlanConfig {
            feature: "purge",
            command: "purge",
            trash: false,
            uninstall_mode: false,
        },
    );
    Ok(PurgePlanPayload {
        summary: summarize(id, &out.plan),
        projects: out.projects,
    })
}

/// Docker plan payload: preview plus the raw image rows (age / in-use).
#[derive(Serialize)]
pub struct DockerPlanPayload {
    pub summary: PlanSummary,
    pub images: Vec<mole_ops::docker::DockerImage>,
}

/// Docker images + unused build cache as one "docker" section. Item ids are
/// the image ids (or "build-cache"); execute_docker only accepts those.
#[tauri::command]
pub async fn plan_docker(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<DockerPlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let home = crate::home();
    let catalog = tauri::async_runtime::spawn_blocking(move || mole_ops::docker::scan(&home))
        .await
        .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }
    let catalog = catalog.ok_or_else(|| IpcError::new("unavailable", "docker not running"))?;

    let mut targets = std::collections::HashMap::new();
    let mut items = Vec::new();
    for img in &catalog.images {
        targets.insert(
            img.id.clone(),
            crate::state::DockerTarget::Image(img.id.clone()),
        );
        let short: String = img
            .id
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect();
        let name = if img.dangling {
            format!("<none> ({short})")
        } else {
            format!("{}:{}", img.repository, img.tag)
        };
        items.push(PlanItem {
            id: img.id.clone(),
            path: name,
            size_kb: Some(img.size_kb),
            item_count: 1,
            scope: "user",
        });
    }
    if catalog.build_cache_unused_kb > 0 {
        targets.insert("build-cache".into(), crate::state::DockerTarget::BuildCache);
        items.push(PlanItem {
            id: "build-cache".into(),
            path: "Docker build cache".into(),
            size_kb: Some(catalog.build_cache_unused_kb),
            item_count: 1,
            scope: "user",
        });
    }
    let total_kb = items.iter().map(|i| i.size_kb.unwrap_or(0)).sum();
    let count = items.len();
    let plan_id = state.store_docker(targets);
    Ok(DockerPlanPayload {
        summary: PlanSummary {
            plan_id,
            sections: vec![PlanSection {
                title: "docker".into(),
                items,
                total_kb,
            }],
            total_kb,
            count,
        },
        images: catalog.images,
    })
}

/// Remove the selected Docker items through the docker CLI. Images still
/// used by a container are refused by docker and reported as failed.
#[tauri::command]
pub async fn execute_docker(
    state: State<'_, AppState>,
    plan_id: String,
    selection: Vec<String>,
    on_progress: Channel<ExecItem>,
) -> Result<ExecReport, IpcError> {
    let _busy = crate::tray_anim::busy();
    let mut targets = Vec::new();
    for id in &selection {
        let target = state.docker_target(&plan_id, id).map_err(|code| {
            IpcError::new(
                code_static(code),
                "docker scan unavailable — re-run the scan",
            )
        })?;
        targets.push((id.clone(), target));
    }
    let home = crate::home();
    let report = tauri::async_runtime::spawn_blocking(move || {
        let mut report = ExecReport {
            items: Vec::new(),
            total_freed_kb: 0,
            skipped: 0,
            failed: 0,
        };
        let Some(docker) = mole_ops::docker::find_docker(&home) else {
            report.failed = targets.len() as u64;
            return report;
        };
        for (id, target) in targets {
            let (path, result) = match &target {
                crate::state::DockerTarget::Image(image) => (
                    image.clone(),
                    mole_ops::docker::remove_image(&docker, image),
                ),
                crate::state::DockerTarget::BuildCache => (
                    "Docker build cache".to_string(),
                    mole_ops::docker::prune_build_cache(&docker),
                ),
            };
            let item = match result {
                Ok(()) => ExecItem {
                    id,
                    path,
                    outcome: "removed".into(),
                    size_kb: None,
                    error: None,
                },
                Err(e) => {
                    report.failed += 1;
                    ExecItem {
                        id,
                        path,
                        outcome: "failed".into(),
                        size_kb: None,
                        error: Some(e),
                    }
                }
            };
            let _ = on_progress.send(item.clone());
            report.items.push(item);
        }
        report
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    Ok(report)
}

/// Installer artifact plan (Trash-routed).
#[tauri::command]
pub async fn plan_installer(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<PlanSummary, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let cancel_probe = cancel.clone();
    let home = crate::home();
    let plan = tauri::async_runtime::spawn_blocking(move || {
        mole_ops::installer::build_plan(&home, &cancel)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    // Cancelled scans must not become executable plans.
    if cancel_probe.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IpcError::new("cancelled", "scan cancelled"));
    }
    let id = state.store_plan(
        plan.clone(),
        PlanConfig {
            feature: "installer",
            command: "installer",
            trash: true,
            uninstall_mode: false,
        },
    );
    Ok(summarize(id, &plan))
}

// ---------------------------------------------------------------------------
// execute (shared by every destructive feature)
// ---------------------------------------------------------------------------

/// Execute a stored plan. Refuses unknown/expired plans and any selection id
/// outside the plan (two-phase contract); dry_run walks the identical path
/// with no mutation.
#[tauri::command]
pub async fn execute_plan(
    state: State<'_, AppState>,
    plan_id: String,
    selection: Vec<String>,
    dry_run: bool,
    trash_override: Option<bool>,
    on_progress: Channel<ExecItem>,
) -> Result<ExecReport, IpcError> {
    let _busy = crate::tray_anim::busy();
    let (plan, config) = state
        .get_plan(&plan_id)
        .map_err(|code| IpcError::new(code_static(code), "plan unavailable — re-run the scan"))?;

    // Selection must be a subset of the stored plan.
    for id in &selection {
        if plan.find(id).is_none() {
            return Err(IpcError::new(
                "selection_mismatch",
                format!("selection id {id} is not in the plan"),
            ));
        }
    }

    // The delete-mode setting only widens recoverability guarantees for
    // clean (cache deletes); uninstall stays Trash-routed unconditionally.
    let trash = match trash_override {
        Some(t) if config.feature == "clean" => t,
        _ => config.trash,
    };
    let opts = ExecOptions {
        home: crate::home(),
        command: config.command.to_string(),
        mode: if trash {
            DeleteMode::Trash
        } else {
            DeleteMode::Permanent
        },
        dry_run,
        uninstall_mode: config.uninstall_mode,
    };
    let report = tauri::async_runtime::spawn_blocking(move || {
        let trash = FinderTrash;
        let privileged = AdminRunner;
        let probes = SystemProbes::new();
        let providers = Providers {
            trash: &trash,
            privileged: &privileged,
            probes: &probes,
        };
        engine::execute(&plan, &selection, &opts, &providers, |item| {
            let _ = on_progress.send(item.clone());
        })
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;

    // A real run invalidates the plan (disk no longer matches the preview).
    if !dry_run {
        state.invalidate_plan(&plan_id);
        // An uninstall changes the installed set; drop the apps cache so the
        // next Apps view open rescans instead of showing removed apps.
        if config.uninstall_mode {
            state.apps.invalidate();
        }
    }
    Ok(report)
}

/// Map a state error string to its static code.
fn code_static(code: &str) -> &'static str {
    match code {
        "plan_expired" => "plan_expired",
        _ => "plan_not_found",
    }
}

/// Cancel a running scan by its task id.
#[tauri::command]
pub fn cancel_task(state: State<'_, AppState>, task_id: String) -> bool {
    state.cancel(&task_id)
}

/// Full detail for one process (modal click-through).
#[tauri::command]
pub async fn process_detail(pid: u32) -> Result<Option<mole_ops::status::ProcessDetail>, IpcError> {
    tauri::async_runtime::spawn_blocking(move || mole_ops::status::process_detail(pid))
        .await
        .map_err(|e| IpcError::new("io", e.to_string()))
}

/// Send SIGTERM/SIGKILL to a process (explicit user action from the modal).
#[tauri::command]
pub fn signal_process(pid: u32, force: bool) -> Result<(), IpcError> {
    mole_ops::status::signal_process(pid, force).map_err(|cause| IpcError::new("io", cause))
}

// ---------------------------------------------------------------------------
// apps view extras: icons, selection-free preview, updates, login items
// ---------------------------------------------------------------------------

/// App icon as a PNG data URL, cached per path (misses cached too, so a
/// catalog-only app is probed exactly once).
#[tauri::command]
pub async fn app_icon(
    state: State<'_, AppState>,
    app_path: String,
) -> Result<Option<String>, IpcError> {
    if let Some(cached) = state.icon_cached(&app_path) {
        return Ok(cached);
    }
    let path = app_path.clone();
    let data_url = tauri::async_runtime::spawn_blocking(move || {
        mole_ops::appmeta::app_icon_png(&path).map(|bytes| {
            use base64::Engine as _;
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        })
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.icon_store(&app_path, data_url.clone());
    Ok(data_url)
}

/// Drop the cached app inventory and every extracted icon.
///
/// Backs the Apps view's manual refresh: both caches are deliberately sticky
/// (the inventory is even mirrored to disk for an instant cold start), so an
/// app installed or re-signed while Tidy was running — or an icon that failed
/// to extract once — stays stale until something clears them.
#[tauri::command]
pub fn refresh_app_cache(state: State<'_, AppState>) {
    state.apps.invalidate();
    state.icons_clear();
}

/// Leftover preview for the expandable app rows: same discovery as
/// plan_uninstall but nothing is stored — expanding is not selecting, and a
/// preview must never become executable by id.
#[tauri::command]
pub async fn preview_uninstall(
    state: State<'_, AppState>,
    app_paths: Vec<String>,
    task_id: String,
) -> Result<UninstallPlanPayload, IpcError> {
    let _busy = crate::tray_anim::busy();
    let cancel = state.cancel_flag(&task_id);
    let home = crate::home();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let probes = SystemProbes::new();
        mole_ops::uninstall::plan_uninstall(&home, &probes, &app_paths, &cancel)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;
    state.clear_task(&task_id);
    Ok(UninstallPlanPayload {
        summary: summarize(String::new(), &out.plan),
        notes: out.notes,
    })
}

/// Instant-paint payload: the newest known catalog plus whether its ids are
/// live (in-memory snapshot within TTL) or display-only (persisted last scan).
#[derive(Serialize)]
pub struct CachedUpdatesPayload {
    pub catalog: mole_ops::updates::UpdateCatalog,
    pub live: bool,
}

/// Return the newest known catalog without scanning: memory cache first, then
/// the persisted last scan. live=false results are display-only — update and
/// ignore actions still require a fresh list_app_updates snapshot.
#[tauri::command]
pub fn cached_app_updates(state: State<'_, AppState>) -> Option<CachedUpdatesPayload> {
    if let Some(catalog) = state.cached_updates() {
        return Some(CachedUpdatesPayload {
            catalog,
            live: true,
        });
    }
    mole_ops::updates::load_catalog(&crate::home()).map(|catalog| CachedUpdatesPayload {
        catalog,
        live: false,
    })
}

/// Scan Homebrew, Mac App Store, Sparkle, and Electron update channels. The
/// returned ids are stored server-side as the authorization snapshot for
/// later update/ignore actions.
#[tauri::command]
pub async fn list_app_updates(
    state: State<'_, AppState>,
    task_id: String,
    force: bool,
) -> Result<mole_ops::updates::UpdateCatalog, IpcError> {
    if !force {
        if let Some(cached) = state.cached_updates() {
            return Ok(cached);
        }
    }
    let _busy = crate::tray_anim::busy();
    let home = crate::home();
    let inventory = state.apps.clone();
    let cancel = state.cancel_flag(&task_id);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let probes = SystemProbes::new();
        let apps = inventory.get_or_scan(|| {
            let apps = mole_ops::uninstall::inventory(&home, &probes, &cancel);
            let cacheable = !cancel.load(std::sync::atomic::Ordering::Relaxed);
            (apps, cacheable)
        });
        let catalog = mole_ops::updates::scan(&home, &apps, &cancel);
        let cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
        // Persist complete scans only; a cancelled partial catalog must not
        // become the next launch's instant paint.
        if !cancelled {
            mole_ops::updates::save_catalog(&home, &catalog);
        }
        (catalog, cancelled)
    })
    .await
    .map_err(|error| IpcError::new("io", error.to_string()))?;
    state.clear_task(&task_id);
    if result.1 {
        return Err(IpcError::new("cancelled", "update scan cancelled"));
    }
    state.store_updates(&result.0);
    Ok(result.0)
}

/// Run selected updates in request order. Homebrew updates execute directly;
/// App Store/Sparkle/Electron rows delegate to their original updater.
#[tauri::command]
pub async fn run_app_updates(
    state: State<'_, AppState>,
    update_ids: Vec<String>,
    task_id: String,
) -> Result<Vec<mole_ops::updates::UpdateResult>, IpcError> {
    if update_ids.is_empty() {
        return Err(IpcError::new(
            "empty_selection",
            "select at least one update",
        ));
    }
    if !state.begin_updates() {
        return Err(IpcError::new("busy", "another update operation is running"));
    }
    let _busy = crate::tray_anim::busy();
    let selected = match state.get_updates(&update_ids) {
        Ok(selected) => selected,
        Err(cause) => {
            state.finish_updates();
            return Err(IpcError::new(cause, "refresh updates and try again"));
        }
    };
    let cancel = state.cancel_flag(&task_id);
    let result = tauri::async_runtime::spawn_blocking(move || {
        mole_ops::updates::run_updates(&selected, &cancel)
    })
    .await;
    state.clear_task(&task_id);
    state.finish_updates();
    state.clear_updates();
    let result = result.map_err(|error| IpcError::new("io", error.to_string()))?;
    if result
        .iter()
        .any(|item| item.outcome == "updated" || item.outcome == "external")
    {
        state.apps.invalidate();
    }
    Ok(result)
}

/// Persist one ignored-update key after proving it came from the latest scan.
#[tauri::command]
pub fn set_app_update_ignored(
    state: State<'_, AppState>,
    update_id: String,
    ignored: bool,
) -> Result<(), IpcError> {
    if !state.has_update(&update_id) {
        return Err(IpcError::new(
            "unknown_update_id",
            "refresh updates and try again",
        ));
    }
    mole_ops::updates::set_ignored(&crate::home(), &update_id, ignored)
        .map_err(|cause| IpcError::new("io", cause))?;
    state.set_update_ignored(&update_id, ignored);
    Ok(())
}

/// Login items (launch agents/daemons) with launchd enabled state.
#[tauri::command]
pub async fn list_login_items() -> Result<Vec<mole_ops::appmeta::LoginItem>, IpcError> {
    let home = crate::home();
    tauri::async_runtime::spawn_blocking(move || mole_ops::appmeta::list_login_items(&home))
        .await
        .map_err(|e| IpcError::new("io", e.to_string()))
}

/// App-embedded login items and helpers (display-only; owned by their apps).
#[tauri::command]
pub async fn list_embedded_login_items(
) -> Result<Vec<mole_ops::appmeta::EmbeddedLoginItem>, IpcError> {
    let home = crate::home();
    tauri::async_runtime::spawn_blocking(move || {
        mole_ops::appmeta::list_embedded_login_items(&home)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))
}

/// Toggle a USER-scope launchd agent. System-scope items refuse with the
/// named requires_admin cause (no privileged helper yet).
#[tauri::command]
pub async fn set_login_item_enabled(
    label: String,
    path: String,
    scope: String,
    enable: bool,
) -> Result<(), IpcError> {
    if scope != "user" {
        return Err(IpcError::new(
            "requires_admin",
            "system launchd items need the privileged helper",
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        mole_ops::appmeta::set_login_agent_enabled(&label, &path, enable)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?
    .map_err(|cause| IpcError::new("io", cause))
}

// ---------------------------------------------------------------------------
// optimize
// ---------------------------------------------------------------------------

/// The optimize task catalog (explain-before-execute).
#[tauri::command]
pub fn list_optimize_tasks() -> Vec<mole_ops::optimize::OptimizeTask> {
    mole_ops::optimize::tasks(&crate::home())
}

/// Run selected optimize tasks; admin tasks refuse with requires_admin and
/// guarded tasks re-check live process state at run time.
#[tauri::command]
pub async fn run_optimize(
    task_ids: Vec<String>,
) -> Result<Vec<mole_ops::optimize::TaskResult>, IpcError> {
    use mole_core::providers::PrivilegedRunner;
    let _busy = crate::tray_anim::busy();
    let helper_available = AdminRunner.available();
    let home = crate::home();
    tauri::async_runtime::spawn_blocking(move || {
        // Rebuild the catalog at execution time so file-dependent tasks bind
        // to current disk state, and probe processes fresh (tri-state guard).
        let catalog = mole_ops::optimize::tasks(&home);
        let probes = SystemProbes::new();
        let executor = mole_ops::optimize::SystemTaskExecutor::new(&home);
        mole_ops::optimize::run_tasks(&catalog, &task_ids, &executor, helper_available, &probes)
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))
}

// ---------------------------------------------------------------------------
// settings: FDA, autostart, tray behavior
// ---------------------------------------------------------------------------

/// Bundle identifier shared with `tauri.conf.json` and the user LaunchAgent.
const APP_IDENTIFIER: &str = "com.zhichi.tidy";

/// Exact placeholder identifier used by pre-Tidy development builds.
const LEGACY_APP_IDENTIFIER: &str = "com.cleaner.desktop";

/// Full Disk Access probe: FDA-protected user dirs are unreadable without the
/// grant, so a successful read_dir is the evidence.
#[tauri::command]
pub fn fda_status() -> bool {
    ["Library/Safari", "Library/Mail"]
        .iter()
        .any(|sub| std::fs::read_dir(std::path::Path::new(&crate::home()).join(sub)).is_ok())
}

/// Open the Full Disk Access pane in System Settings.
#[tauri::command]
pub fn open_fda_settings() -> Result<(), IpcError> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn()
        .map(|_| ())
        .map_err(|e| IpcError::new("io", e.to_string()))
}

/// Height of the floating "drag Tidy into the list" helper (logical px);
/// its width follows the System Settings window it docks under.
const FDA_HELPER_HEIGHT: f64 = 150.0;
const FDA_HELPER_MIN_WIDTH: f64 = 460.0;

/// PIDs of running System Settings / System Preferences processes. The
/// executable name is stable across locales (the window owner NAME is not:
/// it shows as "系统设置" on a Chinese system).
fn system_settings_pids() -> Vec<i64> {
    let mut pids = Vec::new();
    for name in ["System Settings", "System Preferences"] {
        if let Ok(out) = std::process::Command::new("pgrep")
            .args(["-x", name])
            .output()
        {
            pids.extend(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|l| l.trim().parse::<i64>().ok()),
            );
        }
    }
    pids
}

/// Screen rectangle (logical px, top-left origin) of the frontmost
/// System Settings window, via CGWindowList matched by owner PID. Bounds
/// and owner PIDs need no TCC permission (only window titles would). None
/// when not open yet.
fn system_settings_frame() -> Option<(f64, f64, f64, f64)> {
    let pids = system_settings_pids();
    if pids.is_empty() {
        return None;
    }
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::display::{
        kCGNullWindowID, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
        CGWindowListCopyWindowInfo,
    };
    // SAFETY: plain CG query; the returned array is owned and wrapped
    // immediately so it is released with the wrapper.
    let list = unsafe {
        let raw = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        );
        if raw.is_null() {
            return None;
        }
        CFArray::<CFDictionary<CFString, CFType>>::wrap_under_create_rule(raw)
    };
    let pid_key = CFString::new("kCGWindowOwnerPID");
    let layer_key = CFString::new("kCGWindowLayer");
    let bounds_key = CFString::new("kCGWindowBounds");
    for win in list.iter() {
        let pid = win
            .find(&pid_key)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64());
        if !pid.is_some_and(|p| pids.contains(&p)) {
            continue;
        }
        // Layer 0 = normal windows (skips menus, tooltips, the status item).
        let layer = win
            .find(&layer_key)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
            .unwrap_or(-1);
        if layer != 0 {
            continue;
        }
        // Downcast to the untyped dictionary, then re-key it as string→any.
        let Some(bounds) = win
            .find(&bounds_key)
            .and_then(|v| v.downcast::<CFDictionary>())
            .map(|d| unsafe {
                // SAFETY: the bounds dictionary's keys are CFStrings and its
                // values CFNumbers; the retain keeps it alive with `d`.
                CFDictionary::<CFString, CFType>::wrap_under_get_rule(d.as_concrete_TypeRef())
            })
        else {
            continue;
        };
        let num = |k: &str| {
            bounds
                .find(CFString::new(k))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_f64())
        };
        let (Some(x), Some(y), Some(w), Some(h)) =
            (num("X"), num("Y"), num("Width"), num("Height"))
        else {
            continue;
        };
        // The list is front-to-back, so the first real-sized window is the
        // pane; tiny ones are sheets/popovers.
        if w >= 300.0 && h >= 200.0 {
            return Some((x, y, w, h));
        }
    }
    None
}

/// Show the floating drag helper docked right under the System Settings
/// window (same width, 8 px gap). System Settings may still be launching
/// when this runs, so its window is polled for up to ~3 s; if it never
/// shows up the helper falls back to the bottom-center of the main
/// window's screen. Idempotent: an existing helper is re-docked, not
/// duplicated.
#[tauri::command]
pub async fn fda_helper_show(app: tauri::AppHandle) -> Result<(), IpcError> {
    let frame = tauri::async_runtime::spawn_blocking(|| {
        for _ in 0..30 {
            if let Some(f) = system_settings_frame() {
                return Some(f);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    })
    .await
    .map_err(|e| IpcError::new("io", e.to_string()))?;

    let h = FDA_HELPER_HEIGHT;
    let (x, y, w) = match frame {
        Some((sx, sy, sw, sh)) => {
            let w = sw.max(FDA_HELPER_MIN_WIDTH);
            (sx + (sw - w) / 2.0, sy + sh + 8.0, w)
        }
        None => {
            let monitor = app
                .get_webview_window("main")
                .and_then(|m| m.current_monitor().ok().flatten())
                .or_else(|| app.primary_monitor().ok().flatten());
            let w = FDA_HELPER_MIN_WIDTH;
            match monitor {
                Some(m) => {
                    let scale = m.scale_factor();
                    let size = m.size().to_logical::<f64>(scale);
                    let pos = m.position().to_logical::<f64>(scale);
                    (
                        pos.x + (size.width - w) / 2.0,
                        pos.y + size.height - h - 24.0,
                        w,
                    )
                }
                None => (200.0, 600.0, w),
            }
        }
    };

    if let Some(win) = app.get_webview_window("fda-helper") {
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(
        &app,
        "fda-helper",
        tauri::WebviewUrl::App("index.html#/fda-helper".into()),
    )
    .title("Tidy")
    .inner_size(w, h)
    .position(x, y)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .build()
    .map(|_| ())
    .map_err(|e| IpcError::new("window", e.to_string()))
}

/// Close the drag helper (permission granted, or the user dismissed it).
#[tauri::command]
pub fn fda_helper_hide(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("fda-helper") {
        let _ = win.close();
    }
}

/// What the helper drags: the .app bundle (or the bare binary in dev) plus
/// an icon file for the drag image.
#[derive(Serialize)]
pub struct FdaDragSource {
    pub app_path: String,
    pub icon_path: String,
}

/// Resolve the running bundle and a drag icon on disk. The icon is the
/// bundle's icns when bundled; in dev the embedded PNG is written to a temp
/// file because webview assets are not files the OS can read.
#[tauri::command]
pub fn fda_drag_source() -> Result<FdaDragSource, IpcError> {
    let exe = std::env::current_exe().map_err(|e| IpcError::new("io", e.to_string()))?;
    let bundle = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|x| x == "app"))
        .map(std::path::Path::to_path_buf);
    let app_path = bundle.clone().unwrap_or(exe);
    let icns = bundle.map(|b| b.join("Contents/Resources/icon.icns"));
    let icon_path = match icns {
        Some(p) if p.is_file() => p,
        _ => {
            let p = std::env::temp_dir().join("tidy-drag-icon.png");
            if !p.is_file() {
                std::fs::write(&p, include_bytes!("../icons/128x128@2x.png"))
                    .map_err(|e| IpcError::new("io", e.to_string()))?;
            }
            p
        }
    };
    Ok(FdaDragSource {
        app_path: app_path.to_string_lossy().into_owned(),
        icon_path: icon_path.to_string_lossy().into_owned(),
    })
}

/// LaunchAgent path for an app identifier under the invoking user's home.
fn autostart_plist_for(identifier: &str) -> std::path::PathBuf {
    std::path::Path::new(&crate::home())
        .join("Library/LaunchAgents")
        .join(format!("{identifier}.plist"))
}

/// Current LaunchAgent path for the app's login-item autostart.
fn autostart_plist() -> std::path::PathBuf {
    autostart_plist_for(APP_IDENTIFIER)
}

/// Remove one exact app-owned LaunchAgent. Missing files are already disabled.
fn remove_autostart_plist(plist: &std::path::Path) -> Result<(), IpcError> {
    // SAFE: callers pass only the two exact Tidy-owned LaunchAgent paths; the
    // deletion sink would add Trash and audit noise to an app-owned plist.
    #[allow(clippy::disallowed_methods)]
    match std::fs::remove_file(plist) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(IpcError::from(e)),
    }
}

#[cfg(test)]
mod autostart_tests {
    use super::*;

    #[test]
    fn launch_agent_identifier_matches_tauri_bundle() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], APP_IDENTIFIER);
        assert!(autostart_plist_for(APP_IDENTIFIER).ends_with("com.zhichi.tidy.plist"));
        assert!(autostart_plist_for(LEGACY_APP_IDENTIFIER).ends_with("com.cleaner.desktop.plist"));
    }
}

/// Whether the autostart LaunchAgent is installed.
#[tauri::command]
pub fn autostart_get() -> bool {
    autostart_plist().exists() || autostart_plist_for(LEGACY_APP_IDENTIFIER).exists()
}

/// Install/remove the user-scope autostart LaunchAgent pointing at the
/// CURRENT executable (dev builds register the dev binary — visible, honest).
#[tauri::command]
pub fn autostart_set(enable: bool) -> Result<(), IpcError> {
    let plist = autostart_plist();
    if enable {
        let exe = std::env::current_exe().map_err(IpcError::from)?;
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{APP_IDENTIFIER}</string>
<key>ProgramArguments</key><array><string>{}</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#,
            exe.display()
        );
        if let Some(dir) = plist.parent() {
            std::fs::create_dir_all(dir).map_err(IpcError::from)?;
        }
        std::fs::write(&plist, content).map_err(IpcError::from)?;
        // Write the new generation before removing the exact placeholder so a
        // failed write cannot disable an existing login item.
        remove_autostart_plist(&autostart_plist_for(LEGACY_APP_IDENTIFIER))
    } else {
        // Attempt both exact generations so disabling the setting cannot leave
        // a legacy login item active after the public product rename.
        let current_result = remove_autostart_plist(&plist);
        let legacy_result = remove_autostart_plist(&autostart_plist_for(LEGACY_APP_IDENTIFIER));
        current_result.and(legacy_result)
    }
}

/// Show/hide the menu-bar tray icon.
#[tauri::command]
pub fn tray_set_visible(app: tauri::AppHandle, visible: bool) -> Result<(), IpcError> {
    if let Some(tray) = app.tray_by_id("mole-tray") {
        tray.set_visible(visible)
            .map_err(|e| IpcError::new("io", e.to_string()))?;
    }
    Ok(())
}

/// Toggle close-to-tray (closing the window hides it instead of quitting).
#[tauri::command]
pub fn set_keep_in_tray(enable: bool) {
    crate::KEEP_IN_TRAY.store(enable, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod fda_helper_tests {
    /// Manual: open System Settings, then
    /// cargo test -p tidy system_settings_frame_is_found -- --ignored --nocapture
    #[test]
    #[ignore]
    fn system_settings_frame_is_found() {
        eprintln!("frame = {:?}", super::system_settings_frame());
    }
}

// ---------------------------------------------------------------------------
// 自更新与遥测
// ---------------------------------------------------------------------------

/// 后端持有的应用设置：前端 localStorage 存不了的那几项。
#[derive(Serialize)]
pub struct AppSettings {
    /// 启动时自动检查 Tidy 自身的更新。
    pub update_autocheck: bool,
    /// 用户是否允许匿名使用统计。
    pub telemetry_enabled: bool,
    /// 这个构建是否编译进了上报地址（未配置时设置项整行隐藏）。
    pub telemetry_configured: bool,
    /// 是否还欠用户一次首次告知横幅。
    pub telemetry_notice_pending: bool,
}

/// 读取后端持有的设置。
#[tauri::command]
pub fn app_settings() -> AppSettings {
    let home = crate::home();
    let configured = mole_telemetry::is_configured();
    let enabled = mole_core::settings::bool_or(&home, mole_telemetry::KEY_ENABLED, true);
    AppSettings {
        update_autocheck: mole_core::settings::bool_or(&home, crate::update::KEY_AUTOCHECK, true),
        telemetry_enabled: enabled,
        telemetry_configured: configured,
        telemetry_notice_pending: configured
            && enabled
            && !mole_core::settings::bool_or(&home, mole_telemetry::KEY_NOTICE_SEEN, false),
    }
}

/// 开关：启动时自动检查更新。
#[tauri::command]
pub fn set_update_autocheck(on: bool) -> Result<(), IpcError> {
    mole_core::settings::set(
        &crate::home(),
        crate::update::KEY_AUTOCHECK,
        if on { "1" } else { "0" },
    )
    .map_err(IpcError::from)
}

/// 开关：匿名使用统计。关闭会同时清掉磁盘队列与安装标识。
#[tauri::command]
pub fn set_telemetry_enabled(on: bool) -> Result<(), IpcError> {
    mole_telemetry::set_enabled(&crate::home(), on).map_err(IpcError::from)
}

/// 记下首次告知横幅已经展示过。
#[tauri::command]
pub fn telemetry_notice_ack() -> Result<(), IpcError> {
    mole_telemetry::mark_notice_seen(&crate::home()).map_err(IpcError::from)
}

/// 上报一个事件。关闭遥测时是空操作。
#[tauri::command]
pub fn telemetry_track(event: crate::telemetry::TrackRequest) {
    let home = crate::home();
    mole_telemetry::track(event.into_event(&home));
}

/// 检查 Tidy 自身是否有新版本。
#[tauri::command]
pub async fn update_check(
    app: tauri::AppHandle,
    auto: bool,
) -> Result<Option<crate::update::UpdateInfo>, IpcError> {
    crate::update::check(&app, auto, &crate::home()).await
}

/// 下载、验签、安装新版本，然后重启。
#[tauri::command]
pub async fn update_install(
    app: tauri::AppHandle,
    on_progress: Channel<crate::update::DownloadProgress>,
) -> Result<(), IpcError> {
    crate::update::install(&app, on_progress).await
}
