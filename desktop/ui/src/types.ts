// Hand-maintained mirror of the Rust DTOs in src-tauri (dto.rs/commands.rs).
// Keep field names in lockstep with the serde output.

export interface AppMeta {
  app_version: string;
  protection_data_sha256: string;
  helper_available: boolean;
  home: string;
}

export interface DiskStatus {
  name: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
}

export interface ProcessInfo {
  pid: number;
  name: string;
  cpu_percent: number;
  memory_bytes: number;
  cpu_time_ms: number;
  app_path: string | null;
}

export interface ProcessDetail {
  pid: number;
  name: string;
  app_path: string | null;
  cpu_percent: number;
  memory_bytes: number;
  user: string | null;
  parent_chain: [number, string][];
  threads: number | null;
  open_files: number | null;
  listen_ports: string[];
  disk_read_bytes: number;
  disk_written_bytes: number;
  children: number;
  run_time_seconds: number;
  cwd: string | null;
  exe: string | null;
  cmd: string[];
}

export interface BatteryStatus {
  percent: number;
  charging: boolean;
  cycle_count: number | null;
  temperature_c: number | null;
  health_percent: number | null;
  watts: number | null;
}

export interface GpuStatus {
  utilization_percent: number | null;
  core_count: number | null;
}

export interface NetworkStatus {
  rx_bytes: number;
  tx_bytes: number;
  rx_rate_bps: number;
  tx_rate_bps: number;
  interface: string | null;
}

export interface FanStatus {
  actual_rpm: number;
  min_rpm: number | null;
  max_rpm: number | null;
  target_rpm: number | null;
}

export interface HardwareInfo {
  chip: string;
  memory_gb: number;
  os_version: string;
}

export interface StatusSnapshot {
  host: string;
  platform: string;
  hardware: HardwareInfo;
  uptime_seconds: number;
  cpu_usage_percent: number;
  cpu_count: number;
  per_core_percent: number[];
  load_avg_1m: number;
  memory_total_bytes: number;
  memory_used_bytes: number;
  swap_used_bytes: number;
  memory_pressure_percent: number | null;
  gpu: GpuStatus;
  disks: DiskStatus[];
  battery: BatteryStatus | null;
  network: NetworkStatus;
  fans: FanStatus[];
  top_processes: ProcessInfo[];
}

export interface WhitelistPayload {
  patterns: string[];
  warnings: string[];
}

// --- two-phase plan/execute ---

export interface PlanItem {
  id: string;
  path: string;
  size_kb: number | null;
  item_count: number;
  scope: "user" | "system";
}

export interface PlanSection {
  title: string;
  items: PlanItem[];
  total_kb: number;
}

export interface PlanSummary {
  plan_id: string;
  sections: PlanSection[];
  total_kb: number;
  count: number;
}

export interface ScanEvent {
  label: string;
  count: number;
}

export interface ExecItem {
  id: string;
  path: string;
  outcome: string;
  size_kb: number | null;
  error: string | null;
}

export interface ExecReport {
  items: ExecItem[];
  total_freed_kb: number;
  skipped: number;
  failed: number;
}

// --- feature payloads ---

export interface DirEntryInfo {
  name: string;
  path: string;
  size_kb: number;
  is_dir: boolean;
  item_count: number;
}

export interface DirListing {
  root: string;
  entries: DirEntryInfo[];
  total_kb: number;
  truncated: boolean;
}

export interface AppInfo {
  name: string;
  bundle_id: string;
  version: string;
  path: string;
  size_kb: number;
  protected: boolean;
  official_uninstaller: string | null;
  running: boolean;
}

export interface UninstallNote {
  bundle_id: string;
  note: string;
}

export interface UninstallPlanPayload {
  summary: PlanSummary;
  notes: UninstallNote[];
}

export interface ProjectReport {
  root: string;
  blockers: string[];
  /** Days since last commit / edit; null when unknown. */
  idle_days: number | null;
}

/** One Docker image row (mirror of mole_ops::docker::DockerImage). */
export interface DockerImage {
  id: string;
  repository: string;
  tag: string;
  size_kb: number;
  containers: number;
  age_days: number | null;
  dangling: boolean;
}

export interface DockerPlanPayload {
  summary: PlanSummary;
  images: DockerImage[];
}

export interface PurgePlanPayload {
  summary: PlanSummary;
  projects: ProjectReport[];
}

export interface OptimizeTask {
  id: string;
  title: string;
  description: string;
  /** Argv steps, all shown before running. */
  commands: string[][];
  requires_admin: boolean;
  /** Apps that must be closed first (tri-state guarded at run time). */
  guard_processes: string[];
}

export interface TaskResult {
  id: string;
  outcome: string;
  output: string;
}

export interface TouchIdStatus {
  enabled: boolean;
  source: string | null;
}

export interface AppUpdate {
  id: string;
  kind: "app" | "package";
  name: string;
  installed: string;
  latest: string;
  source: "homebrew" | "app_store" | "sparkle" | "electron" | "website";
  action: "terminal" | "open_app_store" | "open_app" | "open_website";
  release_notes: string | null;
  command_hint: string | null;
  ignored: boolean;
}

export interface UpToDateApp {
  name: string;
  version: string;
  source: string;
}

export interface UpdateCatalog {
  updates: AppUpdate[];
  up_to_date: UpToDateApp[];
  warnings: string[];
  checked_at: number;
}

/** Instant-paint payload; live=false means display-only stale ids. */
export interface CachedUpdates {
  catalog: UpdateCatalog;
  live: boolean;
}

export interface UpdateResult {
  id: string;
  outcome: "updated" | "external" | "still_pending" | "skipped" | "failed" | "cancelled";
  cause: string;
  message: string;
}

export interface LoginItem {
  label: string;
  path: string;
  program: string | null;
  program_exists: boolean;
  scope: "user" | "system";
  enabled: boolean;
}

export interface EmbeddedLoginItem {
  app_name: string;
  app_path: string;
  item_name: string;
  kind: "login" | "helper";
}

export interface BlockedCaches {
  owners: string[];
  count: number;
  total_kb: number;
}

/** One iOS backup folder (mirror of mole_ops::clean::BackupInfo). */
export interface BackupInfo {
  path: string;
  device: string;
  product: string;
  last_backup_days: number | null;
}

export interface CleanPlanPayload {
  summary: PlanSummary;
  blocked: BlockedCaches;
  backups: BackupInfo[];
}

/** One leftover of an app that is no longer installed. */
export interface OrphanInfo {
  path: string;
  bundle_id: string;
  idle_days: number | null;
}

export interface OrphanPlanPayload {
  summary: PlanSummary;
  orphans: OrphanInfo[];
}

/** What a tool item removes (mirror of mole_ops::tools::ToolTarget). */
export type ToolTarget =
  | { kind: "snapshot"; stamp: string }
  | { kind: "simulator"; udid: string }
  | { kind: "brew_cleanup" };

/** One command-backed reclaimable (mirror of mole_ops::tools::ToolItem). */
export interface ToolItem {
  id: string;
  label: string;
  detail: string | null;
  size_kb: number | null;
  requires_admin: boolean;
  target: ToolTarget;
}

export interface ToolPlanPayload {
  summary: PlanSummary;
  items: ToolItem[];
}

/** 后端持有的设置（前端 localStorage 存不下的那几项）。 */
export interface AppSettings {
  update_autocheck: boolean;
  telemetry_enabled: boolean;
  telemetry_configured: boolean;
  telemetry_notice_pending: boolean;
}

/** 一个可用的 Tidy 新版本。 */
export interface UpdateInfo {
  version: string;
  current_version: string;
  notes: string | null;
  /** 当前版本低于 feed 声明的最低版本：提示不可关闭。 */
  mandatory: boolean;
}

/** 更新包下载进度。 */
export interface DownloadProgress {
  downloaded: number;
  total: number | null;
}

/** 上报给后端的遥测事件。字段取值由 Rust 侧白名单收口。 */
export type TrackRequest =
  | { kind: "view_opened"; view: string }
  | { kind: "scan_completed"; scan: string; duration_ms: number }
  | { kind: "clean_executed"; mode: string; result: string }
  | { kind: "app_uninstalled" }
  | { kind: "optimize_run"; action: string }
  | { kind: "updates_run"; source: string }
  | { kind: "self_update"; from: string; to: string; result: string }
  | { kind: "error_occurred"; code: string; view: string };
