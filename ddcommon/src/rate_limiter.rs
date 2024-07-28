// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Rate limiter implementations
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

fn duration_since_epoch() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

/// Token bucket rate limiter
///
/// This rate limiter is based on the token bucket algorithm. It allows for a burst of requests
/// up to the capacity of the bucket, with a refill rate of `capacity` per `interval` nanoseconds.
///
/// The rate limiter keeps track of the number of tokens consumed and the number of tokens allowed
/// in the current window. It also keeps track of the previous window rate to calculate the
/// effective rate.
///
/// The effective rate is calculated as the average of the current window rate and the previous
/// window rate.
///
/// <div class="warning">This implementation is not thread safe, consider wrapping in a <code>Mutex</code></div>
///
/// # Example
///
/// ```rust
/// use ddcommon::rate_limiter::TokenBucketRateLimiter;
/// use std::time::Duration;
///
/// // Create a rate limiter with a capacity of 100 tokens per second
/// let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
///
/// fn some_protected_function(limiter: &mut TokenBucketRateLimiter) {
///     if limiter.is_allowed(None) {
///         println!("Request allowed");
///     } else {
///         println!("Request denied");
///     }
/// }
/// ```
#[derive(Debug)]
pub struct TokenBucketRateLimiter {
    capacity: f64,
    interval: f64,
    tokens: f64,
    last_update: Duration,
    current_window: u64,
    tokens_total: u64,
    tokens_allowed: u64,
    previous_window_rate: Option<f64>,
}

impl TokenBucketRateLimiter {
    /// Create a new token bucket rate limiter
    ///
    /// # Arguments
    ///
    /// * `capacity` - The capacity of the token bucket per `interval`
    /// * `interval` - The refill interval in nanoseconds
    ///
    /// # Notes
    /// * If `capacity` is negative, all requests are allowed
    /// * If `capacity` is zero, no requests are allowed
    ///
    /// # Example
    ///
    /// ```rust
    /// use ddcommon::rate_limiter::TokenBucketRateLimiter;
    ///
    /// // Create a rate limiter with a capacity of 100 tokens per second
    /// let limiter = TokenBucketRateLimiter::new(100.0, 1e9);
    /// ```
    pub fn new(capacity: f64, interval: f64) -> Self {
        TokenBucketRateLimiter {
            capacity,
            interval,
            tokens: capacity,
            last_update: Duration::from_nanos(0),
            current_window: 0,
            tokens_total: 0,
            tokens_allowed: 0,
            previous_window_rate: None,
        }
    }

    /// Check if a request is allowed
    ///
    /// This method checks if a request is allowed based on the current state of the token bucket.
    ///
    /// # Arguments
    ///
    /// * `ts` - The timestamp of the request as a `Duration` since Unix Epoch. If `None`, the current time is used.
    ///
    /// # Returns
    ///
    /// * `true` if the request is allowed, `false` otherwise
    ///
    /// # Notes
    ///
    /// * If the ts time window is in the past, no requests are allowed
    ///   * These requests are not processed and do not affect the token counts
    ///
    /// # Example
    ///
    /// ```rust
    /// use ddcommon::rate_limiter::TokenBucketRateLimiter;
    ///
    /// let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
    ///
    /// if limiter.is_allowed(None) {
    ///     println!("Request allowed");
    /// } else {
    ///     println!("Request denied");
    /// }
    /// ```
    ///
    /// ```rust
    /// use ddcommon::rate_limiter::TokenBucketRateLimiter;
    /// use std::time::Duration;
    ///
    /// let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
    ///
    /// // Use a specific timestamp for the request
    /// let now = Duration::from_secs(1_722_079_058_866_801_000);
    ///
    /// if limiter.is_allowed(Some(now)) {
    ///     println!("Request allowed");
    /// } else {
    ///     println!("Request denied");
    /// }
    /// ```
    pub fn is_allowed(&mut self, ts: Option<Duration>) -> bool {
        if self.capacity < 0.0 {
            return true;
        } else if self.capacity == 0.0 {
            return false;
        }

        let now = ts.unwrap_or(duration_since_epoch());
        let now_ns = now.as_secs_f64() * 1e9;
        let window = (now_ns / self.interval).trunc() as u64;

        // Current window is in the past, always return false
        // DEV: This check MUST happen before the `allowed` check
        if window < self.current_window {
            return false;
        }

        if self.last_update == Duration::from_nanos(0) {
            self.last_update = now;
        }

        let allowed = (|| -> bool {
            let mut elapsed = Duration::from_nanos(0);
            if self.last_update < now {
                elapsed = now - self.last_update;
            }
            let elapsed_ns = elapsed.as_secs_f64() * 1e9;

            // Refill tokens if needed
            if self.tokens < self.capacity {
                let tokens_to_add = (elapsed_ns / self.interval) * self.capacity;
                if tokens_to_add > 0.0 {
                    self.tokens += tokens_to_add;
                    self.tokens = self.tokens.min(self.capacity);
                    self.last_update = now;
                }
            }

            if self.tokens >= 1.0 {
                self.tokens -= 1.0;
                return true;
            }
            false
        })();

        if window > self.current_window {
            if self.current_window != 0 {
                self.previous_window_rate = Some(self.current_window_rate());
            }
            self.current_window = window;
            self.tokens_total = 0;
            self.tokens_allowed = 0;
        }

        // Update the token counts
        self.tokens_total += 1;
        if allowed {
            self.tokens_allowed += 1;
        }

        allowed
    }

    /// Calculate the effective rate
    ///
    /// The effective rate is calculated as the average of the current window rate and the previous
    /// window rate.
    ///
    /// # Returns
    ///
    /// * The effective rate as a `f64`
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use std::time::SystemTime;
    /// use std::time::UNIX_EPOCH;
    /// use ddcommon::rate_limiter::TokenBucketRateLimiter;
    ///
    /// let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
    ///
    /// // Consume 200% of tokens in a single window
    /// let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    /// for _ in 0..200 {
    ///     limiter.is_allowed(Some(now));
    /// }
    /// let rate = limiter.effective_rate();
    /// assert_eq!(rate, 0.50);
    ///
    /// println!("Effective rate: {}", rate);
    /// ```
    pub fn effective_rate(&self) -> f64 {
        if self.capacity == 0.0 {
            return 0.0;
        } else if self.capacity < 0.0 {
            return 1.0;
        }

        let current_rate: f64 = self.current_window_rate();

        match self.previous_window_rate {
            None => current_rate,
            Some(prev_rate) => (current_rate + prev_rate) / 2.0,
        }
    }

    /// Calculate the current windows rate
    ///
    /// The current window rate is calculated as the number of tokens allowed divided by the total
    /// number of tokens seen.
    ///
    /// # Returns
    ///
    /// * The current window rate as a `f64`
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use std::time::SystemTime;
    /// use std::time::UNIX_EPOCH;
    /// use ddcommon::rate_limiter::TokenBucketRateLimiter;
    ///
    /// let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
    ///
    /// // Consume 200% of tokens in a single window
    /// let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    /// for _ in 0..200 {
    ///     limiter.is_allowed(Some(now));
    /// }
    /// let rate = limiter.current_window_rate();
    /// assert_eq!(rate, 0.50);
    ///
    /// println!("Current window rate: {}", rate);
    /// ```
    pub fn current_window_rate(&self) -> f64 {
        // If no tokens have been seen then return 1.0
        // DEV: This is to avoid a division by zero error
        if self.tokens_total == 0 {
            return 1.0;
        }

        self.tokens_allowed as f64 / self.tokens_total as f64
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Add;

    use super::*;
    use rstest::rstest;

    macro_rules! assert_is_allowed {
        ($limiter:expr, $ts:expr, $iterations:expr) => {
            for _ in 0..$iterations as u64 {
                assert!($limiter.is_allowed(Some($ts)));
            }
            assert!(!$limiter.is_allowed(Some($ts)));
        };
    }

    #[rstest]
    fn test_tbrl_new(
        #[values(1.0, 10.0, 1_000.0)] capacity: f64,
        #[values(1e3, 1e6, 1e9)] interval: f64,
    ) {
        let limiter = TokenBucketRateLimiter::new(capacity, interval);

        assert_eq!(limiter.capacity, capacity);
        assert_eq!(limiter.interval, interval);
        assert_eq!(limiter.tokens, capacity);
        assert_eq!(limiter.last_update, Duration::from_nanos(0));
        assert_eq!(limiter.current_window, 0);
        assert_eq!(limiter.tokens_total, 0);
        assert_eq!(limiter.tokens_allowed, 0);
    }

    #[rstest]
    fn test_tbrl_is_allowed(
        #[values(1.0, 10.0, 50.0, 100.0, 1_000.0)] capacity: f64,
        #[values(1e3, 1e6, 1e9)] interval: f64,
    ) {
        let mut limiter = TokenBucketRateLimiter::new(capacity, interval);
        let mut now = duration_since_epoch();
        assert_is_allowed!(limiter, now, capacity);

        now = now.add(Duration::from_nanos(interval as u64));
        assert_is_allowed!(limiter, now, capacity);
    }

    #[rstest]
    fn test_tbrl_is_allowed_capacity_zero(#[values(1e3, 1e6, 1e9)] interval: f64) {
        let mut limiter = TokenBucketRateLimiter::new(0.0, interval);
        let now = duration_since_epoch();
        for i in 0..1_000_000_u64 {
            assert!(!limiter.is_allowed(Some(now + Duration::from_nanos(interval as u64 * i))));
        }
    }
    #[rstest]
    fn test_tbrl_is_allowed_capacity_negative(#[values(1e3, 1e6, 1e9)] interval: f64) {
        let mut limiter = TokenBucketRateLimiter::new(-1.0, interval);
        let now = duration_since_epoch();
        for i in 0..1_000_000_u64 {
            assert!(limiter.is_allowed(Some(now + Duration::from_nanos(interval as u64 * i))));
        }
    }

    #[rstest]
    fn test_tbrl_is_allowed_old_window(#[values(2, 10, 100)] gap: u64) {
        let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
        let now = duration_since_epoch();
        assert_is_allowed!(limiter, now, 100.0);
        assert!(
            // Go back multiple intervals
            !limiter.is_allowed(Some(now - Duration::from_nanos(gap * 1e9 as u64)))
        );

        // We do not process an older window
        // We test with capacity + 1
        assert_eq!(limiter.tokens_total, 101);
        assert_eq!(limiter.tokens_allowed, 100);
    }

    #[rstest]
    fn test_tbrl_is_allowed_new_window(#[values(2, 10, 100)] gap: u64) {
        let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);
        let now = duration_since_epoch();
        assert_is_allowed!(limiter, now, 100.0);
        assert!(
            // Go forward multiple intervals
            limiter.is_allowed(Some(now + Duration::from_nanos(gap * 1e9 as u64)))
        );

        // We reset counters between windows
        assert_eq!(limiter.tokens_total, 1);
        assert_eq!(limiter.tokens_allowed, 1);
    }

    #[rstest]
    fn test_tbrl_is_allowed_small_gaps(#[values(1e3, 1e6, 1e9)] interval: f64) {
        let mut limiter = TokenBucketRateLimiter::new(100.0, interval);
        let now = duration_since_epoch();

        // Increment the interval by just a little to never run out of tokens
        let gap = interval as u64 / 100;
        for i in 0..1_000_000 {
            assert!(limiter.is_allowed(Some(now + Duration::from_nanos(gap * i))));
        }
    }

    #[rstest]
    fn test_tbrl_effective_rate() {
        let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);

        // Nothing consumed, we just return 1.0
        assert_eq!(limiter.effective_rate(), 1.0);

        let mut now = duration_since_epoch();

        // Consume all tokens in this window
        for _ in 0..100 {
            limiter.is_allowed(Some(now));
        }

        // Our effective rate should be 1.0
        assert_eq!(limiter.effective_rate(), 1.0);

        // Move forward 2 windows
        now = now.add(Duration::from_nanos(2 * 1e9 as u64));

        // Consume 200% of the tokens
        for _ in 0..200 {
            limiter.is_allowed(Some(now));
        }

        // Our effective rate should be 0.75 (previous 1.0 + 0.5 / 2)
        assert_eq!(limiter.effective_rate(), 0.75);

        // Move forward 2 windows
        now = now.add(Duration::from_nanos(2 * 1e9 as u64));

        // Consume 200% of the tokens
        for _ in 0..200 {
            limiter.is_allowed(Some(now));
        }

        // Our effective rate should be 0.5 (previous 0.5 + 0.5 / 2)
        assert_eq!(limiter.effective_rate(), 0.5);
    }

    #[rstest]
    fn test_tbrl_effective_rate_large_gap() {
        let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);

        let mut now = duration_since_epoch();

        limiter.is_allowed(Some(now));
        assert_eq!(limiter.effective_rate(), 1.0);

        // Move forward 10 windows
        // Even though we move really far ahead, we still consider the previous rate for
        // the effective rate calculation
        now = now.add(Duration::from_nanos(10 * 1e9 as u64));
        for _ in 0..200 {
            limiter.is_allowed(Some(now));
        }
        assert_eq!(limiter.effective_rate(), 0.75);
    }

    #[rstest]
    // Out of total capacity of 100
    // (capacity, first window tokens requested, expected rate)
    #[case(100.0, 100, 1.0)]
    #[case(100.0, 1, 1.0)]
    #[case(100.0, 75, 1.0)]
    #[case(100.0, 125, 0.8)]
    #[case(100.0, 250, 0.4)]
    #[case(20.0, 1000, 0.02)]
    #[case(-1.0, 100, 1.0)]
    #[case(-1.0, 1, 1.0)]
    #[case(0.0, 100, 0.0)]
    #[case(0.0, 1, 0.0)]
    fn test_tbrl_effective_rate_cases_one_window(
        #[case] capacity: f64,
        #[case] window_1_tokens: u64,
        #[case] expected_rate: f64,
    ) {
        let mut limiter = TokenBucketRateLimiter::new(capacity, 1e9);
        assert_eq!(limiter.previous_window_rate, None);

        let now = duration_since_epoch();

        for _ in 0..window_1_tokens {
            limiter.is_allowed(Some(now));
        }

        assert_eq!(limiter.previous_window_rate, None);
        assert_eq!(limiter.effective_rate(), expected_rate);
    }

    #[rstest]
    // Out of total capacity of 100
    // (first window tokens requested, second window tokens requested, expected rate)
    #[case(100, 100, 1.0)]
    #[case(1, 1, 1.0)]
    #[case(100, 200, 0.75)]
    #[case(1, 200, 0.75)]
    #[case(75, 75, 1.0)]
    #[case(125, 100, 0.9)]
    #[case(125, 100, 0.9)]
    #[case(250, 1000, 0.25)]
    fn test_tbrl_effective_rate_cases_two_windows(
        #[case] window_1_tokens: u64,
        #[case] window_2_tokens: u64,
        #[case] expected_rate: f64,
    ) {
        let mut limiter = TokenBucketRateLimiter::new(100.0, 1e9);

        let mut now = duration_since_epoch();

        for _ in 0..window_1_tokens {
            limiter.is_allowed(Some(now));
        }

        // Move forward 2 windows
        now = now.add(Duration::from_nanos(2 * 1e9 as u64));

        for _ in 0..window_2_tokens {
            limiter.is_allowed(Some(now));
        }

        assert_eq!(limiter.effective_rate(), expected_rate);
    }
}
