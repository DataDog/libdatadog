// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use crate::HttpClientError;

/// Configuration for automatic request retries with exponential backoff.
///
/// Retry is opt-in — pass a `RetryConfig` to
/// [`crate::HttpClientBuilder::retry`] to enable it.
///
/// All errors are retried except [`HttpClientError::InvalidConfig`].
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub(crate) max_retries: u32,
    pub(crate) initial_delay: Duration,
    pub(crate) jitter: bool,
}

impl RetryConfig {
    /// Create a new retry config with defaults: 3 retries, 100ms initial
    /// delay, exponential backoff with jitter.
    pub fn new() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(100),
            jitter: true,
        }
    }

    /// Set the maximum number of retry attempts (not counting the initial
    /// request).
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Set the initial delay before the first retry. Subsequent retries
    /// double this value (exponential backoff).
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Enable or disable jitter. When enabled, each delay is replaced with
    /// a uniform random value between 0 and the calculated delay.
    pub fn with_jitter(mut self, jitter: bool) -> Self {
        self.jitter = jitter;
        self
    }

    /// Calculate the delay for a given attempt (1-indexed).
    ///
    /// Exponential backoff: `initial_delay * 2^(attempt - 1)`.
    /// With jitter: uniform random from 0 to the calculated delay.
    pub(crate) fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self
            .initial_delay
            .saturating_mul(2u32.saturating_pow(attempt - 1));
        if self.jitter {
            let base_nanos = base.as_nanos() as u64;
            if base_nanos == 0 {
                return Duration::ZERO;
            }
            let jittered = fastrand::u64(0..base_nanos);
            Duration::from_nanos(jittered)
        } else {
            base
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns true if the error is retryable.
pub(crate) fn is_retryable(err: &HttpClientError) -> bool {
    !matches!(err, HttpClientError::InvalidConfig(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = RetryConfig::new();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_delay, Duration::from_millis(100));
        assert!(config.jitter);
    }

    #[test]
    fn builder_methods() {
        let config = RetryConfig::new()
            .max_retries(5)
            .initial_delay(Duration::from_millis(200))
            .with_jitter(false);
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.initial_delay, Duration::from_millis(200));
        assert!(!config.jitter);
    }

    #[test]
    fn exponential_backoff_without_jitter() {
        let config = RetryConfig::new()
            .initial_delay(Duration::from_millis(100))
            .with_jitter(false);
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(100));
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(200));
        assert_eq!(config.delay_for_attempt(3), Duration::from_millis(400));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let config = RetryConfig::new()
            .initial_delay(Duration::from_millis(100))
            .with_jitter(true);
        for _ in 0..100 {
            let delay = config.delay_for_attempt(1);
            assert!(delay <= Duration::from_millis(100));
        }
    }

    #[test]
    fn retryable_errors() {
        assert!(is_retryable(&HttpClientError::ConnectionFailed(
            "refused".to_owned()
        )));
        assert!(is_retryable(&HttpClientError::IoError(
            "broken pipe".to_owned()
        )));
        assert!(is_retryable(&HttpClientError::RequestFailed {
            status: 503,
            body: "unavailable".to_owned(),
        }));
        assert!(is_retryable(&HttpClientError::RequestFailed {
            status: 404,
            body: "not found".to_owned(),
        }));
        assert!(is_retryable(&HttpClientError::TimedOut));
    }

    #[test]
    fn invalid_config_not_retryable() {
        assert!(!is_retryable(&HttpClientError::InvalidConfig(
            "bad".to_owned()
        )));
    }
}
