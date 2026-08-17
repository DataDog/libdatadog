// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::MutexExt;
use std::fmt;
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

/// Monotonic clock used by [`RateLimiter`].
///
/// Production code uses [`Clock::system`] (real wall-clock). Tests inject
/// [`Clock::mock`] so token replenishment and window rollover are
/// deterministic and do not depend on wall-clock timing.
#[derive(Clone)]
pub(crate) enum Clock {
    System,
    #[cfg(test)]
    Mock(Arc<MockClockInner>),
}

/// Shared state for the mock clock variant.
#[cfg(test)]
pub(crate) struct MockClockInner {
    now: Mutex<Instant>,
}

impl Clock {
    /// Real wall-clock source backed by [`std::time::Instant::now`].
    pub(crate) fn system() -> Self {
        Clock::System
    }

    /// Controllable clock for tests. Starts at an arbitrary `Instant` and only
    /// advances when [`Clock::advance`] is called.
    #[cfg(test)]
    pub(crate) fn mock() -> Self {
        Clock::Mock(Arc::new(MockClockInner {
            now: Mutex::new(Instant::now()),
        }))
    }

    /// Returns the current monotonic timestamp.
    pub(crate) fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            #[cfg(test)]
            Clock::Mock(inner) => *inner.now.lock().unwrap(),
        }
    }

    /// Advances the mock clock by `dur`. Panics if called on [`Clock::System`].
    #[cfg(test)]
    pub(crate) fn advance(&self, dur: Duration) {
        match self {
            Clock::System => panic!("cannot advance the system clock"),
            Clock::Mock(inner) => {
                let mut g = inner.now.lock().unwrap();
                // `Instant + Duration` is infallible on Rust ≥ 1.75; use
                // checked_add for defensiveness in case the test harness ever
                // runs on an older toolchain.
                *g = g.checked_add(dur).expect("clock overflow");
            }
        }
    }
}

/// A token bucket rate limiter implementation
#[derive(Clone)]
pub struct RateLimiter {
    /// Rate limit value that doesn't need to be protected by mutex
    rate_limit: i32,

    /// Clock used to read the current monotonic timestamp. Clones share the
    /// same time source (important for tests using the mock variant).
    clock: Clock,

    /// Inner state protected by a mutex for thread safety
    inner: Arc<Mutex<RateLimiterState>>,
}

/// The internal state of the rate limiter
struct RateLimiterState {
    /// The time window in nanoseconds where the rate limit applies
    time_window_ns: u64,

    /// Current number of tokens available
    tokens: i64,

    /// Maximum number of tokens that can be stored
    max_tokens: i64,

    /// Last time tokens were replenished
    last_update: Instant,

    /// Start time of the current window
    current_window_start: Option<Instant>,

    /// Number of tokens allowed in the current window
    tokens_allowed: u64,

    /// Total number of token requests in the current window
    tokens_total: u64,

    /// Rate from the previous window for calculating effective rate
    prev_window_rate: Option<f64>,
}

impl fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.lock_or_panic();

        let current_rate_val = self.current_window_rate(&state);
        let effective_rate_val =
            self.calculate_internal_effective_rate(current_rate_val, state.prev_window_rate);

        f.debug_struct("RateLimiter")
            .field("rate_limit", &self.rate_limit)
            .field("tokens", &state.tokens)
            .field("max_tokens", &state.max_tokens)
            .field("last_update", &state.last_update)
            .field("effective_rate", &effective_rate_val)
            .finish()
    }
}

impl RateLimiter {
    /// Creates a new RateLimiter with the given rate limit.
    ///
    /// # Parameters
    /// * `rate_limit` - Maximum number of requests allowed per `time_window_ns`:
    ///   * rate_limit > 0: max number of requests to allow per window
    ///   * rate_limit == 0: disallow all requests
    ///   * rate_limit < 0: allow all requests
    /// * `time_window_ns` - The time window in nanoseconds (default: 1,000,000,000 ns = 1 second).
    ///   With the default window `rate_limit` is equivalent to requests per second, but the actual
    ///   unit is always "requests per `time_window_ns`".
    pub fn new(rate_limit: i32, time_window_ns: Option<u64>) -> Self {
        Self::new_with_clock(Clock::system(), rate_limit, time_window_ns)
    }

    /// Creates a new [`RateLimiter`] with an explicit [`Clock`] source.
    ///
    /// Used by tests to make token replenishment and window rollover
    /// deterministic. Production callers should use [`RateLimiter::new`].
    pub(crate) fn new_with_clock(
        clock: Clock,
        rate_limit: i32,
        time_window_ns: Option<u64>,
    ) -> Self {
        let window_ns = time_window_ns.unwrap_or(1_000_000_000); // Default to 1 second in ns

        let state = RateLimiterState {
            time_window_ns: window_ns,
            tokens: rate_limit as i64,
            max_tokens: rate_limit as i64,
            last_update: clock.now(),
            current_window_start: None,
            tokens_allowed: 0,
            tokens_total: 0,
            prev_window_rate: None,
        };

        RateLimiter {
            rate_limit,
            clock,
            inner: Arc::new(Mutex::new(state)),
        }
    }

    /// Checks if the current request is allowed and consumes a token if it is.
    ///
    /// # Returns
    /// `true` if the request is allowed, `false` otherwise
    pub fn is_allowed(&self) -> bool {
        // Fast paths that don't touch the lock at all.
        if self.rate_limit == 0 {
            // Still need to update window counts so effective_rate() reflects denials.
            let now = self.clock.now();
            let mut state = self.inner.lock_or_panic();
            self.update_rate_counts_locked(&mut state, false, now);
            return false;
        }
        if self.rate_limit < 0 {
            let now = self.clock.now();
            let mut state = self.inner.lock_or_panic();
            self.update_rate_counts_locked(&mut state, true, now);
            return true;
        }

        let now = self.clock.now();
        let mut state = self.inner.lock_or_panic();

        // Try to consume a token; if not enough, replenish and try again.
        let allowed = if state.tokens >= 1 {
            state.tokens -= 1;
            true
        } else {
            self.replenish(&mut state, now);
            if state.tokens >= 1 {
                state.tokens -= 1;
                true
            } else {
                false
            }
        };

        self.update_rate_counts_locked(&mut state, allowed, now);
        allowed
    }

    /// Update counts used to determine effective rate. Caller must hold the lock.
    fn update_rate_counts_locked(
        &self,
        state: &mut RateLimiterState,
        allowed: bool,
        timestamp: Instant,
    ) {
        // No window start yet, start a new window
        if state.current_window_start.is_none() {
            state.current_window_start = Some(timestamp);
        }
        // If more time than the configured time window has passed, reset
        else if let Some(window_start) = state.current_window_start {
            let elapsed = timestamp.duration_since(window_start);
            if elapsed.as_nanos() as u64 >= state.time_window_ns {
                // Store previous window's rate
                state.prev_window_rate = Some(self.current_window_rate(state));
                state.tokens_allowed = 0;
                state.tokens_total = 0;
                state.current_window_start = Some(timestamp);
            }
        }

        // Keep track of total tokens seen vs allowed
        if allowed {
            state.tokens_allowed += 1;
        }
        state.tokens_total += 1;
    }

    /// Replenish tokens based on elapsed time
    fn replenish(&self, state: &mut RateLimiterState, timestamp: Instant) {
        let elapsed = timestamp.duration_since(state.last_update);

        // Calculate new tokens to add
        let tokens_to_add: i64 = ((elapsed.as_nanos() as f64 / state.time_window_ns as f64)
            * self.rate_limit as f64) as i64; // Truncates fractional tokens

        if tokens_to_add > 0 {
            state.tokens += tokens_to_add;
            // Cap tokens at max_tokens. Since state.tokens started < 1 and max_tokens > 0,
            // state.tokens was definitely < max_tokens before adding.
            if state.tokens > state.max_tokens {
                state.tokens = state.max_tokens;
            }
            // Only advance last_update by the time consumed by whole tokens, preserving
            // fractional progress toward the next token. Use u128 to avoid overflow in
            // the intermediate product; integer division guarantees consumed_ns ≤ elapsed.
            let consumed_ns = (tokens_to_add as u128 * state.time_window_ns as u128
                / self.rate_limit as u128) as u64;
            state.last_update += std::time::Duration::from_nanos(consumed_ns);
        }
    }

    /// Calculate the current window rate
    fn current_window_rate(&self, state: &RateLimiterState) -> f64 {
        // No tokens have been seen, effectively 100% sample rate
        if state.tokens_total == 0 {
            return 1.0;
        }

        // Get rate of tokens allowed
        state.tokens_allowed as f64 / state.tokens_total as f64
    }

    /// Helper function to calculate the effective rate based on current and optional previous rate.
    fn calculate_internal_effective_rate(
        &self, // Takes &self to be a method, though it doesn't use self directly
        current_rate: f64,
        prev_window_rate_opt: Option<f64>,
    ) -> f64 {
        if let Some(prev_rate) = prev_window_rate_opt {
            (current_rate + prev_rate) / 2.0
        } else {
            current_rate
        }
    }

    /// Returns the effective sample rate of this rate limiter (between 0.0 and 1.0)
    pub fn effective_rate(&self) -> f64 {
        let state = self.inner.lock_or_panic();
        let current_rate = self.current_window_rate(&state);
        // Use the new helper
        self.calculate_internal_effective_rate(current_rate, state.prev_window_rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_rate_limiter_allow_all() {
        let limiter = RateLimiter::new(-1, None);

        // Should allow all requests
        for _ in 0..100 {
            assert!(limiter.is_allowed());
        }

        // Effective rate should be 1.0 (100%)
        assert_eq!(limiter.effective_rate(), 1.0);
    }

    #[test]
    fn test_rate_limiter_block_all() {
        let limiter = RateLimiter::new(0, None);

        // Should block all requests
        for _ in 0..10 {
            assert!(!limiter.is_allowed());
        }

        // Effective rate should be 0.0 (0%)
        assert_eq!(limiter.effective_rate(), 0.0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter_accumulates_fractional_tokens() {
        // With rate=2/s each token takes 500ms. Sleeping 300ms twice (600ms total) must
        // yield at least one token. Before the fix, each sub-token call reset last_update
        // to the call time, so the second 300ms window also computed only 0.6 tokens and
        // the limiter starved indefinitely. Margins: the first assert!(!..) has 200ms of
        // headroom below 500ms; the final assert!(..) has 100ms of headroom above 500ms.
        let limiter = RateLimiter::new(2, None);

        // Drain all initial tokens.
        for _ in 0..2 {
            assert!(limiter.is_allowed());
        }
        assert!(!limiter.is_allowed());

        // First sleep: 300ms → 0.6 tokens, not enough to allow.
        thread::sleep(Duration::from_millis(300));
        assert!(!limiter.is_allowed());

        // Second sleep: another 300ms. Total elapsed since drain ≈ 600ms → 1.2 tokens.
        // The fix preserves fractional progress so this succeeds; the old code reset
        // last_update on the first call and only saw another 0.6 tokens here.
        thread::sleep(Duration::from_millis(300));
        assert!(limiter.is_allowed());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter_limit_rate() {
        let limiter = RateLimiter::new(5, None); // 5 per second

        // Should allow exactly 5 requests
        for _ in 0..5 {
            assert!(limiter.is_allowed());
        }

        // 6th request should be blocked
        assert!(!limiter.is_allowed());

        // Wait for tokens to replenish
        thread::sleep(Duration::from_millis(200)); // 0.2s * 5 tokens/s ≈ 1 token

        // Should allow one more request
        assert!(limiter.is_allowed());

        // But the next one should be blocked
        assert!(!limiter.is_allowed());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter_effective_rate() {
        let limiter = RateLimiter::new(50, None); // 50 per second

        // Request 100 tokens when only 50 are available
        let mut allowed_count = 0;
        for _ in 0..100 {
            if limiter.is_allowed() {
                allowed_count += 1;
            }
        }

        // Should have allowed about 50 requests
        assert_eq!(allowed_count, 50);

        // Effective rate should be about 0.5 (50%)
        let rate = limiter.effective_rate();
        assert!(
            (0.45..=0.55).contains(&rate),
            "Expected rate around 0.5, got {rate}",
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter_thread_safety_no_panic() {
        // Frozen clock: no replenishment, no window rollover. The only thing this
        // test asserts is that a shared limiter does not panic under contention —
        // `join().unwrap()` propagates any panic from the spawned thread.
        let clock = Clock::mock();
        let limiter = RateLimiter::new_with_clock(clock.clone(), 100, None);
        let limiter_clone = limiter.clone();

        let handle = thread::spawn(move || {
            for _ in 0..100 {
                let _ = limiter_clone.is_allowed();
            }
        });
        for _ in 0..100 {
            let _ = limiter.is_allowed();
        }
        handle.join().expect("limiter panicked under contention");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter_budget_invariant_concurrent() {
        // Frozen clock ⇒ `replenish` always computes 0 tokens and the window never
        // resets. Four threads race on a shared limiter with a budget of 100; the
        // total number of allowed requests must be exactly 100, and the next
        // request must be denied. No timing slack: this is an exact invariant.
        let clock = Clock::mock();
        let limiter = Arc::new(RateLimiter::new_with_clock(clock.clone(), 100, None));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let l = limiter.clone();
            handles.push(thread::spawn(move || {
                let mut allowed = 0;
                for _ in 0..50 {
                    if l.is_allowed() {
                        allowed += 1;
                    }
                }
                allowed
            }));
        }
        let total: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(
            total, 100,
            "token budget must be exactly exhausted with a frozen clock"
        );
        // Budget is gone; the next request must be denied.
        assert!(!limiter.is_allowed());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rate_limiter_window_reset_under_contention() {
        // Deterministic window reset: drain the first window, advance the mock
        // clock past the window boundary, then race on the fresh window. The
        // second window must have a full budget again.
        let clock = Clock::mock();
        let limiter = Arc::new(RateLimiter::new_with_clock(
            clock.clone(),
            10,
            Some(1_000_000),
        ));

        // Drain the first window (10 tokens, 1ms window).
        let drainer = {
            let l = limiter.clone();
            thread::spawn(move || {
                let mut n = 0;
                for _ in 0..10 {
                    if l.is_allowed() {
                        n += 1;
                    }
                }
                n
            })
        };
        assert_eq!(drainer.join().unwrap(), 10);
        assert!(!limiter.is_allowed(), "first window must be exhausted");

        // Advance the clock past the 1ms window. Deterministic reset.
        clock.advance(Duration::from_millis(2));

        // Race on the fresh window: 5 + 5 = 10 requests, budget is 10 again.
        let b = {
            let l = limiter.clone();
            thread::spawn(move || {
                let mut n = 0;
                for _ in 0..5 {
                    if l.is_allowed() {
                        n += 1;
                    }
                }
                n
            })
        };
        let mut main_n = 0;
        for _ in 0..5 {
            if limiter.is_allowed() {
                main_n += 1;
            }
        }
        let b_allowed = b.join().unwrap();
        assert_eq!(
            b_allowed + main_n,
            10,
            "second window must have a full budget after deterministic reset"
        );
        assert!(!limiter.is_allowed(), "second window must be exhausted");
    }

    #[test]
    fn test_debug_impl() {
        let limiter = RateLimiter::new(10, None);
        let dbg = format!("{limiter:?}");
        assert!(dbg.contains("RateLimiter"));
    }

    #[test]
    fn test_window_reset_and_token_cap() {
        use std::time::Duration;
        // 1 token per 1ms window
        let limiter = RateLimiter::new(1, Some(1_000_000));
        // Exhaust the initial token
        assert!(limiter.is_allowed());
        // Wait > 1 window to trigger window reset + prev_window_rate assignment
        std::thread::sleep(Duration::from_millis(5));
        // After reset, should allow again (token cap branch also exercised)
        assert!(limiter.is_allowed());
    }

    #[test]
    fn test_effective_rate_zero_total() {
        // New limiter with no calls: effective_rate should be 1.0 (no data = pass through)
        let limiter = RateLimiter::new(10, None);
        let rate = limiter.effective_rate();
        assert_eq!(rate, 1.0);
    }
}
