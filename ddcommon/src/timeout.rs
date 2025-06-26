// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::{Duration, Instant};

pub struct TimeoutManager {
    start_time: Instant,
    timeout: Duration,
}

impl TimeoutManager {
    // 4ms per sched slice, give ~4x10 slices for safety
    const MINIMUM_REAP_TIME: Duration = Duration::from_millis(160);

    pub fn new(timeout: Duration) -> Self {
        Self {
            start_time: Instant::now(),
            timeout,
        }
    }

    pub fn remaining(&self) -> Duration {
        // If elapsed > timeout, remaining will be 0
        let elapsed = self.start_time.elapsed();
        if elapsed >= self.timeout {
            Self::MINIMUM_REAP_TIME
        } else {
            (self.timeout - elapsed).max(Self::MINIMUM_REAP_TIME)
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl std::fmt::Debug for TimeoutManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeoutManager")
            .field("start_time", &self.start_time)
            .field("elapsed", &self.elapsed())
            .field("timeout", &self.timeout)
            .field("remaining", &self.remaining())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_manager_new() {
        let timeout = Duration::from_secs(5);
        let manager = TimeoutManager::new(timeout);

        assert_eq!(manager.timeout(), timeout);
        assert!(manager.elapsed() < Duration::from_millis(100)); // Should be very small
        assert!(manager.remaining() >= TimeoutManager::MINIMUM_REAP_TIME);
    }

    #[test]
    fn test_timeout_manager_remaining() {
        let timeout = Duration::from_millis(100);
        let manager = TimeoutManager::new(timeout);

        // Initially, remaining should be close to timeout but at least MINIMUM_REAP_TIME
        let remaining = manager.remaining();
        assert!(remaining >= TimeoutManager::MINIMUM_REAP_TIME);
        // Note: remaining might be greater than timeout due to MINIMUM_REAP_TIME

        // After sleeping, remaining should decrease (but still respect MINIMUM_REAP_TIME)
        std::thread::sleep(Duration::from_millis(10));
        let remaining_after_sleep = manager.remaining();
        assert!(remaining_after_sleep >= TimeoutManager::MINIMUM_REAP_TIME);
    }

    #[test]
    fn test_timeout_manager_elapsed() {
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);

        // Initially elapsed should be very small
        assert!(manager.elapsed() < Duration::from_millis(100));

        // After sleeping, elapsed should increase
        std::thread::sleep(Duration::from_millis(10));
        let elapsed = manager.elapsed();
        assert!(elapsed >= Duration::from_millis(10));

        #[cfg(not(miri))] // miri allows the clock to go arbitrarily fast
        assert!(elapsed < Duration::from_millis(100)); // Should be reasonable
    }

    #[test]
    fn test_timeout_manager_minimum_reap_time() {
        let timeout = Duration::from_millis(50); // Less than MINIMUM_REAP_TIME
        let manager = TimeoutManager::new(timeout);

        // Even with a small timeout, remaining should be at least MINIMUM_REAP_TIME
        assert_eq!(manager.remaining(), TimeoutManager::MINIMUM_REAP_TIME);
    }

    #[test]
    fn test_timeout_manager_debug() {
        let timeout = Duration::from_secs(1);
        let manager = TimeoutManager::new(timeout);

        let debug_str = format!("{:?}", manager);

        // Debug output should contain the expected fields
        assert!(debug_str.contains("TimeoutManager"));
        assert!(debug_str.contains("start_time"));
        assert!(debug_str.contains("elapsed"));
        assert!(debug_str.contains("timeout"));
        assert!(debug_str.contains("remaining"));
    }

    #[test]
    fn test_timeout_manager_timeout_exceeded() {
        let timeout = Duration::from_millis(10);
        let manager = TimeoutManager::new(timeout);

        // Sleep longer than the timeout
        std::thread::sleep(Duration::from_millis(50));

        // Elapsed should be greater than timeout
        assert!(manager.elapsed() > timeout);

        // Remaining should still be at least MINIMUM_REAP_TIME (not overflow)
        let remaining = manager.remaining();
        assert_eq!(remaining, TimeoutManager::MINIMUM_REAP_TIME);
    }
}
