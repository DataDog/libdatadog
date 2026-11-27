// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A rate limiter whose complete mutable state is one atomic deadline.
///
/// Granularity is supplied by the caller and is not part of the limiter state.
#[repr(transparent)]
#[derive(Default)]
pub struct LocalLimiter {
    deadline: AtomicU64,
}

const TIME_PER_SECOND: u64 = 1_000_000_000; // nanoseconds

/// When set to a non-zero value, `now()` returns this instead of the real clock.
/// This allows tests to control time deterministically, avoiding flakiness from
/// wall-clock timing on CI machines.
#[cfg(test)]
static MOCK_NOW: AtomicU64 = AtomicU64::new(0);

/// Monotonic nanoseconds from a system-wide clock, comparable across processes.
pub fn now() -> u64 {
    monotonic_now().unwrap_or(0)
}

fn monotonic_now() -> Option<u64> {
    #[cfg(test)]
    {
        let mock = MOCK_NOW.load(Ordering::Relaxed);
        if mock != 0 {
            return Some(mock);
        }
    }
    #[cfg(windows)]
    let now = {
        use windows_sys::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };

        static FREQUENCY: AtomicU64 = AtomicU64::new(0);

        let mut frequency = FREQUENCY.load(Ordering::Relaxed);
        if frequency == 0 {
            let mut measured_frequency = 0;
            // SAFETY: the output pointer refers to valid writable storage.
            if unsafe { QueryPerformanceFrequency(&mut measured_frequency) } == 0 {
                return None;
            }
            frequency = u64::try_from(measured_frequency)
                .ok()
                .filter(|frequency| *frequency != 0)?;
            FREQUENCY.store(frequency, Ordering::Relaxed);
        }

        let mut ticks = 0;
        // SAFETY: the output pointer refers to valid writable storage.
        if unsafe { QueryPerformanceCounter(&mut ticks) } == 0 {
            return None;
        }
        let nanos = u128::try_from(ticks)
            .ok()?
            .checked_mul(u128::from(TIME_PER_SECOND))?
            .checked_div(u128::from(frequency))?;
        u64::try_from(nanos).ok()?
    };
    #[cfg(not(windows))]
    let now = {
        let mut ts: libc::timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts points to valid writable storage.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
            return None;
        }
        u64::try_from(ts.tv_sec)
            .ok()?
            .checked_mul(TIME_PER_SECOND)?
            .checked_add(u64::try_from(ts.tv_nsec).ok()?)?
    };
    Some(now)
}

impl LocalLimiter {
    /// Clear all accumulated virtual-time debt.
    pub fn reset(&self) {
        self.deadline.store(0, Ordering::Relaxed);
    }

    /// Try to consume one of `limit` admissions per `granularity`.
    pub fn inc(&self, limit: u32, granularity: Duration) -> bool {
        let Some(granularity) = duration_nanos(granularity) else {
            return false;
        };
        monotonic_now().is_some_and(|now| admit(&self.deadline, granularity, limit, now))
    }

    /// Return whether [`Self::inc`] would currently succeed, without consuming anything.
    pub fn peek(&self, limit: u32, granularity: Duration) -> bool {
        let Some(granularity) = duration_nanos(granularity) else {
            return false;
        };
        monotonic_now().is_some_and(|now| has_room(&self.deadline, granularity, limit, now))
    }

    /// Consume one admission unconditionally, saturating at full capacity.
    ///
    /// Unlike [`Self::inc`] this never fails: once the limiter is exhausted for the next
    /// `granularity`, further debits are no-ops.
    pub fn debit(&self, limit: u32, granularity: Duration) {
        if let (Some(granularity), Some(now)) = (duration_nanos(granularity), monotonic_now()) {
            debit_saturating(&self.deadline, granularity, limit, now);
        }
    }

    /// Whether a hit with sampling `key` may become a candidate, before anything else is
    /// evaluated for it. Consumes nothing; a key within the threshold finding no budget left
    /// counts as a refusal for the threshold.
    pub fn precheck(
        &self,
        threshold: &SamplingThreshold,
        key: u64,
        limit: u32,
        granularity: Duration,
    ) -> bool {
        let (Some(interval_ns), Some(now)) = (duration_nanos(granularity), monotonic_now()) else {
            return false;
        };
        precheck_at(&self.deadline, threshold, key, interval_ns, limit, now)
    }

    /// Try to admit one sampling candidate and feed the outcome into `threshold`.
    pub fn try_lead(
        &self,
        threshold: &SamplingThreshold,
        limit: u32,
        granularity: Duration,
    ) -> bool {
        let (Some(interval_ns), Some(now)) = (duration_nanos(granularity), monotonic_now()) else {
            return false;
        };
        try_lead_at(&self.deadline, threshold, interval_ns, limit, now)
    }

    /// Return the outstanding virtual-time debt as a fraction of `granularity`.
    pub fn rate(&self, granularity: Duration) -> f64 {
        let Some(granularity) = duration_nanos(granularity) else {
            return 1.0;
        };
        monotonic_now().map_or(1.0, |now| {
            debt(&self.deadline, granularity, now).clamp(0.0, 1.0)
        })
    }

    /// Return whether the limiter still has outstanding virtual-time debt.
    pub fn is_active(&self) -> bool {
        monotonic_now().is_none_or(|now| self.deadline.load(Ordering::Relaxed) > now)
    }
}

fn duration_nanos(duration: Duration) -> Option<u64> {
    let nanos = u64::try_from(duration.as_nanos()).ok()?;
    (nanos > 0).then_some(nanos)
}

/// Atomically add one admission's virtual-time debt.
///
/// Each admission costs `ceil(interval_ns / limit)` nanoseconds. Idle time clears debt, and
/// the limiter accepts at most `limit` outstanding costs. The capacity uses the rounded cost,
/// rather than `interval_ns`, so rounding still permits the full initial burst.
fn admit(deadline: &AtomicU64, interval_ns: u64, limit: u32, now: u64) -> bool {
    let Some((cost_ns, max_deadline)) = admission_bounds(interval_ns, limit, now) else {
        return false;
    };
    // `try_update` is the new name, but is not available on the workspace MSRV.
    #[allow(deprecated)]
    let update = deadline.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous_deadline| {
        let next_deadline = previous_deadline.max(now).checked_add(cost_ns)?;
        (next_deadline <= max_deadline).then_some(next_deadline)
    });
    update.is_ok()
}

/// The cost of one admission and the latest deadline the limiter may accumulate.
fn admission_bounds(interval_ns: u64, limit: u32, now: u64) -> Option<(u64, u64)> {
    if limit == 0 {
        return None;
    }
    let admissions = u64::from(limit);
    let cost_ns = interval_ns.div_ceil(admissions);
    let capacity_ns = admissions.checked_mul(cost_ns)?;
    Some((cost_ns, now.checked_add(capacity_ns)?))
}

fn has_room(deadline: &AtomicU64, interval_ns: u64, limit: u32, now: u64) -> bool {
    admission_bounds(interval_ns, limit, now).is_some_and(|(cost_ns, max_deadline)| {
        deadline
            .load(Ordering::Relaxed)
            .max(now)
            .checked_add(cost_ns)
            .is_some_and(|next_deadline| next_deadline <= max_deadline)
    })
}

fn debit_saturating(deadline: &AtomicU64, interval_ns: u64, limit: u32, now: u64) {
    let Some((cost_ns, max_deadline)) = admission_bounds(interval_ns, limit, now) else {
        return;
    };
    // `try_update` is the new name, but is not available on the workspace MSRV.
    #[allow(deprecated)]
    let _ = deadline.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous_deadline| {
        let next_deadline = previous_deadline
            .max(now)
            .saturating_add(cost_ns)
            .min(max_deadline);
        (next_deadline > previous_deadline).then_some(next_deadline)
    });
}

fn precheck_at(
    deadline: &AtomicU64,
    threshold: &SamplingThreshold,
    key: u64,
    interval_ns: u64,
    limit: u32,
    now: u64,
) -> bool {
    if key > threshold.threshold() {
        return false;
    }
    if has_room(deadline, interval_ns, limit, now) {
        return true;
    }
    threshold.record_pass(false, 1.0, limit, interval_ns, now);
    false
}

fn try_lead_at(
    deadline: &AtomicU64,
    threshold: &SamplingThreshold,
    interval_ns: u64,
    limit: u32,
    now: u64,
) -> bool {
    // Measured before admitting, so a candidate arriving at an idle limiter reads as zero.
    let pressure = debt(deadline, interval_ns, now).clamp(0.0, 1.0);
    let admitted = admit(deadline, interval_ns, limit, now);
    threshold.record_pass(admitted, pressure, limit, interval_ns, now);
    admitted
}

/// An adaptive sampling threshold, compared against a uniformly distributed 64-bit key.
///
/// Keys at or below the threshold pass. It aims at about `limit` passes per interval, the
/// fraction `limit / hit rate`: then the limiter rarely has to refuse a pass, and probes with
/// similar thresholds agree on the same keys. Only passes adjust it, so a key above the
/// threshold costs a single relaxed load.
///
/// A pass finding the limiter exhausted, or at least half used, lowers the threshold - but
/// only once per admission interval, so a burst of refused passes counts as one. A pass at an
/// idle limiter raises it by the number of admission intervals since the previous pass.
///
/// When it is applicable:
/// - Hits that should be sampled together share their key, and keys are uniform over u64 (e.g. the
///   random low 64 bits of a trace id). Fresh random keys per hit work, but coordinate nothing.
/// - The hit rate is well above `limit`. Below it the threshold stays at `u64::MAX` and only the
///   limiter acts.
/// - The hit rate changes slowly compared to the interval: the threshold halves at most once per
///   admission interval and grows by at most 4x per pass. Shorter bursts are cut by the limiter
///   alone, which samples whichever keys come first.
/// - Every hit goes through [`LocalLimiter::precheck`] and then [`LocalLimiter::try_lead`]. A
///   caller that skips the precheck never reports refusals, and the threshold cannot go down.
#[repr(C)]
pub struct SamplingThreshold {
    threshold: AtomicU64,
    last_pass: AtomicU64,
    last_decrease: AtomicU64,
}

impl Default for SamplingThreshold {
    fn default() -> Self {
        SamplingThreshold {
            threshold: AtomicU64::new(u64::MAX),
            last_pass: AtomicU64::new(0),
            last_decrease: AtomicU64::new(0),
        }
    }
}

impl SamplingThreshold {
    /// Only keeps the threshold from reaching zero, where no pass could ever raise it again.
    /// Hit rates high enough to make 2^-32 the right fraction do not occur.
    pub const FLOOR: u64 = u64::MAX >> 32;
    /// Pressure from which an admitted pass lowers the threshold.
    const HIGH_PRESSURE: f64 = 0.5;
    /// Upper bound of a single increase.
    const MAX_GROWTH: f64 = 4.0;

    pub fn threshold(&self) -> u64 {
        self.threshold.load(Ordering::Relaxed)
    }

    /// Return to admitting every key.
    pub fn reset(&self) {
        self.threshold.store(u64::MAX, Ordering::Relaxed);
        self.last_pass.store(0, Ordering::Relaxed);
        self.last_decrease.store(0, Ordering::Relaxed);
    }

    fn record_pass(&self, admitted: bool, pressure: f64, limit: u32, interval_ns: u64, now: u64) {
        if limit == 0 {
            return;
        }
        let last_pass = self.last_pass.swap(now, Ordering::Relaxed);
        let factor = if !admitted || pressure >= Self::HIGH_PRESSURE {
            // Racing passes may both decrease; losing that race is as harmless as the others.
            let cost_ns = interval_ns.div_ceil(u64::from(limit));
            let last_decrease = self.last_decrease.load(Ordering::Relaxed);
            if last_decrease != 0 && now.saturating_sub(last_decrease) < cost_ns {
                return;
            }
            self.last_decrease.store(now, Ordering::Relaxed);
            if admitted { 1.0 - pressure / 2.0 } else { 0.5 }
        } else if pressure <= 0.0 && last_pass != 0 {
            // u64 -> f64 is lossy, but this only steers a heuristic.
            let gap = now.saturating_sub(last_pass) as f64 * f64::from(limit) / interval_ns as f64;
            gap.clamp(1.0, Self::MAX_GROWTH)
        } else {
            return;
        };
        if factor == 1.0 {
            return;
        }
        // Concurrent passes may race here; losing one adjustment is harmless.
        let current = self.threshold.load(Ordering::Relaxed);
        // f64 -> u64 saturates, which is the clamping wanted at u64::MAX.
        let next = ((current as f64) * factor) as u64;
        self.threshold
            .store(next.max(Self::FLOOR), Ordering::Relaxed);
    }
}

fn debt(deadline: &AtomicU64, granularity: u64, now: u64) -> f64 {
    // Floating-point precision is sufficient for this diagnostic ratio.
    deadline.load(Ordering::Relaxed).saturating_sub(now) as f64 / granularity as f64
}

#[cfg(test)]
mod tests {
    use super::{
        LocalLimiter, MOCK_NOW, SamplingThreshold, TIME_PER_SECOND, admit, debit_saturating, debt,
        duration_nanos, has_room, now, precheck_at, try_lead_at,
    };
    use core::sync::atomic::{AtomicU64, Ordering};
    use core::time::Duration;

    fn set_mock_time(nanos: u64) {
        MOCK_NOW.store(nanos, Ordering::Relaxed);
    }

    fn advance_mock_time(nanos: u64) {
        MOCK_NOW.fetch_add(nanos, Ordering::Relaxed);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter() {
        set_mock_time(TIME_PER_SECOND);
        let limiter = LocalLimiter::default();
        let granularity = Duration::from_secs(1);

        assert!(limiter.inc(2, granularity));
        assert_eq!(limiter.rate(granularity), 0.5);
        assert!(limiter.inc(2, granularity));
        assert_eq!(limiter.rate(granularity), 1.0);
        assert!(!limiter.inc(2, granularity));

        advance_mock_time(TIME_PER_SECOND / 2);
        assert_eq!(limiter.rate(granularity), 0.5);
        assert!(limiter.inc(2, granularity));
        assert_eq!(limiter.rate(granularity), 1.0);

        advance_mock_time(TIME_PER_SECOND);
        assert_eq!(limiter.rate(granularity), 0.0);
        assert!(!limiter.inc(0, granularity));
        assert!(!limiter.inc(1, Duration::ZERO));

        set_mock_time(0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_now_monotonic() {
        let t1 = now();
        assert!(t1 > 0);
        let t2 = now();
        assert!(t2 >= t1);
    }

    #[test]
    fn test_local_limiter_algorithm() {
        let deadline = AtomicU64::new(0);
        assert!(!admit(&deadline, 100, 0, 1000));
        for _ in 0..3 {
            assert!(admit(&deadline, 100, 3, 1000));
        }
        assert!(!admit(&deadline, 100, 3, 1000));
        assert_eq!(deadline.load(Ordering::Relaxed), 1102);
        assert_eq!(debt(&deadline, 100, 1000), 1.02);
        assert!(!admit(&deadline, 100, 3, 1033));
        assert!(admit(&deadline, 100, 3, 1034));

        assert_eq!(duration_nanos(Duration::ZERO), None);
        assert_eq!(duration_nanos(Duration::from_secs(1)), Some(1_000_000_000));

        let deadline = AtomicU64::new(0);
        assert!(has_room(&deadline, 100, 2, 1000));
        debit_saturating(&deadline, 100, 2, 1000);
        assert!(has_room(&deadline, 100, 2, 1000));
        debit_saturating(&deadline, 100, 2, 1000);
        assert!(!has_room(&deadline, 100, 2, 1000));
        debit_saturating(&deadline, 100, 2, 1000);
        assert_eq!(
            deadline.load(Ordering::Relaxed),
            1100,
            "debit saturates at capacity"
        );
        assert!(!has_room(&deadline, 100, 2, 1049));
        assert!(has_room(&deadline, 100, 2, 1050));
        assert!(!has_room(&deadline, 100, 0, 1000));

        let limiter = LocalLimiter::default();
        assert_eq!(size_of::<LocalLimiter>(), size_of::<AtomicU64>());
        limiter.deadline.store(42, Ordering::Relaxed);
        limiter.reset();
        assert_eq!(limiter.deadline.load(Ordering::Relaxed), 0);
    }

    /// Feed `hits_per_sec` hits with uniformly spread keys for `secs` seconds, the way the
    /// tracer does (precheck, then try to lead). Return how many led in the last second and
    /// the lowest threshold seen.
    fn simulate(
        limiter: &LocalLimiter,
        threshold: &SamplingThreshold,
        limit: u32,
        hits_per_sec: u64,
        secs: u64,
        start: u64,
    ) -> (u64, u64) {
        let interval = TIME_PER_SECOND;
        let mut led_last_sec = 0;
        let mut lowest = threshold.threshold();
        let total = hits_per_sec * secs;
        // A Weyl sequence spreads consecutive hits over the whole key space.
        let mut key: u64 = start;
        for i in 0..total {
            let now = start + i * interval / hits_per_sec;
            key = key.wrapping_add(0x9E37_79B9_7F4A_7C15);
            if precheck_at(&limiter.deadline, threshold, key, interval, limit, now)
                && try_lead_at(&limiter.deadline, threshold, interval, limit, now)
                && i >= total - hits_per_sec
            {
                led_last_sec += 1;
            }
            lowest = lowest.min(threshold.threshold());
        }
        (led_last_sec, lowest)
    }

    fn assert_near(load: u64, t: u64, what: &str) {
        let expected = u64::MAX / load;
        assert!(
            t > expected / 8 && t < expected.saturating_mul(8),
            "load {load}: {what} threshold {t} far from {expected}"
        );
    }

    #[test]
    fn test_sampling_threshold_stays_open_under_budget() {
        let limiter = LocalLimiter::default();
        let threshold = SamplingThreshold::default();
        let (led, _) = simulate(&limiter, &threshold, 100, 20, 5, TIME_PER_SECOND);
        assert_eq!(led, 20);
        assert_eq!(threshold.threshold(), u64::MAX);
    }

    #[test]
    fn test_sampling_threshold_converges_and_recovers() {
        for load in [10, 100, 1000] {
            let limiter = LocalLimiter::default();
            let threshold = SamplingThreshold::default();
            let start = TIME_PER_SECOND;
            let (led, lowest) = simulate(&limiter, &threshold, 1, load, 30, start);
            assert_near(load, threshold.threshold(), "converged");
            assert!(
                lowest > u64::MAX / load / 16,
                "load {load}: overshot to {lowest}"
            );
            assert!(led <= 1, "load {load}: {led} led within one second");

            // Load drops to a tenth: the threshold must open up again.
            let t = threshold.threshold();
            let next = start + 31 * TIME_PER_SECOND;
            simulate(&limiter, &threshold, 1, load / 10 + 1, 60, next);
            assert!(
                threshold.threshold() > t,
                "load {load}: threshold did not recover"
            );
        }
    }

    #[test]
    fn test_sampling_threshold_survives_a_burst() {
        let limiter = LocalLimiter::default();
        let threshold = SamplingThreshold::default();
        let start = TIME_PER_SECOND;
        simulate(&limiter, &threshold, 1, 10, 30, start);
        let steady = threshold.threshold();

        // 100x the load for two seconds lowers the threshold at most once per interval.
        let (_, lowest) = simulate(
            &limiter,
            &threshold,
            1,
            1000,
            2,
            start + 30 * TIME_PER_SECOND,
        );
        assert!(
            lowest >= steady / 8,
            "burst crashed the threshold from {steady} to {lowest}"
        );

        simulate(
            &limiter,
            &threshold,
            1,
            10,
            30,
            start + 32 * TIME_PER_SECOND,
        );
        assert_near(10, threshold.threshold(), "recovered");
    }

    #[test]
    fn test_sampling_threshold_decreases_once_per_interval() {
        let threshold = SamplingThreshold::default();
        let interval = TIME_PER_SECOND;
        for i in 1..1000 {
            threshold.record_pass(false, 1.0, 1, interval, interval + i);
        }
        assert_eq!(threshold.threshold(), u64::MAX / 2 + 1);
        threshold.record_pass(false, 1.0, 1, interval, 2 * interval + 1);
        assert_eq!(threshold.threshold(), u64::MAX / 4 + 1);
    }

    #[test]
    fn test_sampling_threshold_floor_and_reset() {
        let threshold = SamplingThreshold::default();
        for i in 1..100 {
            threshold.record_pass(false, 1.0, 1, TIME_PER_SECOND, i * TIME_PER_SECOND);
        }
        assert_eq!(threshold.threshold(), SamplingThreshold::FLOOR);
        threshold.reset();
        assert_eq!(threshold.threshold(), u64::MAX);
    }
}
