// Automatic memory release. A sleeping thread polls every 5 s whether a
// release is due under the user's ReleasePrefs (`memrelease::auto_release_due`);
// memory is sampled only when due, releases below the threshold are skipped
// (`memrelease::should_release`), and missed intervals never accumulate:
// the monotonic clock does not advance during sleep, and each due check
// restarts the interval whether it released or skipped.

use mole_ops::resident::memrelease;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// Poll period of the scheduler thread.
const POLL: Duration = Duration::from_secs(5);

/// Spawn the scheduler thread.
pub fn start(_app: &AppHandle) {
    std::thread::Builder::new()
        .name("resident-memrelease".into())
        .spawn(|| {
            mole_ops::scanutil::set_scan_thread_qos();
            let home = crate::home();
            let mut schedule = Schedule::new(Instant::now());
            loop {
                // Re-read prefs every poll so a change in the panel wins.
                let prefs = memrelease::load_prefs(&home);
                if schedule.due(&prefs, Instant::now()) {
                    let used = used_percent();
                    // Re-read at execution time: disabling right before the
                    // slot must win over the copy read above.
                    let prefs = memrelease::load_prefs(&home);
                    if prefs.automatic && memrelease::should_release(&prefs, used) {
                        let _ = memrelease::release_now();
                    }
                }
                std::thread::sleep(POLL);
            }
        })
        .expect("spawn memory release scheduler");
}

/// Current used-memory percentage, sampled on demand.
fn used_percent() -> Option<f32> {
    // Without a reading `should_release` skips thresholded releases.
    memrelease::used_memory_percent()
}

/// Interval bookkeeping around `memrelease::auto_release_due`.
pub struct Schedule {
    /// Start of the current interval (launch, enabling, or last due check).
    last_run: Instant,
    /// Automatic release was on at the previous poll.
    was_on: bool,
}

impl Schedule {
    /// Start counting the first interval at `now` (no release at launch).
    pub fn new(now: Instant) -> Self {
        Self {
            last_run: now,
            was_on: false,
        }
    }

    /// True when a due check should run now; restarts the interval if so.
    pub fn due(&mut self, prefs: &memrelease::ReleasePrefs, now: Instant) -> bool {
        // Turning automatic release on starts a fresh interval instead of
        // firing at once for time that passed while it was off.
        if prefs.automatic && !self.was_on {
            self.last_run = now;
        }
        self.was_on = prefs.automatic;
        if memrelease::auto_release_due(prefs, Some(self.last_run), now) {
            self.last_run = now;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memrelease::ReleasePrefs;

    /// Prefs with automatic release every `minutes`.
    fn every(minutes: u32) -> ReleasePrefs {
        ReleasePrefs {
            automatic: true,
            interval_minutes: minutes,
            threshold_percent: Some(80),
        }
    }

    #[test]
    fn fires_once_per_interval_and_not_at_enable() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0);
        let prefs = every(3);
        let mut fired = 0;
        let mut t = t0;
        while t < t0 + Duration::from_secs(9 * 60 + 1) {
            if s.due(&prefs, t) {
                fired += 1;
            }
            t += POLL;
        }
        assert_eq!(fired, 3);
    }

    #[test]
    fn re_enabling_restarts_the_interval() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0);
        let off = ReleasePrefs {
            automatic: false,
            ..every(3)
        };
        assert!(!s.due(&off, t0 + Duration::from_secs(3600)));
        assert!(!s.due(&every(3), t0 + Duration::from_secs(3605)));
        assert!(s.due(&every(3), t0 + Duration::from_secs(3605 + 180)));
    }
}
