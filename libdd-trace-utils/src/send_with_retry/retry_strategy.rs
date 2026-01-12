// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Types used when calling [`super::send_with_retry`] to configure the retry logic.

use std::time::Duration;
use tokio::time::sleep;

/// Enum representing the type of backoff to use for the delay between retries.
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub enum RetryBackoffType {
    /// Increases the delay by a fixed increment each attempt.
    Linear,
    /// The delay is constant for each attempt.
    Constant,
    /// The delay is doubled for each attempt.
    Exponential,
}

/// Struct representing the retry strategy for sending data.
///
/// This struct contains the parameters that define how retries should be handled when sending data.
/// It includes the maximum number of retries, the delay between retries, the type of backoff to
/// use, and an optional jitter to add randomness to the delay.
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct RetryStrategy {
    /// The maximum number of retries to attempt.
    max_retries: u32,
    // The minimum delay between retries.
    delay_ms: Duration,
    /// The type of backoff to use for the delay between retries.
    backoff_type: RetryBackoffType,
    /// An optional jitter to add randomness to the delay.
    jitter: Option<Duration>,
}

impl Default for RetryStrategy {
    fn default() -> Self {
        RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Exponential,
            jitter: None,
        }
    }
}

impl RetryStrategy {
    /// Creates a new `RetryStrategy` with the specified parameters.
    ///
    /// # Arguments
    ///
    /// * `max_retries`: The maximum number of retries to attempt.
    /// * `delay_ms`: The minimum delay between retries, in milliseconds.
    /// * `backoff_type`: The type of backoff to use for the delay between retries.
    /// * `jitter`: An optional jitter to add randomness to the delay, in milliseconds.
    ///
    /// # Returns
    ///
    /// A `RetryStrategy` instance with the specified parameters.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
    /// use std::time::Duration;
    ///
    /// let retry_strategy = RetryStrategy::new(5, 100, RetryBackoffType::Exponential, Some(50));
    /// ```
    pub fn new(
        max_retries: u32,
        delay_ms: u64,
        backoff_type: RetryBackoffType,
        jitter: Option<u64>,
    ) -> RetryStrategy {
        RetryStrategy {
            max_retries,
            delay_ms: Duration::from_millis(delay_ms),
            backoff_type,
            jitter: jitter.map(Duration::from_millis),
        }
    }
    /// Delays the next request attempt based on the retry strategy.
    ///
    /// If a jitter duration is specified in the retry strategy, a random duration up to the jitter
    /// value is added to the delay.
    ///
    /// # Arguments
    ///
    /// * `attempt`: The number of the current attempt (1-indexed).
    pub(crate) async fn delay(&self, attempt: u32) {
        let delay = match self.backoff_type {
            RetryBackoffType::Exponential => self.delay_ms * 2u32.pow(attempt - 1),
            RetryBackoffType::Constant => self.delay_ms,
            RetryBackoffType::Linear => self.delay_ms + (self.delay_ms * (attempt - 1)),
        };

        if let Some(jitter) = self.jitter {
            let jitter = rand::random::<u64>() % jitter.as_millis() as u64;
            sleep(delay + Duration::from_millis(jitter)).await;
        } else {
            sleep(delay).await;
        }
    }

    /// Returns the maximum number of retries.
    pub(crate) fn max_retries(&self) -> u32 {
        self.max_retries
    }
}

#[cfg(test)]
// For tests RetryStrategy tests the observed delay should be approximate.
mod tests {
    use super::*;
    use tokio::time::Instant;

    // This tolerance is on the higher side to account for github's runners not having consistent
    // performance. It shouldn't impact the quality of the tests since the most important aspect
    // of the retry logic is we wait a minimum amount of time.
    const RETRY_STRATEGY_TIME_TOLERANCE_MS: u64 = 100;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_constant() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );

        let start = Instant::now();
        retry_strategy.delay(2).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_linear() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Linear,
            jitter: None,
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );

        let start = Instant::now();
        retry_strategy.delay(3).await;
        let elapsed = start.elapsed();

        // For the Linear strategy, the delay for the 3rd attempt should be delay_ms + (delay_ms *
        // 2).
        assert!(
            elapsed >= retry_strategy.delay_ms + (retry_strategy.delay_ms * 2)
                && elapsed
                    <= retry_strategy.delay_ms
                        + (retry_strategy.delay_ms * 2)
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_exponential() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Exponential,
            jitter: None,
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );

        let start = Instant::now();
        retry_strategy.delay(3).await;
        let elapsed = start.elapsed();
        // For the Exponential strategy, the delay for the 3rd attempt should be delay_ms * 2^(3-1)
        // = delay_ms * 4.
        assert!(
            elapsed >= retry_strategy.delay_ms * 4
                && elapsed
                    <= retry_strategy.delay_ms * 4
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_jitter() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Constant,
            jitter: Some(Duration::from_millis(50)),
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        // The delay should be between delay_ms and delay_ms + jitter
        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + retry_strategy.jitter.unwrap()
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time of {} ms was not within expected range",
            elapsed.as_millis()
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_max_retries() {
        let retry_strategy = RetryStrategy {
            max_retries: 17,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Constant,
            jitter: Some(Duration::from_millis(50)),
        };

        assert_eq!(
            retry_strategy.max_retries(),
            17,
            "Max retries did not match expected value"
        );
    }
}
