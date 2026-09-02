// Typed invoke wrappers: one function per Tauri command, so views never call
// invoke() with a raw string. Progress streams use Tauri channels.

import { Channel, invoke as rawInvoke } from "@tauri-apps/api/core";
import { currentView, track } from "./telemetry";
import type {
  AppInfo,
  AppMeta,
  CachedUpdates,
  CleanPlanPayload,
  DirListing,
  EmbeddedLoginItem,
  ExecItem,
  ExecReport,
  OptimizeTask,
  PlanSummary,
  ProcessDetail,
  PurgePlanPayload,
  ScanEvent,
  StatusSnapshot,
  TaskResult,
  LoginItem,
  TouchIdStatus,
  UninstallPlanPayload,
  UpdateCatalog,
  UpdateResult,
  WhitelistPayload,
  DockerPlanPayload,
  OrphanPlanPayload,
  ToolPlanPayload,
  AppSettings,
  UpdateInfo,
  DownloadProgress,
} from "./types";

/**
 * invoke 包装：任何带稳定错误码的后端失败都记一条 error_occurred，然后原样
 * 抛出去。集中在这里是因为错误上报最容易漏——散在每个 view 的 catch 里，
 * 迟早会有一个分支忘记埋，而且很容易顺手把 message 也带上（message 里有
 * 绝对路径）。这里只取 code，Rust 侧还会再过一遍白名单。
 */
function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return rawInvoke<T>(cmd, args).catch((err: unknown) => {
    const code = (err as { code?: unknown } | null)?.code;
    if (typeof code === "string") {
      track({ kind: "error_occurred", code, view: currentView() });
    }
    throw err;
  });
}

/** Build a progress channel from a plain callback. */
function channel<T>(cb: (event: T) => void): Channel<T> {
  const ch = new Channel<T>();
  ch.onmessage = cb;
  return ch;
}

/** Session-unique task id for cancellable scans. */
let taskCounter = 0;
export const newTaskId = () => `task-${++taskCounter}-${Date.now()}`;

export const appMeta = () => invoke<AppMeta>("app_meta");
export const statusSnapshot = () => invoke<StatusSnapshot>("status_snapshot");

export const whitelistGet = () => invoke<WhitelistPayload>("whitelist_get");
export const whitelistSet = (patterns: string[]) =>
  invoke<string[]>("whitelist_set", { patterns });
export const purgePathsGet = () => invoke<string[] | null>("purge_paths_get");
export const touchidStatus = () => invoke<TouchIdStatus>("touchid_status");

export const analyzeScan = (
  root: string,
  taskId: string,
  fresh: boolean,
  onProgress: (e: ScanEvent) => void,
) =>
  invoke<DirListing>("analyze_scan", {
    root,
    taskId,
    fresh,
    onProgress: channel(onProgress),
  });

export const planDeletePaths = (paths: string[], taskId: string) =>
  invoke<PlanSummary>("plan_delete_paths", { paths, taskId });

export const planClean = (taskId: string, onProgress: (e: ScanEvent) => void) =>
  invoke<CleanPlanPayload>("plan_clean", {
    taskId,
    onProgress: channel(onProgress),
  });

export const revealInFinder = (path: string) =>
  invoke<void>("reveal_in_finder", { path });

export const listApps = (taskId: string) =>
  invoke<AppInfo[]>("list_apps", { taskId });

export const planUninstall = (appPaths: string[], taskId: string) =>
  invoke<UninstallPlanPayload>("plan_uninstall", { appPaths, taskId });

export const planPurge = (taskId: string, onProgress: (e: ScanEvent) => void) =>
  invoke<PurgePlanPayload>("plan_purge", { taskId, onProgress: channel(onProgress) });

export const planDocker = (taskId: string) =>
  invoke<DockerPlanPayload>("plan_docker", { taskId });

/** Docker items are removed by the docker CLI, not the file sink. */
export const executeDocker = (
  planId: string,
  selection: string[],
  onProgress: (item: ExecItem) => void,
) =>
  invoke<ExecReport>("execute_docker", { planId, selection, onProgress: channel(onProgress) });

export const planInstaller = (taskId: string) =>
  invoke<PlanSummary>("plan_installer", { taskId });

/** Trash contents; the backend stores this plan permanent-only. */
export const planTrash = (taskId: string) => invoke<PlanSummary>("plan_trash", { taskId });

/** Leftovers of apps no longer installed (Trash-routed). */
export const planOrphans = (taskId: string) =>
  invoke<OrphanPlanPayload>("plan_orphans", { taskId });

/** Snapshots / simulators / Homebrew: removed by their own CLIs. */
export const planTools = (taskId: string) => invoke<ToolPlanPayload>("plan_tools", { taskId });

export const executeTools = (
  planId: string,
  selection: string[],
  onProgress: (item: ExecItem) => void,
) =>
  invoke<ExecReport>("execute_tools", { planId, selection, onProgress: channel(onProgress) });

export const executePlan = (
  planId: string,
  selection: string[],
  dryRun: boolean,
  onProgress: (item: ExecItem) => void,
  trashOverride: boolean | null = null,
) =>
  invoke<ExecReport>("execute_plan", {
    planId,
    selection,
    dryRun,
    trashOverride,
    onProgress: channel(onProgress),
  });

export const cancelTask = (taskId: string) =>
  invoke<boolean>("cancel_task", { taskId });

export const listOptimizeTasks = () => invoke<OptimizeTask[]>("list_optimize_tasks");
export const runOptimize = (taskIds: string[]) =>
  invoke<TaskResult[]>("run_optimize", { taskIds });

export const appIcon = (appPath: string) =>
  invoke<string | null>("app_icon", { appPath });

/** Drop the backend app-inventory and icon caches before a manual re-scan. */
export const refreshAppCache = () => invoke<void>("refresh_app_cache");

export const previewUninstall = (appPaths: string[], taskId: string) =>
  invoke<UninstallPlanPayload>("preview_uninstall", { appPaths, taskId });

/** Newest known catalog without scanning (memory or persisted last scan). */
export const cachedAppUpdates = () => invoke<CachedUpdates | null>("cached_app_updates");

export const listAppUpdates = (taskId: string, force = false) =>
  invoke<UpdateCatalog>("list_app_updates", { taskId, force });

export const runAppUpdates = (updateIds: string[], taskId: string) =>
  invoke<UpdateResult[]>("run_app_updates", { updateIds, taskId });

export const setAppUpdateIgnored = (updateId: string, ignored: boolean) =>
  invoke<void>("set_app_update_ignored", { updateId, ignored });

export const listLoginItems = () => invoke<LoginItem[]>("list_login_items");

export const listEmbeddedLoginItems = () =>
  invoke<EmbeddedLoginItem[]>("list_embedded_login_items");

export const setLoginItemEnabled = (
  label: string,
  path: string,
  scope: string,
  enable: boolean,
) => invoke<void>("set_login_item_enabled", { label, path, scope, enable });

export const processDetail = (pid: number) =>
  invoke<ProcessDetail | null>("process_detail", { pid });

export const signalProcess = (pid: number, force: boolean) =>
  invoke<void>("signal_process", { pid, force });

export const fdaStatus = () => invoke<boolean>("fda_status");
export const openFdaSettings = () => invoke<void>("open_fda_settings");
/** Floating "drag Tidy into the list" helper shown next to System Settings. */
export const fdaHelperShow = () => invoke<void>("fda_helper_show");
export const fdaHelperHide = () => invoke<void>("fda_helper_hide");
export const fdaDragSource = () =>
  invoke<{ app_path: string; icon_path: string }>("fda_drag_source");
export const autostartGet = () => invoke<boolean>("autostart_get");
export const autostartSet = (enable: boolean) => invoke<void>("autostart_set", { enable });
export const traySetVisible = (visible: boolean) => invoke<void>("tray_set_visible", { visible });
export const setKeepInTray = (enable: boolean) => invoke<void>("set_keep_in_tray", { enable });

// --- 后端设置、自更新 -------------------------------------------------------

export const appSettings = () => invoke<AppSettings>("app_settings");
export const setUpdateAutocheck = (on: boolean) =>
  invoke<void>("set_update_autocheck", { on });
export const setTelemetryEnabled = (on: boolean) =>
  invoke<void>("set_telemetry_enabled", { on });
export const telemetryNoticeAck = () => invoke<void>("telemetry_notice_ack");

/** `auto` 为真时受"启动自动检查"开关与 24 小时频率限制约束。 */
export const updateCheck = (auto: boolean) =>
  invoke<UpdateInfo | null>("update_check", { auto });

/** 下载、验签、安装，成功后应用会自行重启（这个 Promise 不会 resolve）。 */
export const updateInstall = (onProgress: (p: DownloadProgress) => void) =>
  invoke<void>("update_install", { onProgress: channel(onProgress) });
