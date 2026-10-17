// Memory release: purge volatile (purgeable) memory now, plus the automatic
// release schedule. The purge is the public Mach call
// mach_vm_purgable_control(mach_task_self(), 0, VM_PURGABLE_PURGE_ALL, ...):
// the kernel discards only objects their owners explicitly marked volatile
// (caches that are designed to be thrown away). It needs no admin rights,
// touches no other task, and never allocates memory to manufacture pressure.

use super::history::median;
use super::memory;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Outcome of one release attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    Completed,
    Busy,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseReport {
    pub status: ReleaseStatus,
    /// Observed drop in app-held memory, i.e. used + purgeable (median of
    /// samples before/after); None when sampling failed. Zero is a valid
    /// "no change".
    pub observed_reduction_bytes: Option<u64>,
    /// Used-memory percentage drop, rounded, when observed.
    pub reduction_percent: Option<u8>,
}

/// Automatic release schedule, persisted at ~/.config/mole/memory-release.json.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleasePrefs {
    pub automatic: bool,
    /// One of 3, 5, 15, 30, 60, 120.
    pub interval_minutes: u32,
    /// None = any usage; else 70 / 80 / 90.
    pub threshold_percent: Option<u8>,
}

impl Default for ReleasePrefs {
    /// Off, every 15 minutes, at 80 % usage.
    fn default() -> Self {
        Self {
            automatic: false,
            interval_minutes: 15,
            threshold_percent: Some(80),
        }
    }
}

/// Allowed check intervals, minutes.
pub const INTERVALS: [u32; 6] = [3, 5, 15, 30, 60, 120];
/// Allowed usage thresholds, percent (None = any usage).
pub const THRESHOLDS: [u8; 3] = [70, 80, 90];

impl ReleasePrefs {
    /// Whether every field holds one of the offered choices.
    pub fn is_valid(&self) -> bool {
        INTERVALS.contains(&self.interval_minutes)
            && self
                .threshold_percent
                .is_none_or(|t| THRESHOLDS.contains(&t))
    }
}

// Public SDK ABI (mach/mach_vm.h, mach/vm_purgable.h); libc does not bind it.
// VM_PURGABLE_PURGE_ALL = 2 ("purge all volatile objects now").
const VM_PURGABLE_PURGE_ALL: libc::c_int = 2;
extern "C" {
    // Same signature smc.rs uses; libc's binding is deprecated for mach2.
    fn mach_task_self() -> u32;
    fn mach_vm_purgable_control(
        target: libc::mach_port_t,
        address: u64,
        control: libc::c_int,
        state: *mut libc::c_int,
    ) -> libc::kern_return_t;
}

/// Map the Mach result to a status.
fn status_of(code: libc::kern_return_t) -> ReleaseStatus {
    match code {
        libc::KERN_SUCCESS => ReleaseStatus::Completed,
        libc::KERN_NOT_SUPPORTED | libc::KERN_INVALID_ARGUMENT => ReleaseStatus::Unsupported,
        _ => ReleaseStatus::Failed,
    }
}

/// The purge itself. PURGE_ALL ignores the address; our own task port only.
fn purge() -> ReleaseStatus {
    let mut state: libc::c_int = 0;
    let code =
        unsafe { mach_vm_purgable_control(mach_task_self(), 0, VM_PURGABLE_PURGE_ALL, &mut state) };
    status_of(code)
}

/// Median of app-held memory (Activity Monitor "Memory Used" plus volatile
/// purgeable pages) over `n` samples `gap` apart. The purgeable share is
/// included because that is exactly what a purge returns to the system —
/// Activity Monitor files it under cached, so "used" alone would always read
/// "no change". None if any read fails (a partial median would make the
/// reported drop untrustworthy).
fn sample_used(n: usize, gap: Duration) -> Option<u64> {
    let mut values = Vec::with_capacity(n);
    for i in 0..n {
        if i > 0 {
            std::thread::sleep(gap);
        }
        let m = memory::read()?;
        values.push(m.used.saturating_add(m.purgeable));
    }
    median(values)
}

/// Reduction in bytes and in rounded percentage points of `total`.
pub(crate) fn reduction(
    before: Option<u64>,
    after: Option<u64>,
    total: u64,
) -> (Option<u64>, Option<u8>) {
    match (before, after) {
        (Some(b), Some(a)) => {
            let drop = b.saturating_sub(a);
            let pct = (total > 0).then(|| {
                ((drop as f64 / total as f64) * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8
            });
            (Some(drop), pct)
        }
        _ => (None, None),
    }
}

/// Purge volatile (purgeable) memory now. Blocking (~1 s with sampling);
/// concurrent calls return Busy.
pub fn release_now() -> ReleaseReport {
    static RUNNING: Mutex<()> = Mutex::new(());
    let Ok(_guard) = RUNNING.try_lock() else {
        return ReleaseReport {
            status: ReleaseStatus::Busy,
            observed_reduction_bytes: None,
            reduction_percent: None,
        };
    };
    let total = memory::read().map_or(0, |m| m.total);
    let before = sample_used(3, Duration::from_millis(100));
    let status = purge();
    if status != ReleaseStatus::Completed {
        return ReleaseReport {
            status,
            observed_reduction_bytes: None,
            reduction_percent: None,
        };
    }
    // Let the VM settle before measuring the effect.
    std::thread::sleep(Duration::from_millis(500));
    let after = sample_used(3, Duration::from_millis(500));
    let (observed_reduction_bytes, reduction_percent) = reduction(before, after, total);
    ReleaseReport {
        status,
        observed_reduction_bytes,
        reduction_percent,
    }
}

/// ~/.config/mole/memory-release.json
pub fn prefs_file(home: &str) -> PathBuf {
    PathBuf::from(home).join(".config/mole/memory-release.json")
}

/// Load prefs (defaults on missing/malformed/out-of-range file).
pub fn load_prefs(home: &str) -> ReleasePrefs {
    std::fs::read_to_string(prefs_file(home))
        .ok()
        .and_then(|t| serde_json::from_str::<ReleasePrefs>(&t).ok())
        .filter(ReleasePrefs::is_valid)
        .unwrap_or_default()
}

/// Validate and persist prefs; Err("invalid") for out-of-range values.
pub fn save_prefs(home: &str, prefs: &ReleasePrefs) -> Result<(), String> {
    if !prefs.is_valid() {
        return Err("invalid".into());
    }
    let path = prefs_file(home);
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(prefs).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    };
    write().map_err(|e| format!("failed: {e}"))
}

/// Scheduler helper: current used-memory percent (Activity Monitor "Memory
/// Used" / installed RAM), 0–100. One cheap host_statistics64 read; needs no
/// Sampler. None on a Mach error.
pub fn used_memory_percent() -> Option<f32> {
    memory::read()?.usage_percent()
}

/// Scheduler helper: is an automatic release due? True when automatic
/// release is on and a full interval has passed since `last_run` (or there
/// was no run yet in this session). Missed intervals never accumulate: the
/// caller records `now` as the new `last_run` after acting (or skipping
/// below threshold), so a long sleep yields exactly one due check.
pub fn auto_release_due(prefs: &ReleasePrefs, last_run: Option<Instant>, now: Instant) -> bool {
    if !prefs.automatic {
        return false;
    }
    let interval = Duration::from_secs(u64::from(prefs.interval_minutes.max(1)) * 60);
    last_run.is_none_or(|t| now.saturating_duration_since(t) >= interval)
}

/// Scheduler helper: given current used-memory percent, should a due check
/// actually release? Always with no threshold; otherwise only at or above
/// it. Unknown usage with a threshold set → skip (never release blindly).
pub fn should_release(prefs: &ReleasePrefs, usage_percent: Option<f32>) -> bool {
    match prefs.threshold_percent {
        None => true,
        Some(t) => usage_percent.is_some_and(|u| u.round() >= f32::from(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_validation() {
        assert!(ReleasePrefs::default().is_valid());
        for i in INTERVALS {
            let p = ReleasePrefs {
                interval_minutes: i,
                ..ReleasePrefs::default()
            };
            assert!(p.is_valid());
        }
        let bad_interval = ReleasePrefs {
            interval_minutes: 7,
            ..ReleasePrefs::default()
        };
        assert!(!bad_interval.is_valid());
        let bad_threshold = ReleasePrefs {
            threshold_percent: Some(75),
            ..ReleasePrefs::default()
        };
        assert!(!bad_threshold.is_valid());
        let any = ReleasePrefs {
            threshold_percent: None,
            ..ReleasePrefs::default()
        };
        assert!(any.is_valid());
    }

    #[test]
    fn prefs_roundtrip_and_reject_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        assert_eq!(load_prefs(home), ReleasePrefs::default());
        let p = ReleasePrefs {
            automatic: true,
            interval_minutes: 60,
            threshold_percent: None,
        };
        save_prefs(home, &p).unwrap();
        assert_eq!(load_prefs(home), p);
        let bad = ReleasePrefs {
            interval_minutes: 0,
            ..p.clone()
        };
        assert_eq!(save_prefs(home, &bad), Err("invalid".into()));
        assert_eq!(load_prefs(home), p, "invalid save leaves the file alone");
        // A hand-edited out-of-range file loads as defaults.
        std::fs::write(
            prefs_file(home),
            r#"{"automatic":true,"interval_minutes":9,"threshold_percent":80}"#,
        )
        .unwrap();
        assert_eq!(load_prefs(home), ReleasePrefs::default());
    }

    #[test]
    fn due_logic_never_catches_up() {
        let t0 = Instant::now();
        let on = ReleasePrefs {
            automatic: true,
            interval_minutes: 15,
            threshold_percent: Some(80),
        };
        assert!(auto_release_due(&on, None, t0));
        assert!(!auto_release_due(
            &on,
            Some(t0),
            t0 + Duration::from_secs(14 * 60)
        ));
        assert!(auto_release_due(
            &on,
            Some(t0),
            t0 + Duration::from_secs(15 * 60)
        ));
        // After a 3-hour sleep: due once, and only once after recording now.
        let late = t0 + Duration::from_secs(3 * 3600);
        assert!(auto_release_due(&on, Some(t0), late));
        assert!(!auto_release_due(
            &on,
            Some(late),
            late + Duration::from_secs(5)
        ));
        let off = ReleasePrefs::default();
        assert!(!auto_release_due(&off, None, t0));
    }

    #[test]
    fn threshold_logic() {
        let p = ReleasePrefs {
            automatic: true,
            interval_minutes: 15,
            threshold_percent: Some(80),
        };
        assert!(should_release(&p, Some(80.0)));
        assert!(should_release(&p, Some(79.6)));
        assert!(!should_release(&p, Some(79.4)));
        assert!(!should_release(&p, None));
        let any = ReleasePrefs {
            threshold_percent: None,
            ..p
        };
        assert!(should_release(&any, None));
    }

    #[test]
    fn used_memory_percent_is_a_percentage() {
        let p = used_memory_percent().expect("memory counters");
        assert!((0.0..=100.0).contains(&p) && p > 0.0);
    }

    #[test]
    fn reduction_math() {
        assert_eq!(reduction(Some(900), Some(800), 1000), (Some(100), Some(10)));
        assert_eq!(reduction(Some(800), Some(900), 1000), (Some(0), Some(0)));
        assert_eq!(reduction(None, Some(900), 1000), (None, None));
        assert_eq!(reduction(Some(5), Some(1), 0), (Some(4), None));
    }

    #[test]
    fn mach_results_map_to_status() {
        assert_eq!(status_of(libc::KERN_SUCCESS), ReleaseStatus::Completed);
        assert_eq!(
            status_of(libc::KERN_NOT_SUPPORTED),
            ReleaseStatus::Unsupported
        );
        assert_eq!(status_of(libc::KERN_FAILURE), ReleaseStatus::Failed);
    }

    /// The purge must discard a volatile region we own and leave ordinary
    /// memory intact. PURGE_ALL is system-wide (it empties every app's
    /// volatile caches), so this runs only on request:
    /// `cargo test -p mole-ops purge_discards -- --ignored`.
    #[test]
    #[ignore = "purges system-wide volatile memory; run manually"]
    fn purge_discards_only_volatile_objects() {
        extern "C" {
            fn mach_vm_allocate(
                target: libc::mach_port_t,
                address: *mut u64,
                size: u64,
                flags: i32,
            ) -> libc::kern_return_t;
            fn mach_vm_deallocate(
                target: libc::mach_port_t,
                address: u64,
                size: u64,
            ) -> libc::kern_return_t;
        }
        const VM_FLAGS_ANYWHERE: i32 = 1;
        const VM_FLAGS_PURGABLE: i32 = 2;
        const VM_PURGABLE_SET_STATE: i32 = 0;
        const VM_PURGABLE_NONVOLATILE: i32 = 0;
        const VM_PURGABLE_VOLATILE: i32 = 1;
        const VM_PURGABLE_EMPTY: i32 = 2;
        let task = unsafe { mach_task_self() };
        let live = vec![0x5au8; 1 << 20];
        let size: u64 = 4 << 20;
        let mut addr = 0u64;
        assert_eq!(
            unsafe {
                mach_vm_allocate(task, &mut addr, size, VM_FLAGS_ANYWHERE | VM_FLAGS_PURGABLE)
            },
            libc::KERN_SUCCESS
        );
        unsafe { std::ptr::write_bytes(addr as *mut u8, 0x42, size as usize) };
        let mut state = VM_PURGABLE_VOLATILE;
        assert_eq!(
            unsafe { mach_vm_purgable_control(task, addr, VM_PURGABLE_SET_STATE, &mut state) },
            libc::KERN_SUCCESS
        );
        assert_eq!(purge(), ReleaseStatus::Completed);
        state = VM_PURGABLE_NONVOLATILE;
        assert_eq!(
            unsafe { mach_vm_purgable_control(task, addr, VM_PURGABLE_SET_STATE, &mut state) },
            libc::KERN_SUCCESS
        );
        assert_eq!(state, VM_PURGABLE_EMPTY, "volatile region was purged");
        assert!(live.iter().all(|b| *b == 0x5a), "ordinary memory untouched");
        unsafe { mach_vm_deallocate(task, addr, size) };
    }
}
