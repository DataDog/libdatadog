// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

pub trait Limiter {
    /// Takes the limit per interval.
    /// Returns false if the limit is exceeded, otherwise true.
    fn inc(&self, limit: u32) -> bool;
    /// Returns the effective rate per interval.
    /// Note: The rate is only guaranteed to be accurate immediately after a call to inc().
    fn rate(&self) -> f64;
    /// Updates the rate and returns it
    fn update_rate(&self) -> f64;
}

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

        let (mut ticks, mut frequency) = (0, 0);
        // SAFETY: both output pointers refer to valid writable storage.
        if unsafe { QueryPerformanceCounter(&mut ticks) } == 0
            || unsafe { QueryPerformanceFrequency(&mut frequency) } == 0
        {
            return None;
        }
        let nanos = u128::try_from(ticks)
            .ok()?
            .checked_mul(u128::from(TIME_PER_SECOND))?
            .checked_div(u128::try_from(frequency).ok()?)?;
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

fn admit(deadline: &AtomicU64, granularity: u64, limit: u32, now: u64) -> bool {
    if limit == 0 {
        return false;
    }
    let quantum = granularity.div_ceil(u64::from(limit));
    let Some(window) = u64::from(limit).checked_mul(quantum) else {
        return false;
    };
    let Some(ceiling) = now.checked_add(window) else {
        return false;
    };
    deadline
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous| {
            previous
                .max(now)
                .checked_add(quantum)
                .filter(|next| *next <= ceiling)
        })
        .is_ok()
}

fn debt(deadline: &AtomicU64, granularity: u64, now: u64) -> f64 {
    // Floating-point precision is sufficient for this diagnostic ratio.
    deadline.load(Ordering::Relaxed).saturating_sub(now) as f64 / granularity as f64
}

#[cfg(test)]
mod tests {
    use crate::rate_limiter::{
        LocalLimiter, MOCK_NOW, TIME_PER_SECOND, admit, debt, duration_nanos, now,
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

        let limiter = LocalLimiter::default();
        assert_eq!(size_of::<LocalLimiter>(), size_of::<AtomicU64>());
        limiter.deadline.store(42, Ordering::Relaxed);
        limiter.reset();
        assert_eq!(limiter.deadline.load(Ordering::Relaxed), 0);
    }
}
