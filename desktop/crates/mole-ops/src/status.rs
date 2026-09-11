// Status: read-only system snapshot for the dashboard. Collectors are local
// and bounded: sysinfo for CPU/mem/disk/process, ioreg for GPU utilization
// and battery detail, `memory_pressure` for the pressure badge, and one
// cached `system_profiler` call for the GPU core count. Fan RPM and CPU/GPU
// die temperatures come from the read-only SMC client in `smc.rs`
// (unprivileged reads); when the SMC has no matching sensor they stay
// unreported rather than guessed.

use serde::Serialize;
use std::process::Command;
use std::sync::OnceLock;
use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// One disk row for the dashboard.
#[derive(Debug, Serialize)]
pub struct DiskStatus {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// One process row (top by CPU).
#[derive(Debug, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// Accumulated CPU time in ms — the energy-impact proxy (real per-process
    /// energy needs sudo powermetrics).
    pub cpu_time_ms: u64,
    /// Outermost .app bundle the executable lives in (icon source).
    pub app_path: Option<String>,
}

/// Outermost `.app` bundle prefix of an executable path.
fn app_bundle_of(exe: &str) -> Option<String> {
    let idx = exe.find(".app/")?;
    Some(exe[..idx + 4].to_string())
}

/// Battery detail from pmset + ioreg AppleSmartBattery.
#[derive(Debug, Default, Serialize)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
    pub cycle_count: Option<u32>,
    /// AppleRawMaxCapacity / DesignCapacity.
    pub health_percent: Option<u8>,
    /// |voltage × amperage|, current draw or charge rate.
    pub watts: Option<f32>,
}

/// GPU state from IOAccelerator statistics.
#[derive(Debug, Serialize)]
pub struct GpuStatus {
    pub utilization_percent: Option<u8>,
    pub core_count: Option<u32>,
}

/// Network totals plus a sampled rate.
#[derive(Debug, Serialize)]
pub struct NetworkStatus {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Sampled over the snapshot interval, scaled to per-second.
    pub rx_rate_bps: u64,
    pub tx_rate_bps: u64,
    /// Default-route interface name, when resolvable.
    pub interface: Option<String>,
}

/// Static hardware identity (chip, RAM, macOS version).
#[derive(Debug, Serialize)]
pub struct HardwareInfo {
    pub chip: String,
    pub memory_gb: u64,
    pub os_version: String,
}

/// Compact snapshot the dashboard renders.
#[derive(Debug, Serialize)]
pub struct StatusSnapshot {
    pub host: String,
    pub platform: String,
    pub hardware: HardwareInfo,
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f32,
    pub cpu_count: usize,
    /// Per-core usage for the bar chart.
    pub per_core_percent: Vec<f32>,
    pub load_avg_1m: f64,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub swap_used_bytes: u64,
    /// 100 − system-wide free percentage (from `memory_pressure`).
    pub memory_pressure_percent: Option<u8>,
    pub gpu: GpuStatus,
    pub disks: Vec<DiskStatus>,
    pub battery: Option<BatteryStatus>,
    pub network: NetworkStatus,
    /// Fan telemetry via unprivileged SMC reads; empty on fanless models.
    pub fans: Vec<crate::smc::FanStatus>,
    /// Hottest CPU/GPU die sensor via SMC; None when unavailable.
    pub temperatures: crate::smc::Temperatures,
    pub top_processes: Vec<ProcessInfo>,
}

/// First integer following `key` in ioreg-style text, e.g. `"Key" = 42`.
fn ioreg_value(text: &str, key: &str) -> Option<i64> {
    let idx = text.find(key)?;
    let rest = &text[idx + key.len()..];
    let rest = rest.trim_start_matches(['"', ' ', '=']);
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    num.parse().ok()
}

/// Run a command and return stdout as text (empty on any failure).
fn cmd_text(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// GPU utilization from IOAccelerator PerformanceStatistics.
fn gpu_utilization() -> Option<u8> {
    let text = cmd_text("ioreg", &["-r", "-d", "1", "-c", "IOAccelerator"]);
    ioreg_value(&text, "Device Utilization %").map(|v| v.clamp(0, 100) as u8)
}

/// GPU core count via system_profiler, cached for the process lifetime
/// (the call costs ~1s; the number cannot change while running).
fn gpu_core_count() -> Option<u32> {
    static CACHE: OnceLock<Option<u32>> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let text = cmd_text("system_profiler", &["SPDisplaysDataType"]);
        text.lines()
            .find(|l| l.contains("Total Number of Cores"))
            .and_then(|l| l.rsplit(':').next())
            .and_then(|v| v.trim().parse().ok())
    })
}

/// Memory pressure percent from the system tool (100 − free%).
fn memory_pressure_percent() -> Option<u8> {
    let text = cmd_text("memory_pressure", &["-Q"]);
    let line = text
        .lines()
        .find(|l| l.contains("free percentage"))?
        .rsplit(':')
        .next()?
        .trim()
        .trim_end_matches('%');
    let free: u8 = line.parse().ok()?;
    Some(100 - free.min(100))
}

/// Default-route interface name.
fn default_interface() -> Option<String> {
    let text = cmd_text("route", &["-n", "get", "default"]);
    text.lines()
        .find(|l| l.trim_start().starts_with("interface:"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.trim().to_string())
}

/// Battery detail: percent/charging from pmset, the rest from ioreg.
fn battery_status() -> Option<BatteryStatus> {
    let pmset = cmd_text("pmset", &["-g", "batt"]);
    let line = pmset.lines().find(|l| l.contains('%'))?;
    let pct_token = line.split_whitespace().find(|t| t.contains('%'))?;
    let percent: u8 = pct_token.trim_end_matches([';', '%']).parse().ok()?;
    let charging = line.contains("charging") && !line.contains("discharging");

    let ioreg = cmd_text("ioreg", &["-r", "-c", "AppleSmartBattery"]);
    let cycle_count = ioreg_value(&ioreg, "\"CycleCount\"").map(|v| v as u32);
    let health_percent = match (
        ioreg_value(&ioreg, "\"AppleRawMaxCapacity\""),
        ioreg_value(&ioreg, "\"DesignCapacity\""),
    ) {
        (Some(max), Some(design)) if design > 0 => {
            Some(((max as f64 / design as f64) * 100.0).round().min(100.0) as u8)
        }
        _ => None,
    };
    // Amperage is signed (negative = discharging); voltage in mV, current in mA.
    let watts = match (
        ioreg_value(&ioreg, "\"Voltage\""),
        ioreg_value(&ioreg, "\"Amperage\""),
    ) {
        (Some(v), Some(a)) => {
            // ioreg prints unsigned 64-bit for negative amperage; normalize.
            let a = if a > i64::from(i32::MAX) {
                a - (1i64 << 32)
            } else {
                a
            };
            Some(((v as f64 * a.unsigned_abs() as f64) / 1_000_000.0) as f32)
        }
        _ => None,
    };
    Some(BatteryStatus {
        percent,
        charging,
        cycle_count,
        health_percent,
        watts,
    })
}

/// Persistent sampling state: CPU and per-process usage are deltas between
/// refreshes, so keeping one System across snapshots lets each call do a
/// single full-process refresh (the previous call is the baseline) instead of
/// two refreshes bracketing a 300ms sleep — half the scan cost per tick and
/// no artificial latency. The elapsed time between calls doubles as the
/// network-rate window.
struct Sampler {
    sys: System,
    networks: Networks,
    /// When the previous refresh happened (the delta window start).
    last_refresh: std::time::Instant,
}

/// 只取 macOS 版本号（如 "15.6"）。snapshot() 要采样进程和 SMC，代价大得多，
/// 遥测这种只要一个字符串的场景走这里。
pub fn os_version() -> String {
    System::os_version().unwrap_or_default()
}

/// One snapshot from the dashboard's polling loop.
pub fn snapshot() -> StatusSnapshot {
    static SAMPLER: OnceLock<std::sync::Mutex<Sampler>> = OnceLock::new();
    let proc_kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_exe(UpdateKind::OnlyIfNotSet);

    let mut guard = SAMPLER
        .get_or_init(|| {
            let mut sys = System::new();
            // Baseline pass: per-process CPU is a delta, so the first real
            // snapshot needs this earlier refresh or everything reads 0.0.
            sys.refresh_cpu_usage();
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, proc_kind);
            std::sync::Mutex::new(Sampler {
                sys,
                networks: Networks::new_with_refreshed_list(),
                last_refresh: std::time::Instant::now(),
            })
        })
        .lock()
        .unwrap();
    let sampler = &mut *guard;

    // Give the very first snapshot (and any burst of back-to-back calls) a
    // wide enough delta window for meaningful CPU numbers.
    let min_window =
        sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(std::time::Duration::from_millis(200));
    let since_baseline = sampler.last_refresh.elapsed();
    if since_baseline < min_window {
        std::thread::sleep(min_window - since_baseline);
    }

    let sys = &mut sampler.sys;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, proc_kind);
    sampler.networks.refresh(true);
    let secs = sampler.last_refresh.elapsed().as_secs_f64().max(0.001);
    sampler.last_refresh = std::time::Instant::now();
    let networks = &sampler.networks;
    let (mut rx_rate, mut tx_rate) = (0f64, 0f64);
    for data in networks.values() {
        rx_rate += data.received() as f64 / secs;
        tx_rate += data.transmitted() as f64 / secs;
    }

    let disks = Disks::new_with_refreshed_list()
        .iter()
        .filter(|d| {
            let mp = d.mount_point().to_string_lossy().into_owned();
            mp == "/" || mp.starts_with("/Volumes/")
        })
        .map(|d| DiskStatus {
            name: d.name().to_string_lossy().into_owned(),
            mount_point: d.mount_point().to_string_lossy().into_owned(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        })
        .collect();

    let mut top: Vec<ProcessInfo> = sys
        .processes()
        .values()
        .map(|p| ProcessInfo {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cpu_percent: p.cpu_usage(),
            memory_bytes: p.memory(),
            cpu_time_ms: p.accumulated_cpu_time(),
            app_path: p.exe().and_then(|e| app_bundle_of(&e.to_string_lossy())),
        })
        .collect();
    top.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    top.truncate(50);

    StatusSnapshot {
        host: System::host_name().unwrap_or_default(),
        platform: format!(
            "{} {}",
            System::name().unwrap_or_default(),
            System::os_version().unwrap_or_default()
        ),
        hardware: HardwareInfo {
            chip: cmd_text("sysctl", &["-n", "machdep.cpu.brand_string"])
                .trim()
                .to_string(),
            memory_gb: sys.total_memory() / (1024 * 1024 * 1024),
            os_version: System::os_version().unwrap_or_default(),
        },
        uptime_seconds: System::uptime(),
        cpu_usage_percent: sys.global_cpu_usage(),
        cpu_count: sys.cpus().len(),
        per_core_percent: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
        load_avg_1m: System::load_average().one,
        memory_total_bytes: sys.total_memory(),
        memory_used_bytes: sys.used_memory(),
        swap_used_bytes: sys.used_swap(),
        memory_pressure_percent: memory_pressure_percent(),
        gpu: GpuStatus {
            utilization_percent: gpu_utilization(),
            core_count: gpu_core_count(),
        },
        disks,
        battery: battery_status(),
        network: NetworkStatus {
            rx_bytes: networks.values().map(|n| n.total_received()).sum(),
            tx_bytes: networks.values().map(|n| n.total_transmitted()).sum(),
            rx_rate_bps: rx_rate as u64,
            tx_rate_bps: tx_rate as u64,
            interface: default_interface(),
        },
        fans: crate::smc::fans(),
        temperatures: crate::smc::temperatures(),
        top_processes: top,
    }
}

/// Full detail for one process (the click-through modal).
#[derive(Debug, Serialize)]
pub struct ProcessDetail {
    pub pid: u32,
    pub name: String,
    pub app_path: Option<String>,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub user: Option<String>,
    /// Ancestors from launchd down: (pid, name).
    pub parent_chain: Vec<(u32, String)>,
    pub threads: Option<u32>,
    pub open_files: Option<u32>,
    /// "*:3306"-style listening sockets.
    pub listen_ports: Vec<String>,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
    pub children: u32,
    pub run_time_seconds: u64,
    pub cwd: Option<String>,
    pub exe: Option<String>,
    pub cmd: Vec<String>,
}

/// Collect detail for one pid. The lsof probes are bounded per call and only
/// run on explicit user click.
pub fn process_detail(pid: u32) -> Option<ProcessDetail> {
    use sysinfo::{Pid, Users};
    let mut sys = System::new();
    // Two passes so cpu_usage() is a real interval delta, not 0.0.
    sys.refresh_processes(ProcessesToUpdate::All, true);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let p = sys.process(Pid::from_u32(pid))?;

    let users = Users::new_with_refreshed_list();
    let user = p
        .user_id()
        .and_then(|uid| users.get_user_by_id(uid))
        .map(|u| u.name().to_string());

    // Ancestor chain up to launchd (pid 1).
    let mut parent_chain = Vec::new();
    let mut cursor = p.parent();
    while let Some(ppid) = cursor {
        let Some(pp) = sys.process(ppid) else { break };
        parent_chain.push((ppid.as_u32(), pp.name().to_string_lossy().into_owned()));
        if ppid.as_u32() <= 1 {
            break;
        }
        cursor = pp.parent();
    }
    parent_chain.reverse();

    let children = sys
        .processes()
        .values()
        .filter(|c| c.parent() == Some(Pid::from_u32(pid)))
        .count() as u32;

    // Thread count via ps -M (one line per thread + header).
    let threads = {
        let text = cmd_text("ps", &["-M", "-p", &pid.to_string()]);
        let lines = text.lines().count();
        if lines > 1 {
            Some((lines - 1) as u32)
        } else {
            None
        }
    };

    // Open-file count and listening sockets via bounded lsof calls.
    let open_files = {
        let text = cmd_text("lsof", &["-nP", "-p", &pid.to_string()]);
        let lines = text.lines().count();
        if lines > 1 {
            Some((lines - 1) as u32)
        } else {
            None
        }
    };
    let listen_ports: Vec<String> = {
        let text = cmd_text(
            "lsof",
            &["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid.to_string()],
        );
        let mut ports: Vec<String> = text
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().nth(8))
            .map(|addr| addr.to_string())
            .collect();
        ports.sort();
        ports.dedup();
        ports
    };

    let disk = p.disk_usage();
    Some(ProcessDetail {
        pid,
        name: p.name().to_string_lossy().into_owned(),
        app_path: p.exe().and_then(|e| app_bundle_of(&e.to_string_lossy())),
        cpu_percent: p.cpu_usage(),
        memory_bytes: p.memory(),
        user,
        parent_chain,
        threads,
        open_files,
        listen_ports,
        disk_read_bytes: disk.total_read_bytes,
        disk_written_bytes: disk.total_written_bytes,
        children,
        run_time_seconds: p.run_time(),
        cwd: p.cwd().map(|c| c.to_string_lossy().into_owned()),
        exe: p.exe().map(|e| e.to_string_lossy().into_owned()),
        cmd: p
            .cmd()
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sampler must persist across calls: a steady-state snapshot does one
    /// refresh with no built-in sleep. The pre-sampler implementation slept
    /// 300ms inside every call, so a generous 400ms bound pins the regression
    /// without flaking on slow runners.
    #[test]
    fn steady_state_snapshot_is_single_pass() {
        let first = snapshot();
        assert!(first.cpu_count > 0);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let t0 = std::time::Instant::now();
        let second = snapshot();
        assert!(second.cpu_count > 0);
        assert!(!second.top_processes.is_empty());
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(400),
            "steady-state snapshot slept like the old double-refresh path: {:?}",
            t0.elapsed()
        );
    }
}

/// Send SIGTERM (graceful) or SIGKILL (force) to a process. Refuses pid ≤ 1;
/// permission errors surface as named causes.
pub fn signal_process(pid: u32, force: bool) -> Result<(), String> {
    if pid <= 1 {
        return Err("refusing-system-pid".to_string());
    }
    let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
    let rc = unsafe { libc::kill(pid as i32, sig) };
    if rc == 0 {
        Ok(())
    } else {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => Err("permission-denied".to_string()),
            Some(libc::ESRCH) => Err("no-such-process".to_string()),
            _ => Err("kill-failed".to_string()),
        }
    }
}
