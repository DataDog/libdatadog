// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pool for reusing `OwnedSample` instances to reduce allocation overhead.

use super::{Metadata, OwnedSample};
use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

/// A bounded pool of `OwnedSample` instances for efficient reuse.
///
/// The pool maintains a limited number of samples that can be reused
/// across multiple profiling operations. When a sample is requested,
/// it's either taken from the pool or freshly allocated. When returned,
/// it's reset and added back to the pool if there's space, otherwise dropped.
///
/// This pool is **thread-safe** and uses a lock-free `ArrayQueue` internally,
/// allowing concurrent access from multiple threads without locks.
///
/// # Example
/// ```no_run
/// # use libdd_profiling::owned_sample::{SamplePool, Metadata, SampleType};
/// # use std::sync::Arc;
/// let metadata = Arc::new(Metadata::new(vec![
///     SampleType::CpuTime,
///     SampleType::WallTime,
/// ], 64, true).unwrap());
///
/// let pool = SamplePool::new(metadata, 10);
///
/// // Get a sample from the pool (thread-safe)
/// let mut sample = pool.get();
/// sample.set_value(SampleType::CpuTime, 100).unwrap();
/// // ... use sample ...
///
/// // Return it to the pool for reuse (thread-safe)
/// pool.put(sample);
/// ```
pub struct SamplePool {
    /// The sample type metadata configuration shared by all samples
    metadata: Arc<Metadata>,
    /// Lock-free bounded queue of available samples.
    /// Uses `ArrayQueue` for lock-free concurrent access via atomic operations,
    /// enabling efficient multi-threaded usage without mutex contention.
    samples: ArrayQueue<Box<OwnedSample>>,
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
    /// # use libdd_profiling::owned_sample::{SamplePool, Metadata, SampleType};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
    /// let pool = SamplePool::new(metadata, 100);
    /// ```
    pub fn new(metadata: Arc<Metadata>, capacity: usize) -> Self {
        Self {
            metadata,
            samples: ArrayQueue::new(capacity),
        }
    }

    /// Gets a sample from the pool, or allocates a new one if the pool is empty.
    ///
    /// The returned sample is guaranteed to be reset and ready to use.
    ///
    /// This method is **thread-safe** and can be called concurrently from multiple threads.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{SamplePool, Metadata, SampleType};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
    /// # let pool = SamplePool::new(metadata, 10);
    /// let sample = pool.get();
    /// assert_eq!(sample.num_locations(), 0);
    /// ```
    pub fn get(&self) -> Box<OwnedSample> {
        self.samples.pop().unwrap_or_else(|| {
            Box::new(OwnedSample::new(self.metadata.clone()))
        })
    }

    /// Returns a sample to the pool for reuse.
    ///
    /// The sample is reset before being added to the pool. If the pool is at capacity,
    /// the sample is dropped instead.
    ///
    /// This method is **thread-safe** and can be called concurrently from multiple threads.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{SamplePool, Metadata, SampleType};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
    /// # let pool = SamplePool::new(metadata, 10);
    /// let mut sample = pool.get();
    /// sample.set_value(SampleType::CpuTime, 100).unwrap();
    /// pool.put(sample);  // Resets and returns to pool
    /// ```
    pub fn put(&self, mut sample: Box<OwnedSample>) {
        // Reset the sample to clean state
            sample.reset();
        
        // Try to add back to pool (lock-free operation)
        // If full, push() returns Err(sample), which we just drop
        let _ = self.samples.push(sample);
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
        self.samples.capacity()
    }
}

// SAFETY: SamplePool uses ArrayQueue which is Send + Sync, and Arc<Metadata> which is also Send + Sync
unsafe impl Send for SamplePool {}
unsafe impl Sync for SamplePool {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owned_sample::SampleType;

    #[test]
    fn test_pool_basic() {
        let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
        let pool = SamplePool::new(metadata, 5);

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
        let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
        let pool = SamplePool::new(metadata, 2);

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
        let metadata = Arc::new(Metadata::new(vec![
            SampleType::CpuTime,
            SampleType::WallTime,
        ], 64, true).unwrap());
        let pool = SamplePool::new(metadata, 5);

        // Get a sample and modify it
        let mut sample = pool.get();
        sample.set_value(SampleType::CpuTime, 100).unwrap();
        sample.set_value(SampleType::WallTime, 200).unwrap();

        // Return it to pool
        pool.put(sample);

        // Get it back - should be reset
        let sample = pool.get();
        assert_eq!(sample.get_value(SampleType::CpuTime).unwrap(), 0);
        assert_eq!(sample.get_value(SampleType::WallTime).unwrap(), 0);
        assert_eq!(sample.num_locations(), 0);
        assert_eq!(sample.num_labels(), 0);
    }

    #[test]
    fn test_pool_thread_safety() {
        use std::thread;

        let metadata = Arc::new(Metadata::new(vec![
            SampleType::CpuTime,
            SampleType::WallTime,
        ], 64, true).unwrap());
        let pool = Arc::new(SamplePool::new(metadata, 20));

        // Spawn multiple threads that all use the pool concurrently
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for i in 0..100 {
                        // Get a sample from the pool
                        let mut sample = pool.get();
                        
                        // Use it
                        sample.set_value(SampleType::CpuTime, (thread_id * 1000 + i) as i64).unwrap();
                        sample.set_value(SampleType::WallTime, (thread_id * 2000 + i) as i64).unwrap();
                        
                        // Return it to the pool
                        pool.put(sample);
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Pool should have accumulated samples (up to its capacity)
        assert!(pool.len() <= pool.capacity());
        assert!(pool.len() > 0); // Should have at least some samples
    }
}

