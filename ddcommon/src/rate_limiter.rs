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

fn now() -> u64 {
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
        (ts.tv_sec * TIME_PER_SECOND + ts.tv_nsec) as u64
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
    use crate::rate_limiter::{Limiter, LocalLimiter, TIME_PER_SECOND};
    use std::sync::atomic::Ordering;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter() {
        let limiter = LocalLimiter::default();
        // Two are allowed, then one more because a small amount of time passed since the first one
        assert!(limiter.inc(2));
        // Work around floating point precision issues
        assert!(limiter.rate() > 0.49999 && limiter.rate() <= 0.5);
        // Add a minimal amount of time to ensure the test doesn't run faster than timer precision
        sleep(Duration::from_micros(100));
        assert!(limiter.inc(2));
        // We're close to 1, but not quite, due to the minimal time passed
        assert!(limiter.rate() > 0.5 && limiter.rate() < 1.);
        sleep(Duration::from_micros(100));
        assert!(limiter.inc(2));
        // Rate capped at 1
        assert_eq!(1., limiter.rate());
        sleep(Duration::from_micros(100));
        assert!(!limiter.inc(2));
        sleep(Duration::from_micros(100));
        assert!(!limiter.inc(2));
        sleep(Duration::from_micros(100));

        // reduce 4 times, we're going into negative territory. Next increment will reset to zero.
        limiter
            .last_update
            .fetch_sub(3 * TIME_PER_SECOND as u64, Ordering::Relaxed);
        assert!(limiter.inc(2));
        // Work around floating point precision issues
        assert!(limiter.rate() > 0.49999 && limiter.rate() <= 0.5); // We're starting from scratch
        sleep(Duration::from_micros(100));
        assert!(limiter.inc(2));
        sleep(Duration::from_micros(100));
        assert!(limiter.inc(2));
        sleep(Duration::from_micros(100));
        assert!(!limiter.inc(2));
        sleep(Duration::from_micros(100));

        // Test change to higher value
        assert!(limiter.inc(3));
        sleep(Duration::from_micros(100));
        assert!(!limiter.inc(3));

        // Then change to lower value - but we have no capacity
        assert!(!limiter.inc(1));

        // The counter is around 4 (because last limit was 3)
        // We're keeping the highest successful limit stored, thus subtracting 3 twice will reset it
        limiter
            .last_update
            .fetch_sub(2 * TIME_PER_SECOND as u64, Ordering::Relaxed);

        // And now 1 succeeds again.
        assert!(limiter.inc(1));
    }
}
