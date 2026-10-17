// Resident (menu-bar) monitoring: the data behind the status-bar item and the
// tray panel. One sampler owns every rolling history (one minute of samples)
// so the status item, the panel and the main window all read the same numbers.
// Everything here is read-only except `memrelease` (purges volatile memory via
// a public Mach call) and `apps::quit_app` (asks an app to quit normally).
//
// Wire format is snake_case JSON, like the rest of the Tidy IPC surface.

pub mod apps;
pub mod memrelease;
pub mod prefs;
pub mod sampler;

mod cpu;
mod disk;
mod diskhealth;
mod ffi;
mod gpu;
mod history;
mod memory;
mod net;
mod objc;
mod power;
mod procs;

use serde::{Deserialize, Serialize};

pub use net::{list_interfaces, InterfaceInfo};
pub use sampler::Sampler;

/// Samples kept per rolling history: one minute at the 1 s base tick.
pub const HISTORY_LEN: usize = 60;

/// One metric the status item can show, in `prefs::DEFAULT_ORDER` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricId {
    Cpu,
    CpuTemperature,
    Gpu,
    GpuTemperature,
    Memory,
    Disk,
    DiskTemperature,
    BatteryTemperature,
    ChargingPower,
    BatteryPower,
    SystemPower,
    FanSpeed,
    Network,
}

/// Panel tab currently on screen; drives which expensive samples run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelTab {
    #[default]
    Overview,
    Cpu,
    Gpu,
    Memory,
    Disk,
    Network,
}

/// What the consumers currently need. Cheap metrics are always sampled;
/// process lists, disk health and interface details only on demand.
#[derive(Debug, Clone, Default)]
pub struct Demand {
    /// Panel window visible.
    pub panel_open: bool,
    pub tab: PanelTab,
    /// Metrics enabled in the status item (temperatures/fans/power are
    /// sampled only when shown or when the panel needs them).
    pub metrics: Vec<MetricId>,
}

/// Memory pressure as macOS reports it (kern.memorystatus_vm_pressure_level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    Normal,
    Warning,
    Critical,
    #[default]
    Unknown,
}

/// One CPU performance level (hw.perflevelN), fastest first. Names are
/// Apple's own ("Performance"/"Efficiency"; M5-class: "Super"/"Performance").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreLevel {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuReading {
    /// Whole-machine usage 0–100; None until two tick samples exist.
    pub usage: Option<f32>,
    /// Marketing name, e.g. "Apple M5 Pro".
    pub model: String,
    pub core_count: usize,
    /// Efficiency / performance core counts (Apple Silicon), when known.
    pub e_cores: Option<u32>,
    pub p_cores: Option<u32>,
    /// Every performance level with its real name (additive; empty on Intel).
    #[serde(default)]
    pub core_levels: Vec<CoreLevel>,
    /// Mean and max of `history` (ignoring gaps).
    pub avg_1m: Option<f32>,
    pub peak_1m: Option<f32>,
    pub temp_c: Option<f32>,
    /// Oldest → newest, `HISTORY_LEN` slots, None = no sample (gap).
    pub history: Vec<Option<f32>>,
    pub temp_history: Vec<Option<f32>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuReading {
    pub usage: Option<f32>,
    pub name: String,
    pub core_count: Option<u32>,
    /// Renderer / Tiler engine utilisation (IOAccelerator statistics).
    pub renderer: Option<f32>,
    pub tiler: Option<f32>,
    /// In-use system memory attributed to the GPU, when reported.
    pub memory_used_bytes: Option<u64>,
    pub avg_1m: Option<f32>,
    pub peak_1m: Option<f32>,
    pub temp_c: Option<f32>,
    pub history: Vec<Option<f32>>,
    pub temp_history: Vec<Option<f32>>,
    /// Renderer / Tiler one-minute histories (additive), same slots as `history`.
    #[serde(default)]
    pub renderer_history: Vec<Option<f32>>,
    #[serde(default)]
    pub tiler_history: Vec<Option<f32>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryReading {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub swap_used_bytes: u64,
    pub usage_percent: Option<f32>,
    pub pressure: Pressure,
    pub history: Vec<Option<f32>>,
}

/// SMART-derived health of the boot disk. Every field is optional: what the
/// drive does not report stays None and the UI hides the row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskHealth {
    /// "verified" | "warning" | "failing" | "unknown".
    pub status: String,
    pub file_system: Option<String>,
    /// 100 − percentage used (NVMe), when reported.
    pub health_percent: Option<u8>,
    pub temp_c: Option<f32>,
    pub data_read_bytes: Option<u64>,
    pub data_written_bytes: Option<u64>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub available_spare_percent: Option<u8>,
    pub media_errors: Option<u64>,
    pub critical_warning: Option<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskReading {
    /// Boot volume display name ("Macintosh HD").
    pub volume_name: String,
    pub total_bytes: u64,
    /// System estimate, includes purgeable space (like Finder).
    pub available_bytes: u64,
    pub used_percent: Option<f32>,
    /// System-wide block-device I/O rates and totals since boot.
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
    pub read_total_bytes: Option<u64>,
    pub write_total_bytes: Option<u64>,
    /// (read_bps, write_bps) per slot; None = gap.
    pub io_history: Vec<Option<(u64, u64)>>,
    pub temp_c: Option<f32>,
    /// Only filled while the Disk tab is visible (cached ≥ 5 min).
    pub health: Option<DiskHealth>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetDetails {
    pub interface: String,
    pub connected: bool,
    pub mac: Option<String>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    /// Wi-Fi only: "Wi-Fi 6 (802.11ax)", "36 (5 GHz, 80 MHz)".
    pub wifi_standard: Option<String>,
    pub wifi_channel: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkReading {
    pub interface: Option<String>,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
    /// (rx_bps, tx_bps) per slot; None = gap.
    pub history: Vec<Option<(u64, u64)>>,
    /// Persisted cumulative totals (survive restarts; reset by the user).
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
    /// Since this app session started.
    pub session_rx_bytes: u64,
    pub session_tx_bytes: u64,
    /// Only filled while the Network tab is visible.
    pub details: Option<NetDetails>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PowerReading {
    pub battery_percent: Option<u8>,
    pub charging: bool,
    /// Power flowing into the machine from the adapter, watts.
    pub charging_watts: Option<f32>,
    /// Battery power, watts; positive = discharging, negative = charging.
    pub battery_watts: Option<f32>,
    pub system_watts: Option<f32>,
    pub battery_temp_c: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FanReading {
    pub id: u32,
    pub rpm: f32,
}

/// One application row (processes grouped by owning .app bundle; loose
/// processes stand alone).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppUsage {
    /// Stable grouping key (bundle path, else "pid:<n>").
    pub key: String,
    pub name: String,
    /// .app bundle path, for the icon and "Open location".
    pub bundle_path: Option<String>,
    /// Representative pid (the bundle's main process when known).
    pub pid: u32,
    pub process_count: u32,
    pub pids: Vec<u32>,
    pub executable: Option<String>,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// Some process memory could not be read.
    pub partial: bool,
    /// Has a regular (Dock) app that `quit_app` can ask to quit.
    pub quittable: bool,
}

/// Everything the menu bar and panel show, published at most once a second.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResidentReading {
    /// Unix ms of this sample.
    pub at_ms: u64,
    pub cpu: CpuReading,
    pub gpu: GpuReading,
    pub memory: MemoryReading,
    pub disk: DiskReading,
    pub network: NetworkReading,
    pub power: PowerReading,
    pub fans: Vec<FanReading>,
    /// Filled only while the CPU tab is visible (sorted by CPU, desc).
    pub apps_cpu: Vec<AppUsage>,
    /// Filled only while the Memory tab is visible (sorted by memory, desc).
    pub apps_memory: Vec<AppUsage>,
    /// True when some value is older than its freshness limit.
    pub stale: bool,
}
