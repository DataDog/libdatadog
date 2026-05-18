// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

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

/// A thread-safe limiter built on Atomics.
/// It's base unit is in seconds, i.e. the minimum allowed rate is 1 per second.
/// Internally the limiter works with the system time granularity, i.e. nanoseconds on unix and
/// milliseconds on windows.
/// The implementation is a sliding window: every time the limiter is increased, the amount of time
/// that has passed is also refilled.
#[repr(C)]
pub struct LocalLimiter {
    hit_count: AtomicI64,
    last_update: AtomicU64,
    last_limit: AtomicU32,
    granularity: i64,
}

const TIME_PER_SECOND: i64 = 1_000_000_000; // nanoseconds

/// When set to a non-zero value, `now()` returns this instead of the real clock.
/// This allows tests to control time deterministically, avoiding flakiness from
/// wall-clock timing on CI machines.
#[cfg(test)]
static MOCK_NOW: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    #[cfg(test)]
    {
        let mock = MOCK_NOW.load(Ordering::Relaxed);
        if mock != 0 {
            return mock;
        }
    }
    #[cfg(windows)]
    let now = unsafe {
        static FREQUENCY: AtomicU64 = AtomicU64::new(0);

        let mut frequency = FREQUENCY.load(Ordering::Relaxed);
        if frequency == 0 {
            windows_sys::Win32::System::Performance::QueryPerformanceFrequency(
                &mut frequency as *mut u64 as *mut i64,
            );
            FREQUENCY.store(frequency, Ordering::Relaxed);
        }

        let mut perf_counter = 0;
        windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut perf_counter);
        perf_counter as u64 * frequency / TIME_PER_SECOND as u64
    };
    #[cfg(not(windows))]
    let now = {
        let mut ts: libc::timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        // tv_sec is i32 on 32bit architecture
        // https://sourceware.org/bugzilla/show_bug.cgi?id=16437
        #[cfg(target_pointer_width = "32")]
        {
            (ts.tv_sec as i64 * TIME_PER_SECOND + ts.tv_nsec as i64) as u64
        }
        #[cfg(target_pointer_width = "64")]
        {
            (ts.tv_sec * TIME_PER_SECOND + ts.tv_nsec) as u64
        }
    };
    now
}

impl Default for LocalLimiter {
    fn default() -> Self {
        LocalLimiter {
            hit_count: Default::default(),
            last_update: AtomicU64::from(now()),
            last_limit: Default::default(),
            granularity: TIME_PER_SECOND,
        }
    }
}

impl LocalLimiter {
    /// Allows setting a custom time granularity. The default() implementation is 1 second.
    pub fn with_granularity(seconds: u32) -> LocalLimiter {
        let mut limiter = LocalLimiter::default();
        limiter.granularity *= seconds as i64;
        limiter
    }

    /// Resets, with a given granularity.
    pub fn reset(&mut self, seconds: u32) {
        self.last_update.store(now(), Ordering::Relaxed);
        self.hit_count.store(0, Ordering::Relaxed);
        self.last_limit.store(0, Ordering::Relaxed);
        self.granularity = TIME_PER_SECOND * seconds as i64;
    }

    fn update(&self, limit: u32, inc: i64) -> i64 {
        let now = now();
        let last = self.last_update.swap(now, Ordering::SeqCst);
        // Make sure reducing the limit doesn't stall for a long time
        let clear_limit = limit.max(self.last_limit.load(Ordering::Relaxed));
        let clear_counter = (now as i64 - last as i64) * (clear_limit as i64);
        let subtract = clear_counter - inc;
        let mut previous_hits = self.hit_count.fetch_sub(subtract, Ordering::SeqCst);
        // Handle where the limiter goes below zero
        if previous_hits < subtract {
            let add = clear_counter - previous_hits.max(0);
            self.hit_count.fetch_add(add, Ordering::Acquire);
            previous_hits += add - clear_counter;
        }
        previous_hits
    }
}

impl Limiter for LocalLimiter {
    fn inc(&self, limit: u32) -> bool {
        let previous_hits = self.update(limit, self.granularity);
        if previous_hits / self.granularity >= limit as i64 {
            self.hit_count
                .fetch_sub(self.granularity, Ordering::Acquire);
            false
        } else {
            // We don't care about race conditions here:
            // If the last limit was high enough to increase the previous_hits, we are anyway close
            // to a number realistic to decrease the count quickly; i.e. we won't stall the limiter
            // indefinitely when switching from a high to a low limit.
            self.last_limit.store(limit, Ordering::Relaxed);
            true
        }
    }

    fn rate(&self) -> f64 {
        let last_limit = self.last_limit.load(Ordering::Relaxed);
        let hit_count = self.hit_count.load(Ordering::Relaxed);
        (hit_count as f64 / (last_limit as i64 * self.granularity) as f64).clamp(0., 1.)
    }

    fn update_rate(&self) -> f64 {
        self.update(0, self.granularity);
        self.rate()
    }
}

#[cfg(test)]
mod tests {
    use crate::rate_limiter::{now, Limiter, LocalLimiter, MOCK_NOW, TIME_PER_SECOND};
    use std::sync::atomic::Ordering;

    fn set_mock_time(nanos: u64) {
        MOCK_NOW.store(nanos, Ordering::Relaxed);
    }

    fn advance_mock_time(nanos: u64) {
        MOCK_NOW.fetch_add(nanos, Ordering::Relaxed);
    }

    /// A small time tick (100ns) used to simulate minimal time passing between operations.
    const TICK: u64 = 100;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter() {
        // Use mock time for deterministic behavior — real wall-clock sleeps are flaky on CI.
        set_mock_time(1_000_000_000);

        let limiter = LocalLimiter::default();

        // First inc uses 1 of 2 slots: rate is exactly 0.5
        assert!(limiter.inc(2));
        assert_eq!(0.5, limiter.rate());

        // Second inc: rate approaches 1.0 but not quite (tiny time elapsed)
        advance_mock_time(TICK);
        assert!(limiter.inc(2));
        assert!(limiter.rate() > 0.5 && limiter.rate() < 1.);

        // Third inc fills the bucket: rate clamps to 1.0
        advance_mock_time(TICK);
        assert!(limiter.inc(2));
        assert_eq!(1., limiter.rate());

        // Over limit — both rejected
        advance_mock_time(TICK);
        assert!(!limiter.inc(2));
        advance_mock_time(TICK);
        assert!(!limiter.inc(2));

        // 3 seconds pass — capacity fully refills, hit count goes negative then resets to zero
        advance_mock_time(3 * TIME_PER_SECOND as u64);
        assert!(limiter.inc(2));
        assert_eq!(0.5, limiter.rate()); // Starting from scratch

        advance_mock_time(TICK);
        assert!(limiter.inc(2));
        advance_mock_time(TICK);
        assert!(limiter.inc(2));
        advance_mock_time(TICK);
        assert!(!limiter.inc(2));

        // Test change to higher limit
        advance_mock_time(TICK);
        assert!(limiter.inc(3));
        advance_mock_time(TICK);
        assert!(!limiter.inc(3));

        // Change to lower limit — no capacity available
        assert!(!limiter.inc(1));

        // 2 seconds pass — the counter resets (last successful limit was 3, so subtracting
        // 3 per second twice clears it)
        advance_mock_time(2 * TIME_PER_SECOND as u64);

        // Now 1 succeeds again
        assert!(limiter.inc(1));

        set_mock_time(0);
    }

    /// Validates the real clock implementation (MOCK_NOW is 0, so `now()` hits the actual
    /// platform clock).
    // We normally shouldn't test private functions directly, but is necessary here since
    // now() is mocked for the other tests.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_now_monotonic() {
        let t1 = now();
        assert!(t1 > 0);
        let t2 = now();
        assert!(t2 >= t1);
    }
}
