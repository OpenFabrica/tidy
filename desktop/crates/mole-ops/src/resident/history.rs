// Rolling one-minute histories and per-metric sampling bookkeeping.
//
// The sampler is called about once a second; every call becomes one history
// slot. Metrics with a slower cadence (CPU 2 s, memory 3 s, ...) carry their
// latest still-fresh value into the in-between slots, so the sparkline is
// time-true. A slot is a gap (None) only when no fresh value exists: the
// sample failed, the metric was not demanded, or the sampler did not run
// (sleep, stalled thread) — in that case the missed seconds become gaps too.

use super::HISTORY_LEN;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Fixed-length ring of optional samples, oldest first.
#[derive(Debug, Clone)]
pub(crate) struct Series<T> {
    slots: VecDeque<Option<T>>,
}

impl<T: Clone> Default for Series<T> {
    /// A full-length history of gaps.
    fn default() -> Self {
        Self {
            slots: std::iter::repeat_n(None, HISTORY_LEN).collect(),
        }
    }
}

impl<T: Clone> Series<T> {
    /// Append one slot, dropping the oldest.
    pub(crate) fn push(&mut self, value: Option<T>) {
        if self.slots.len() >= HISTORY_LEN {
            self.slots.pop_front();
        }
        self.slots.push_back(value);
    }

    /// Overwrite the newest slot (a forced refresh within the same second).
    pub(crate) fn replace_last(&mut self, value: Option<T>) {
        if let Some(last) = self.slots.back_mut() {
            *last = value;
        }
    }

    /// Append `n` gap slots (missed seconds), capped at the history length.
    pub(crate) fn push_gaps(&mut self, n: usize) {
        for _ in 0..n.min(HISTORY_LEN) {
            self.push(None);
        }
    }

    /// Snapshot for the wire, oldest → newest.
    pub(crate) fn to_vec(&self) -> Vec<Option<T>> {
        self.slots.iter().cloned().collect()
    }
}

impl Series<f32> {
    /// Mean and max of the non-gap slots.
    pub(crate) fn avg_peak(&self) -> (Option<f32>, Option<f32>) {
        let values: Vec<f32> = self.slots.iter().flatten().copied().collect();
        if values.is_empty() {
            return (None, None);
        }
        let avg = values.iter().sum::<f32>() / values.len() as f32;
        let peak = values.iter().copied().fold(f32::MIN, f32::max);
        (Some(avg), Some(peak))
    }
}

/// How a sampler call maps onto history slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Advance {
    /// First call ever: push one slot.
    First,
    /// Called again within the same second (forced refresh): replace the
    /// newest slot instead of pushing.
    Replace,
    /// Normal cadence: push one slot after `gaps` missed seconds.
    Push { gaps: usize },
}

/// Decide the slot step from the time since the previous call. Up to 1.5 s
/// is normal 1 Hz jitter; longer pauses insert one gap per missed second.
pub(crate) fn advance(previous: Option<Instant>, now: Instant) -> Advance {
    let Some(previous) = previous else {
        return Advance::First;
    };
    let elapsed = now.saturating_duration_since(previous).as_secs_f64();
    if elapsed < 0.5 {
        Advance::Replace
    } else if elapsed < 1.5 {
        Advance::Push { gaps: 0 }
    } else {
        let gaps = (elapsed.round() as usize).saturating_sub(1);
        Advance::Push {
            gaps: gaps.min(HISTORY_LEN),
        }
    }
}

/// Apply an [`Advance`] to one series with this call's value.
pub(crate) fn step<T: Clone>(series: &mut Series<T>, adv: Advance, value: Option<T>) {
    match adv {
        Advance::First | Advance::Push { gaps: 0 } => series.push(value),
        Advance::Push { gaps } => {
            series.push_gaps(gaps);
            series.push(value);
        }
        Advance::Replace => series.replace_last(value),
    }
}

/// One metric's latest value plus its cadence and freshness limit.
#[derive(Debug, Clone)]
pub(crate) struct Tracked<T> {
    value: Option<T>,
    /// When `value` was measured.
    at: Option<Instant>,
    /// When we last attempted a sample (success or not).
    tried: Option<Instant>,
    interval: Duration,
    limit: Duration,
}

impl<T: Clone> Tracked<T> {
    /// A metric sampled every `interval`; values older than
    /// max(2 × interval + 1 s, 3 s) count as stale.
    pub(crate) fn every(interval: Duration) -> Self {
        let limit = (interval * 2 + Duration::from_secs(1)).max(Duration::from_secs(3));
        Self {
            value: None,
            at: None,
            tried: None,
            interval,
            limit,
        }
    }

    /// Whether a new sample is due. 300 ms of slack absorbs 1 Hz timer jitter
    /// so a 2 s metric does not slip to every 3 s.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.tried.is_none_or(|t| {
            now.saturating_duration_since(t) + Duration::from_millis(300) >= self.interval
        })
    }

    /// Record an attempt; a None result keeps the previous value (it ages).
    pub(crate) fn record(&mut self, now: Instant, value: Option<T>) {
        self.tried = Some(now);
        if let Some(v) = value {
            self.value = Some(v);
            self.at = Some(now);
        }
    }

    /// Force the next `due` check to succeed (demand changed, tab opened).
    pub(crate) fn expire(&mut self) {
        self.tried = None;
    }

    /// Value if measured within the freshness limit.
    pub(crate) fn fresh(&self, now: Instant) -> Option<T> {
        let at = self.at?;
        (now.saturating_duration_since(at) <= self.limit).then(|| self.value.clone())?
    }

    /// Latest value regardless of age (shown with a "cached" mark).
    pub(crate) fn last(&self) -> Option<T> {
        self.value.clone()
    }

    /// A value exists but is older than the freshness limit.
    pub(crate) fn is_stale(&self, now: Instant) -> bool {
        self.at
            .is_some_and(|at| now.saturating_duration_since(at) > self.limit)
    }

    /// Value to publish: fresh, or the cached one while still demanded.
    pub(crate) fn shown(&self, now: Instant, demanded: bool) -> Option<T> {
        if demanded {
            self.last()
        } else {
            self.fresh(now)
        }
    }
}

/// Median of three or more samples; None when empty.
pub(crate) fn median(mut values: Vec<u64>) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        values[mid - 1] / 2 + values[mid] / 2 + (values[mid - 1] % 2 + values[mid] % 2) / 2
    })
}

/// Bytes-per-second between two monotonic counters. A counter that went
/// backwards (driver reset, interface re-created) yields None rather than a
/// huge bogus rate; `elapsed` must be positive.
pub(crate) fn rate(previous: u64, current: u64, elapsed: Duration) -> Option<u64> {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return None;
    }
    let delta = current.checked_sub(previous)?;
    Some((delta as f64 / secs).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_is_fixed_length_and_drops_oldest() {
        let mut s = Series::<f32>::default();
        assert_eq!(s.to_vec().len(), HISTORY_LEN);
        for i in 0..(HISTORY_LEN + 5) {
            s.push(Some(i as f32));
        }
        let v = s.to_vec();
        assert_eq!(v.len(), HISTORY_LEN);
        assert_eq!(v[0], Some(5.0));
        assert_eq!(v[HISTORY_LEN - 1], Some((HISTORY_LEN + 4) as f32));
    }

    #[test]
    fn avg_peak_ignore_gaps() {
        let mut s = Series::<f32>::default();
        assert_eq!(s.avg_peak(), (None, None));
        s.push(Some(10.0));
        s.push(None);
        s.push(Some(30.0));
        assert_eq!(s.avg_peak(), (Some(20.0), Some(30.0)));
    }

    #[test]
    fn advance_maps_elapsed_time_to_slots() {
        let t0 = Instant::now();
        assert_eq!(advance(None, t0), Advance::First);
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(advance(Some(t0), at(200)), Advance::Replace);
        assert_eq!(advance(Some(t0), at(1000)), Advance::Push { gaps: 0 });
        assert_eq!(advance(Some(t0), at(1450)), Advance::Push { gaps: 0 });
        assert_eq!(advance(Some(t0), at(3000)), Advance::Push { gaps: 2 });
        assert_eq!(
            advance(Some(t0), at(3_600_000)),
            Advance::Push { gaps: HISTORY_LEN }
        );
    }

    #[test]
    fn step_inserts_gaps_for_missed_seconds() {
        let mut s = Series::<f32>::default();
        step(&mut s, Advance::First, Some(1.0));
        step(&mut s, Advance::Push { gaps: 2 }, Some(2.0));
        step(&mut s, Advance::Replace, Some(3.0));
        let v = s.to_vec();
        assert_eq!(&v[HISTORY_LEN - 4..], &[Some(1.0), None, None, Some(3.0)]);
    }

    #[test]
    fn tracked_cadence_freshness_and_staleness() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut m = Tracked::<f32>::every(Duration::from_secs(2));
        assert!(m.due(t0));
        m.record(t0, Some(5.0));
        assert!(!m.due(at(1000)));
        assert!(m.due(at(1800)), "jitter slack keeps a 2 s cadence");
        assert_eq!(m.fresh(at(4000)), Some(5.0));
        // A failed sample keeps the old value, which then ages out.
        m.record(at(2000), None);
        assert_eq!(m.fresh(at(6000)), None);
        assert!(m.is_stale(at(6000)));
        assert_eq!(m.shown(at(6000), true), Some(5.0));
        assert_eq!(m.shown(at(6000), false), None);
        m.expire();
        assert!(m.due(at(2100)));
    }

    #[test]
    fn rate_handles_reset_and_zero_elapsed() {
        let s = Duration::from_secs(2);
        assert_eq!(rate(1000, 3000, s), Some(1000));
        assert_eq!(rate(3000, 1000, s), None, "counter went backwards");
        assert_eq!(rate(0, 10, Duration::ZERO), None);
        assert_eq!(rate(u64::MAX - 1, u64::MAX, s), Some(1));
    }

    #[test]
    fn median_of_odd_and_even_sets() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5, 1, 3]), Some(3));
        assert_eq!(median(vec![4, 1, 3, 2]), Some(2));
        assert_eq!(median(vec![u64::MAX, u64::MAX]), Some(u64::MAX));
    }
}
