// Physical memory and pressure, computed the way Activity Monitor does:
// "Memory Used" = App Memory (anonymous pages minus purgeable) + Wired +
// Compressed. One host_statistics64 call plus two sysctls; no subprocess.

use super::cpu::host;
use super::ffi::{sysctl_int, sysctl_pod};
use super::Pressure;

/// One memory observation, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MemorySample {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub swap_used: u64,
    /// Volatile (purgeable) memory apps hold; Activity Monitor counts it as
    /// cached, not used, so a purge shows up here rather than in `used`.
    pub purgeable: u64,
}

impl MemorySample {
    /// Used share of installed RAM, 0–100.
    pub(crate) fn usage_percent(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.used as f64 / self.total as f64 * 100.0).min(100.0) as f32)
    }
}

/// Page counts relevant to the Activity Monitor formula.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Pages {
    pub free: u64,
    pub speculative: u64,
    pub internal: u64,
    pub purgeable: u64,
    pub wired: u64,
    pub compressed: u64,
}

/// Activity Monitor arithmetic: used = (internal − purgeable) + wired +
/// compressor; free excludes speculative pages (vm_stat does the same).
pub(crate) fn compute(total: u64, page_size: u64, p: Pages) -> (u64, u64) {
    let app = p.internal.saturating_sub(p.purgeable);
    let used = app
        .saturating_add(p.wired)
        .saturating_add(p.compressed)
        .saturating_mul(page_size)
        .min(total);
    let free = p
        .free
        .saturating_sub(p.speculative)
        .saturating_mul(page_size)
        .min(total);
    (used, free)
}

extern "C" {
    /// Kernel page size from mach/vm_page_size.h; the unit host_statistics64
    /// counts in, even for a translated process that sees 4 KiB user pages.
    static vm_kernel_page_size: libc::vm_size_t;
}

/// Kernel page size (16 KiB on Apple Silicon, 4 KiB on Intel).
fn page_size() -> u64 {
    let size = unsafe { vm_kernel_page_size } as u64;
    if size > 0 {
        size
    } else {
        4096
    }
}

/// Read the VM counters; None on a Mach error.
pub(crate) fn read() -> Option<MemorySample> {
    let total = u64::try_from(sysctl_int(c"hw.memsize")?).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let rc = unsafe {
        libc::host_statistics64(
            host(),
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    let s = unsafe { stats.assume_init() };
    let pages = Pages {
        free: u64::from(s.free_count),
        speculative: u64::from(s.speculative_count),
        internal: u64::from(s.internal_page_count),
        purgeable: u64::from(s.purgeable_count),
        wired: u64::from(s.wire_count),
        compressed: u64::from(s.compressor_page_count),
    };
    let page = page_size();
    let (used, free) = compute(total, page, pages);
    let swap_used = sysctl_pod::<libc::xsw_usage>(c"vm.swapusage")
        .map(|x| x.xsu_used)
        .unwrap_or(0);
    Some(MemorySample {
        total,
        used,
        free,
        swap_used,
        purgeable: pages.purgeable.saturating_mul(page).min(total),
    })
}

/// Decode kern.memorystatus_vm_pressure_level. XNU exports the dispatch
/// source flags (1 normal, 2 warning, 4 critical), not its internal 0–4 enum.
pub(crate) fn decode_pressure(level: i64) -> Pressure {
    match level {
        1 => Pressure::Normal,
        2 => Pressure::Warning,
        4 => Pressure::Critical,
        _ => Pressure::Unknown,
    }
}

/// Current memory pressure.
pub(crate) fn pressure() -> Pressure {
    sysctl_int(c"kern.memorystatus_vm_pressure_level").map_or(Pressure::Unknown, decode_pressure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_decodes_dispatch_flags_only() {
        assert_eq!(decode_pressure(1), Pressure::Normal);
        assert_eq!(decode_pressure(2), Pressure::Warning);
        assert_eq!(decode_pressure(4), Pressure::Critical);
        for bad in [0, 3, 5, -1] {
            assert_eq!(decode_pressure(bad), Pressure::Unknown);
        }
    }

    #[test]
    fn activity_monitor_formula() {
        let p = Pages {
            free: 100,
            speculative: 20,
            internal: 500,
            purgeable: 50,
            wired: 100,
            compressed: 30,
        };
        // used = (500-50)+100+30 = 580 pages; free = 80 pages.
        assert_eq!(compute(1_000_000, 1000, p), (580_000, 80_000));
        // Clamped to installed RAM on garbage counters.
        let huge = Pages {
            internal: u64::MAX,
            ..p
        };
        assert_eq!(compute(10, 1000, huge).0, 10);
    }

    #[test]
    fn live_read_is_consistent() {
        let m = read().expect("memory counters");
        assert!(m.total > 0 && m.used > 0 && m.used <= m.total);
        assert!(m.free <= m.total);
        assert!(m.purgeable <= m.total);
        let pct = m.usage_percent().unwrap();
        assert!((0.0..=100.0).contains(&pct));
    }
}
