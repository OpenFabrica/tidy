// Whole-machine CPU usage from Mach host tick counters, plus static CPU
// identity (marketing name, core counts, Apple Silicon performance levels).
// One host_statistics call per sample: no per-core walk, no subprocess.

use super::ffi::{sysctl_int, sysctl_string};
use super::CoreLevel;
use std::ffi::CString;

/// Cumulative (user, system, idle, nice) ticks summed over all cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Ticks {
    pub user: u32,
    pub system: u32,
    pub idle: u32,
    pub nice: u32,
}

/// Read the aggregate tick counters; None on a Mach error.
pub(crate) fn read_ticks() -> Option<Ticks> {
    let mut info = libc::host_cpu_load_info {
        cpu_ticks: [0; libc::CPU_STATE_MAX as usize],
    };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    let rc = unsafe {
        libc::host_statistics(
            host(),
            libc::HOST_CPU_LOAD_INFO,
            (&mut info as *mut libc::host_cpu_load_info).cast(),
            &mut count,
        )
    };
    (rc == libc::KERN_SUCCESS).then(|| Ticks {
        user: info.cpu_ticks[libc::CPU_STATE_USER as usize],
        system: info.cpu_ticks[libc::CPU_STATE_SYSTEM as usize],
        idle: info.cpu_ticks[libc::CPU_STATE_IDLE as usize],
        nice: info.cpu_ticks[libc::CPU_STATE_NICE as usize],
    })
}

extern "C" {
    // Declared here because libc's binding is deprecated in favour of mach2.
    fn mach_host_self() -> libc::mach_port_t;
}

/// Cached host port (mach_host_self allocates a send right per call).
pub(crate) fn host() -> libc::mach_port_t {
    static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    *HOST.get_or_init(|| unsafe { mach_host_self() })
}

/// Busy percentage between two tick snapshots. The counters are 32-bit sums
/// over all cores and wrap after a few weeks of uptime on big chips, so the
/// deltas use wrapping arithmetic (correct across one wrap).
pub(crate) fn usage_between(prev: Ticks, cur: Ticks) -> Option<f32> {
    let user = cur.user.wrapping_sub(prev.user) as u64;
    let system = cur.system.wrapping_sub(prev.system) as u64;
    let idle = cur.idle.wrapping_sub(prev.idle) as u64;
    let nice = cur.nice.wrapping_sub(prev.nice) as u64;
    let total = user + system + idle + nice;
    if total == 0 {
        return None;
    }
    let busy = (user + system + nice) as f64 / total as f64 * 100.0;
    Some(busy.clamp(0.0, 100.0) as f32)
}

/// Static CPU identity, read once.
#[derive(Debug, Clone, Default)]
pub(crate) struct CpuInfo {
    pub model: String,
    pub core_count: usize,
    pub e_cores: Option<u32>,
    pub p_cores: Option<u32>,
    pub levels: Vec<CoreLevel>,
}

/// Model name, logical core count and performance levels. Apple Silicon
/// lists levels fastest first (perflevel0); the classic two-level layout is
/// P + E, but M5-class chips name them "Super"/"Performance", so the raw
/// names travel in `levels` for the UI to label honestly.
pub(crate) fn info() -> CpuInfo {
    let model = sysctl_string(c"machdep.cpu.brand_string").unwrap_or_default();
    let core_count = sysctl_int(c"hw.logicalcpu")
        .or_else(|| sysctl_int(c"hw.ncpu"))
        .unwrap_or(0)
        .max(0) as usize;
    let nlevels = sysctl_int(c"hw.nperflevels").unwrap_or(0).clamp(0, 8);
    let levels: Vec<CoreLevel> = (0..nlevels)
        .filter_map(|i| {
            let count = sysctl_int(&CString::new(format!("hw.perflevel{i}.logicalcpu")).ok()?)?;
            let name = sysctl_string(&CString::new(format!("hw.perflevel{i}.name")).ok()?)
                .unwrap_or_default();
            Some(CoreLevel {
                name,
                count: u32::try_from(count).ok()?,
            })
        })
        .collect();
    let (p_cores, e_cores) = match levels.as_slice() {
        [fast, slow] => (Some(fast.count), Some(slow.count)),
        _ => (None, None),
    };
    CpuInfo {
        model,
        core_count,
        e_cores,
        p_cores,
        levels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_from_tick_deltas() {
        let a = Ticks {
            user: 100,
            system: 50,
            idle: 800,
            nice: 50,
        };
        let b = Ticks {
            user: 200,
            system: 100,
            idle: 1100,
            nice: 100,
        };
        // busy 200 of 500 total.
        assert_eq!(usage_between(a, b), Some(40.0));
        assert_eq!(usage_between(a, a), None);
    }

    #[test]
    fn usage_survives_a_32_bit_counter_wrap() {
        let a = Ticks {
            user: u32::MAX - 9,
            system: 0,
            idle: u32::MAX - 29,
            nice: 0,
        };
        let b = Ticks {
            user: 10, // +20
            system: 0,
            idle: 50, // +80
            nice: 0,
        };
        assert_eq!(usage_between(a, b), Some(20.0));
    }

    #[test]
    fn live_ticks_and_info_are_plausible() {
        let t = read_ticks().expect("host_statistics");
        assert!(t.idle > 0);
        let info = info();
        assert!(info.core_count > 0);
        let level_sum: u32 = info.levels.iter().map(|l| l.count).sum();
        if !info.levels.is_empty() {
            assert_eq!(level_sum as usize, info.core_count);
        }
    }
}
