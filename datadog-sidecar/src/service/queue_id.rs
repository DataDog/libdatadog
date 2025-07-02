// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use rand::Rng;
use serde::{Deserialize, Serialize};

/// `QueueId` is a struct that represents a unique identifier for a queue.
/// It contains a single field, `inner`, which is a 64-bit unsigned integer.
#[derive(Default, Copy, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct QueueId {
    pub(crate) inner: u64,
}

impl QueueId {
    /// Generates a new unique `QueueId`.
    ///
    /// This method generates a random 64-bit unsigned integer between 1 (inclusive) and `u64::MAX`
    /// (exclusive) and uses it as the `inner` value of the new `QueueId`.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::QueueId;
    ///
    /// let queue_id = QueueId::new_unique();
    /// ```
    pub fn new_unique() -> Self {
        Self {
            inner: rand::thread_rng().gen_range(1u64..u64::MAX),
        }
    }
}

impl From<u64> for QueueId {
    fn from(value: u64) -> Self {
        QueueId { inner: value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_unique() {
        let queue_id1 = QueueId::new_unique();
        let queue_id2 = QueueId::new_unique();

        assert_ne!(queue_id1, queue_id2, "Generated QueueIds should be unique");

        // Check that the generated QueueId is within the defined range bounds
        assert!(
            queue_id1.inner >= 1 && queue_id1.inner < u64::MAX,
            "Generated QueueId should be within the defined range bounds"
        );
        assert!(
            queue_id2.inner >= 1 && queue_id2.inner < u64::MAX,
            "Generated QueueId should be within the defined range bounds"
        );
    }
}
