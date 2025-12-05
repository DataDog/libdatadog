// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pool for reusing `OwnedSample` instances to reduce allocation overhead.

use super::{OwnedSample, SampleTypeIndices};
use std::sync::Arc;

/// A bounded pool of `OwnedSample` instances for efficient reuse.
///
/// The pool maintains a limited number of samples that can be reused
/// across multiple profiling operations. When a sample is requested,
/// it's either taken from the pool or freshly allocated. When returned,
/// it's reset and added back to the pool if there's space, otherwise dropped.
///
/// # Example
/// ```no_run
/// # use libdd_profiling::owned_sample::{SamplePool, SampleTypeIndices, SampleType};
/// # use std::sync::Arc;
/// let indices = Arc::new(SampleTypeIndices::new(vec![
///     SampleType::Cpu,
///     SampleType::Wall,
/// ]).unwrap());
///
/// let mut pool = SamplePool::new(indices, 10);
///
/// // Get a sample from the pool
/// let mut sample = pool.get();
/// sample.set_value(SampleType::Cpu, 100).unwrap();
/// // ... use sample ...
///
/// // Return it to the pool for reuse
/// pool.put(sample);
/// ```
pub struct SamplePool {
    /// The sample type indices configuration shared by all samples
    indices: Arc<SampleTypeIndices>,
    /// Maximum number of samples to keep in the pool
    capacity: usize,
    /// Stack of available samples
    #[allow(clippy::vec_box)]
    samples: Vec<Box<OwnedSample>>,
}

impl SamplePool {
    /// Creates a new sample pool with the given capacity.
    ///
    /// # Arguments
    /// * `indices` - The sample type indices configuration to use for all samples
    /// * `capacity` - Maximum number of samples to keep in the pool
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{SamplePool, SampleTypeIndices, SampleType};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
    /// let pool = SamplePool::new(indices, 100);
    /// ```
    pub fn new(indices: Arc<SampleTypeIndices>, capacity: usize) -> Self {
        Self {
            indices,
            capacity,
            samples: Vec::with_capacity(capacity),
        }
    }

    /// Gets a sample from the pool, or allocates a new one if the pool is empty.
    ///
    /// The returned sample is guaranteed to be reset and ready to use.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{SamplePool, SampleTypeIndices, SampleType};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
    /// # let mut pool = SamplePool::new(indices, 10);
    /// let sample = pool.get();
    /// assert_eq!(sample.num_locations(), 0);
    /// ```
    pub fn get(&mut self) -> Box<OwnedSample> {
        self.samples.pop().unwrap_or_else(|| {
            Box::new(OwnedSample::new(self.indices.clone()))
        })
    }

    /// Returns a sample to the pool for reuse.
    ///
    /// The sample is reset before being added to the pool. If the pool is at capacity,
    /// the sample is dropped instead.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{SamplePool, SampleTypeIndices, SampleType};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
    /// # let mut pool = SamplePool::new(indices, 10);
    /// let mut sample = pool.get();
    /// sample.set_value(SampleType::Cpu, 100).unwrap();
    /// pool.put(sample);  // Resets and returns to pool
    /// ```
    pub fn put(&mut self, mut sample: Box<OwnedSample>) {
        if self.samples.len() < self.capacity {
            sample.reset();
            self.samples.push(sample);
        }
        // Otherwise, sample is dropped when it goes out of scope
    }

    /// Returns the current number of samples in the pool.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Returns true if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Returns the maximum capacity of the pool.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owned_sample::SampleType;

    #[test]
    fn test_pool_basic() {
        let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
        let mut pool = SamplePool::new(indices, 5);

        assert_eq!(pool.len(), 0);
        assert!(pool.is_empty());
        assert_eq!(pool.capacity(), 5);

        // Get a sample - should allocate new
        let sample = pool.get();
        assert_eq!(pool.len(), 0);

        // Return it
        pool.put(sample);
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_empty());

        // Get it back - should reuse
        let sample = pool.get();
        assert_eq!(pool.len(), 0);

        pool.put(sample);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn test_pool_capacity_limit() {
        let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
        let mut pool = SamplePool::new(indices, 2);

        // Fill the pool
        let sample1 = pool.get();
        let sample2 = pool.get();
        let sample3 = pool.get();

        pool.put(sample1);
        pool.put(sample2);
        assert_eq!(pool.len(), 2);

        // This one should be dropped since pool is at capacity
        pool.put(sample3);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn test_pool_reset() {
        let indices = Arc::new(SampleTypeIndices::new(vec![
            SampleType::Cpu,
            SampleType::Wall,
        ]).unwrap());
        let mut pool = SamplePool::new(indices, 5);

        // Get a sample and modify it
        let mut sample = pool.get();
        sample.set_value(SampleType::Cpu, 100).unwrap();
        sample.set_value(SampleType::Wall, 200).unwrap();

        // Return it to pool
        pool.put(sample);

        // Get it back - should be reset
        let sample = pool.get();
        assert_eq!(sample.get_value(SampleType::Cpu).unwrap(), 0);
        assert_eq!(sample.get_value(SampleType::Wall).unwrap(), 0);
        assert_eq!(sample.num_locations(), 0);
        assert_eq!(sample.num_labels(), 0);
    }
}

