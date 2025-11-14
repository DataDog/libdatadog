// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::parallel::slice_set::{ParallelSliceSet, ParallelSliceStorage};
use crate::profiles::collections::{Arc, ArcOverflow, SetError, StringRef, WELL_KNOWN_STRING_REFS};
use core::ptr;
use std::ffi::c_void;
use std::ops::Deref;

/// A string set which can have parallel read and write operations.
/// This is a newtype wrapper around ParallelSliceSet<u8> that adds
/// string-specific functionality like well-known strings.
#[repr(transparent)]
pub struct ParallelStringSet {
    pub(crate) inner: ParallelSliceSet<u8>,
}

impl Deref for ParallelStringSet {
    type Target = ParallelSliceSet<u8>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl ParallelStringSet {
    /// Consumes the `ParallelStringSet`, returning a non-null pointer to the
    /// inner storage. This storage should not be mutated--it only exists to
    /// be passed across FFI boundaries, which is why its type has been erased.
    #[inline]
    pub fn into_raw(self) -> ptr::NonNull<c_void> {
        Arc::into_raw(self.inner.arc).cast()
    }

    /// Recreates a `ParallelStringSet` from a raw pointer produced by
    /// [`ParallelStringSet::into_raw`].
    ///
    /// # Safety
    ///
    /// The pointer must have been produced by [`ParallelStringSet::into_raw`]
    /// and be returned unchanged.
    #[inline]
    pub unsafe fn from_raw(raw: ptr::NonNull<c_void>) -> Self {
        let arc = Arc::from_raw(raw.cast::<ParallelSliceStorage<u8>>());
        Self {
            inner: ParallelSliceSet { arc },
        }
    }

    pub fn try_clone(&self) -> Result<ParallelStringSet, ArcOverflow> {
        Ok(ParallelStringSet {
            inner: self.inner.try_clone()?,
        })
    }

    /// Tries to create a new parallel string set that contains the well-known
    /// strings, including the empty string.
    pub fn try_new() -> Result<Self, SetError> {
        let inner = ParallelSliceSet::try_new()?;
        let set = Self { inner };

        for id in WELL_KNOWN_STRING_REFS.iter() {
            // SAFETY: the well-known strings are unique, and we're in the
            // constructor where other threads don't have access to it yet.
            _ = unsafe { set.insert_unique_uncontended(id.0.deref())? };
        }
        Ok(set)
    }

    /// # Safety
    /// The string must not have been inserted yet, as it skips checking if
    /// the string is already present.
    pub unsafe fn insert_unique_uncontended(&self, str: &str) -> Result<StringRef, SetError> {
        let thin_slice = self.inner.insert_unique_uncontended(str.as_bytes())?;
        Ok(StringRef(thin_slice.into()))
    }

    /// Adds the string to the string set if it isn't present already, and
    /// returns a handle to the string that can be used to retrieve it later.
    pub fn try_insert(&self, str: &str) -> Result<StringRef, SetError> {
        let thin_slice = self.inner.try_insert(str.as_bytes())?;
        Ok(StringRef(thin_slice.into()))
    }

    /// Selects which shard a hash should go to (0-3 for 4 shards).
    pub fn select_shard(hash: u64) -> usize {
        ParallelSliceSet::<u8>::select_shard(hash)
    }

    /// # Safety
    /// The caller must ensure that the StringId is valid for this set.
    pub unsafe fn get(&self, id: StringRef) -> &str {
        // SAFETY: safe as long as caller respects this function's safety.
        unsafe { core::mem::transmute::<&str, &str>(id.0.deref()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::collections::parallel::slice_set::N_SHARDS;
    use crate::profiles::collections::SetHasher as Hasher;
    use std::hash::BuildHasher;

    #[test]
    fn test_well_known_strings() {
        let strs: [&str; WELL_KNOWN_STRING_REFS.len()] = [
            "",
            "end_timestamp_ns",
            "local root span id",
            "trace endpoint",
            "span id",
        ];
        for (expected, id) in strs.iter().copied().zip(WELL_KNOWN_STRING_REFS) {
            let actual: &str = id.0.deref();
            assert_eq!(expected, actual);
        }

        let mut selected = [0; WELL_KNOWN_STRING_REFS.len()];
        for (id, dst) in WELL_KNOWN_STRING_REFS.iter().zip(selected.iter_mut()) {
            *dst = ParallelStringSet::select_shard(Hasher::default().hash_one(id.0.deref()));
        }
    }

    #[test]
    fn test_parallel_set() {
        let set = ParallelStringSet::try_new().unwrap();
        // SAFETY: these are all well-known strings.
        unsafe {
            let str = set.get(StringRef::EMPTY);
            assert_eq!(str, "");

            let str = set.get(StringRef::END_TIMESTAMP_NS);
            assert_eq!(str, "end_timestamp_ns");

            let str = set.get(StringRef::LOCAL_ROOT_SPAN_ID);
            assert_eq!(str, "local root span id");

            let str = set.get(StringRef::TRACE_ENDPOINT);
            assert_eq!(str, "trace endpoint");

            let str = set.get(StringRef::SPAN_ID);
            assert_eq!(str, "span id");
        };

        let id = set.try_insert("").unwrap();
        assert_eq!(&*id.0, &*StringRef::EMPTY.0);

        let id = set.try_insert("end_timestamp_ns").unwrap();
        assert_eq!(&*id.0, &*StringRef::END_TIMESTAMP_NS.0);

        let id = set.try_insert("local root span id").unwrap();
        assert_eq!(&*id.0, &*StringRef::LOCAL_ROOT_SPAN_ID.0);

        let id = set.try_insert("trace endpoint").unwrap();
        assert_eq!(&*id.0, &*StringRef::TRACE_ENDPOINT.0);

        let id = set.try_insert("span id").unwrap();
        assert_eq!(&*id.0, &*StringRef::SPAN_ID.0);
    }

    #[test]
    fn test_hash_distribution() {
        let test_strings: Vec<String> = (0..100).map(|i| format!("test_string_{}", i)).collect();

        let mut shard_counts = [0; N_SHARDS];

        for s in &test_strings {
            let hash = Hasher::default().hash_one(s);
            let shard = ParallelStringSet::select_shard(hash);
            shard_counts[shard] += 1;
        }

        // Verify that distribution is not completely degenerate
        // (both shards should get at least some strings)
        assert!(shard_counts[0] > 0, "Shard 0 got no strings");
        assert!(shard_counts[1] > 0, "Shard 1 got no strings");

        // Print distribution for manual inspection
        println!("Shard distribution: {:?}", shard_counts);
    }

    #[test]
    fn test_parallel_set_shard_selection() {
        let set = ParallelStringSet::try_new().unwrap();

        // Test with realistic strings that would appear in profiling
        let test_strings = [
            // .NET method signatures
            "System.String.Concat(System.Object)",
            "Microsoft.Extensions.DependencyInjection.ServiceProvider.GetService(System.Type)",
            "System.Text.Json.JsonSerializer.Deserialize<T>(System.String)",
            "MyNamespace.MyClass.MyMethod(Int32 id, String name)",
            // File paths and URLs
            "/usr/lib/x86_64-linux-gnu/libc.so.6",
            "/var/run/datadog/apm.socket",
            "https://api.datadoghq.com/api/v1/traces",
            "/home/user/.local/share/applications/myapp.desktop",
            "C:\\Program Files\\MyApp\\bin\\myapp.exe",
            // Short common strings
            "f",
            "g",
        ];

        let mut ids = Vec::new();
        for &test_str in &test_strings {
            let id = set.try_insert(test_str).unwrap();
            ids.push((test_str, id));
        }

        // Verify all strings can be retrieved correctly
        for (original_str, id) in ids {
            unsafe {
                assert_eq!(set.get(id), original_str);
            }
        }

        // Test that inserting the same strings again returns the same IDs
        for &test_str in &test_strings {
            let id1 = set.try_insert(test_str).unwrap();
            let id2 = set.try_insert(test_str).unwrap();
            assert_eq!(&*id1.0, &*id2.0);
        }
    }

    #[test]
    fn auto_traits_send_sync() {
        fn require_send<T: Send>() {}
        fn require_sync<T: Sync>() {}
        require_send::<super::ParallelStringSet>();
        require_sync::<super::ParallelStringSet>();
    }

    #[test]
    fn test_thread_safety() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        // Create set and add some strings before sharing across threads
        let set = ParallelStringSet::try_new().unwrap();
        let pre_inserted_strings = vec![
            "pre_inserted_1",
            "pre_inserted_2",
            "pre_inserted_3",
            "pre_inserted_4",
        ];
        let mut pre_inserted_ids = Vec::new();
        for s in &pre_inserted_strings {
            let id = set.try_insert(s).unwrap();
            pre_inserted_ids.push((s.to_string(), id));
        }

        // Share the set across threads using try_clone (which clones the internal Arc)
        let num_threads = 4;
        let operations_per_thread = 50;

        // Keep a clone of the original set for final verification
        let original_set = set.try_clone().unwrap();

        // Create a barrier to ensure all threads start work simultaneously
        let barrier = Arc::new(Barrier::new(num_threads));

        // Spawn threads that will both read pre-existing strings and insert new ones
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let set = original_set.try_clone().unwrap();
                let pre_ids = pre_inserted_ids.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    // Wait for all threads to be spawned before starting work
                    barrier.wait();

                    // Read pre-existing strings (should be safe to read concurrently)
                    for (expected_str, id) in &pre_ids {
                        unsafe {
                            let actual_str = set.get(*id);
                            assert_eq!(
                                actual_str,
                                expected_str.as_str(),
                                "Pre-inserted string should be readable"
                            );
                        }
                    }

                    // Concurrently insert new strings
                    for i in 0..operations_per_thread {
                        let new_str = format!("thread_{}_string_{}", thread_id, i);
                        let id = set.try_insert(&new_str).unwrap();
                        unsafe {
                            let retrieved = set.get(id);
                            assert_eq!(retrieved, new_str, "Inserted string should be retrievable");
                        }
                    }

                    // Try inserting strings that other threads might have inserted
                    for i in 0..operations_per_thread {
                        let shared_str = format!("shared_string_{}", i);
                        let id1 = set.try_insert(&shared_str).unwrap();
                        let id2 = set.try_insert(&shared_str).unwrap();
                        // Both should return the same ID (deduplication)
                        assert_eq!(&*id1.0, &*id2.0, "Duplicate inserts should return same ID");
                        unsafe {
                            assert_eq!(set.get(id1), shared_str);
                        }
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread should not panic");
        }

        // Verify final state: all pre-inserted strings should still be readable
        for (expected_str, id) in &pre_inserted_ids {
            unsafe {
                let actual_str = original_set.get(*id);
                assert_eq!(
                    actual_str,
                    expected_str.as_str(),
                    "Pre-inserted strings should remain readable after concurrent operations"
                );
            }
        }

        // Verify that shared strings inserted by multiple threads are deduplicated
        for i in 0..operations_per_thread {
            let shared_str = format!("shared_string_{}", i);
            let id1 = original_set.try_insert(&shared_str).unwrap();
            let id2 = original_set.try_insert(&shared_str).unwrap();
            assert_eq!(
                &*id1.0, &*id2.0,
                "Shared strings should be deduplicated correctly"
            );
        }
    }
}
