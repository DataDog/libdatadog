// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::slice_set::SliceSet;
use super::{Arc, ArcOverflow, SetError, ThinSlice};
use std::hash::{self, BuildHasher};
use std::ops::Deref;

use super::SetHasher as Hasher;

/// Number of shards used by the parallel slice set and (by extension)
/// the string-specific parallel set. Kept as a constant so tests and
/// related code can refer to the same value.
pub const N_SHARDS: usize = 16;

/// The initial capacities for Rust's hash map (and set) currently go
/// like this: 3, 7, 14, 28. We want to avoid some of the smaller sizes so
/// that there's less frequent re-allocation, which is the most expensive
/// part of the set's operations.
const HASH_TABLE_MIN_CAPACITY: usize = 28;

pub type ParallelSliceStorage<T> = super::Sharded<SliceSet<T>, 16>;

/// A slice set which can have parallel read and write operations. It works
/// by sharding the set and using read-write locks local to each shard. Items
/// cannot be removed from the set; the implementation relies on this for a
/// variety of optimizations.
///
/// This is a fairly naive implementation. Unfortunately, dashmap and other
/// off-the-shelf implementations I looked at don't have adequate APIs for
/// avoiding panics, including handling allocation failures. Since we're
/// rolling our own, we can get some benefits like having wait-free lookups
/// when fetching the value associated to an ID.
///
/// Also unfortunately, even parking_lot's RwLock doesn't handle allocation
/// failures. But I'm not going to go _that_ far to avoid allocation failures
/// today. We're very unlikely to run out of memory while adding a waiter to
/// its queue, because the amount of memory used is bounded by the number of
/// threads, which is small.
#[repr(transparent)]
pub struct ParallelSliceSet<T: Copy + hash::Hash + Eq + 'static> {
    pub(crate) arc: Arc<ParallelSliceStorage<T>>,
}

// SAFETY: uses `RwLock<SliceSet<T>>` to synchronize access. All reads/writes
// in this wrapper go through those locks. All non-mut methods of
// `ParallelSliceStorage` and `Set` are safe to call under a read-lock, and all
// mut methods are safe to call under a write-lock.
unsafe impl<T: Copy + hash::Hash + Eq + 'static> Send for ParallelSliceSet<T> {}
unsafe impl<T: Copy + hash::Hash + Eq + 'static> Sync for ParallelSliceSet<T> {}

impl<T: Copy + hash::Hash + Eq + 'static> Deref for ParallelSliceSet<T> {
    type Target = ParallelSliceStorage<T>;
    fn deref(&self) -> &Self::Target {
        &self.arc
    }
}

impl<T: Copy + hash::Hash + Eq + 'static> ParallelSliceSet<T> {
    pub fn try_clone(&self) -> Result<ParallelSliceSet<T>, ArcOverflow> {
        let ptr = self.arc.try_clone().map_err(|_| ArcOverflow)?;
        Ok(ParallelSliceSet { arc: ptr })
    }

    pub const fn select_shard(hash: u64) -> usize {
        // Use lower bits for shard selection to avoid interfering with
        // Swiss tables' internal SIMD comparisons that use upper 7 bits.
        // Using 4 bits provides resilience against hash function deficiencies
        // and optimal scaling for low thread counts.
        (hash & 0b1111) as usize
    }

    /// Tries to create a new parallel slice set.
    pub fn try_new() -> Result<Self, SetError> {
        let storage = ParallelSliceStorage::try_new_with_min_capacity(HASH_TABLE_MIN_CAPACITY)?;
        let ptr = Arc::try_new(storage)?;
        Ok(Self { arc: ptr })
    }

    /// # Safety
    /// The slice must not have been inserted yet, as it skips checking if
    /// the slice is already present.
    pub unsafe fn insert_unique_uncontended(
        &self,
        slice: &[T],
    ) -> Result<ThinSlice<'static, T>, SetError>
    where
        T: hash::Hash,
    {
        let hash = Hasher::default().hash_one(slice);
        let shard_idx = Self::select_shard(hash);
        let lock = &self.shards[shard_idx];
        let mut guard = lock.write();
        guard.insert_unique_uncontended(slice)
    }

    /// Adds the slice to the slice set if it isn't present already, and
    /// returns a handle to the slice that can be used to retrieve it later.
    pub fn try_insert(&self, slice: &[T]) -> Result<ThinSlice<'static, T>, SetError>
    where
        T: hash::Hash + PartialEq,
    {
        // Hash once and reuse it for all operations.
        // Do this without holding any locks.
        let hash = Hasher::default().hash_one(slice);
        let shard_idx = Self::select_shard(hash);
        let lock = &self.shards[shard_idx];

        let read_len = {
            let guard = lock.read();
            // SAFETY: the slice's hash is correct, we use the same hasher as
            // SliceSet uses.
            if let Some(id) = unsafe { guard.deref().find_with_hash(hash, slice) } {
                return Ok(id);
            }
            guard.len()
        };

        let mut write_guard = lock.write();
        let write_len = write_guard.slices.len();
        // This is an ABA defense. It's simple because we only support insert.
        if write_len != read_len {
            // SAFETY: the slice's hash is correct, we use the same hasher as
            // SliceSet uses.
            if let Some(id) = unsafe { write_guard.find_with_hash(hash, slice) } {
                return Ok(id);
            }
        }

        // SAFETY: we just checked above that the slice isn't in the set.
        let id = unsafe { write_guard.insert_unique_uncontended_with_hash(hash, slice)? };
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use crate::profiles::collections::string_set::{StringId, UnsyncStringSet};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// A test struct representing a function with file and function names.
    /// This tests that the generic slice infrastructure works with composite types.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct Function {
        file_name: StringId,
        function_name: StringId,
    }

    impl Function {
        fn new(file_name: StringId, function_name: StringId) -> Self {
            Self {
                file_name,
                function_name,
            }
        }
    }

    #[test]
    fn test_function_deduplication() {
        // Create string set for the string data
        let mut string_set = UnsyncStringSet::try_new().unwrap();

        // Create some strings
        let file1 = string_set.try_insert("src/main.rs").unwrap();
        let file2 = string_set.try_insert("src/lib.rs").unwrap();
        let func1 = string_set.try_insert("main").unwrap();
        let func2 = string_set.try_insert("process_data").unwrap();

        // Create function objects
        let fn1 = Function::new(file1, func1); // main in src/main.rs
        let fn2 = Function::new(file1, func2); // process_data in src/main.rs
        let fn3 = Function::new(file2, func1); // main in src/lib.rs
        let fn4 = Function::new(file1, func1); // main in src/main.rs (duplicate of fn1)

        // Test that the functions are equal/different as expected
        assert_eq!(fn1, fn4, "Same function should be equal");
        assert_ne!(fn1, fn2, "Different functions should not be equal");
        assert_ne!(fn1, fn3, "Different functions should not be equal");
        assert_ne!(fn2, fn3, "Different functions should not be equal");

        // Test that we can distinguish them by their components
        unsafe {
            assert_eq!(string_set.get_string(fn1.file_name), "src/main.rs");
            assert_eq!(string_set.get_string(fn1.function_name), "main");
            assert_eq!(string_set.get_string(fn2.function_name), "process_data");
            assert_eq!(string_set.get_string(fn3.file_name), "src/lib.rs");
        }
    }

    #[test]
    fn auto_traits_send_sync() {
        fn require_send<T: Send>() {}
        fn require_sync<T: Sync>() {}
        require_send::<super::ParallelSliceSet<u8>>();
        require_sync::<super::ParallelSliceSet<u8>>();
    }

    #[test]
    fn test_function_hashing() {
        let mut string_set = UnsyncStringSet::try_new().unwrap();

        let file1 = string_set.try_insert("src/main.rs").unwrap();
        let func1 = string_set.try_insert("main").unwrap();
        let func2 = string_set.try_insert("process_data").unwrap();

        let fn1 = Function::new(file1, func1);
        let fn2 = Function::new(file1, func2);
        let fn1_copy = Function::new(file1, func1);

        // Test hash consistency
        let hash1 = {
            let mut hasher = DefaultHasher::new();
            fn1.hash(&mut hasher);
            hasher.finish()
        };

        let hash1_copy = {
            let mut hasher = DefaultHasher::new();
            fn1_copy.hash(&mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = DefaultHasher::new();
            fn2.hash(&mut hasher);
            hasher.finish()
        };

        // Same function should have same hash
        assert_eq!(hash1, hash1_copy, "Same function should hash consistently");

        // Different functions should have different hashes (with high probability)
        assert_ne!(
            hash1, hash2,
            "Different functions should have different hashes"
        );
    }

    #[test]
    fn test_function_composition() {
        let mut string_set = UnsyncStringSet::try_new().unwrap();

        let file1 = string_set.try_insert("src/utils.rs").unwrap();
        let func1 = string_set.try_insert("calculate_hash").unwrap();

        let function = Function::new(file1, func1);

        // Test that we can access the components
        assert_eq!(function.file_name, file1);
        assert_eq!(function.function_name, func1);

        // Test that the string data is preserved
        unsafe {
            assert_eq!(string_set.get_string(function.file_name), "src/utils.rs");
            assert_eq!(
                string_set.get_string(function.function_name),
                "calculate_hash"
            );
        }
    }

    #[test]
    fn test_many_functions() {
        let mut string_set = UnsyncStringSet::try_new().unwrap();

        // Create a variety of file and function names
        let files = [
            "src/main.rs",
            "src/lib.rs",
            "src/utils.rs",
            "src/parser.rs",
            "src/codegen.rs",
        ];
        let functions = [
            "main", "new", "process", "parse", "generate", "validate", "cleanup", "init",
        ];

        let mut file_ids = Vec::new();
        let mut func_ids = Vec::new();

        // Create string IDs
        for &file in &files {
            file_ids.push(string_set.try_insert(file).unwrap());
        }
        for &func in &functions {
            func_ids.push(string_set.try_insert(func).unwrap());
        }

        let mut functions_created = Vec::new();

        // Create many function combinations
        for &file_id in &file_ids {
            for &func_id in &func_ids {
                let function = Function::new(file_id, func_id);
                functions_created.push(function);
            }
        }

        // Should have files.len() * functions.len() unique functions
        assert_eq!(functions_created.len(), files.len() * functions.len());

        // Test that all functions are different (no duplicates)
        for i in 0..functions_created.len() {
            for j in i + 1..functions_created.len() {
                assert_ne!(
                    functions_created[i], functions_created[j],
                    "All function combinations should be different"
                );
            }
        }

        // Test that we can retrieve the original strings
        for function in &functions_created {
            unsafe {
                let file_str = string_set.get_string(function.file_name);
                let func_str = string_set.get_string(function.function_name);

                // Verify the strings are in our original arrays
                assert!(
                    files.contains(&file_str),
                    "File name should be from our test set"
                );
                assert!(
                    functions.contains(&func_str),
                    "Function name should be from our test set"
                );
            }
        }
    }

    #[test]
    fn test_function_edge_cases() {
        let mut string_set = UnsyncStringSet::try_new().unwrap();

        // Test with empty strings
        let empty_file = string_set.try_insert("").unwrap();
        let empty_func = string_set.try_insert("").unwrap();
        let normal_file = string_set.try_insert("normal.rs").unwrap();
        let normal_func = string_set.try_insert("normal_function").unwrap();

        let fn1 = Function::new(empty_file, empty_func); // Both empty
        let fn2 = Function::new(empty_file, normal_func); // Empty file, normal function
        let fn3 = Function::new(normal_file, empty_func); // Normal file, empty function
        let fn4 = Function::new(normal_file, normal_func); // Both normal

        // All should be different
        let functions = [fn1, fn2, fn3, fn4];
        for i in 0..functions.len() {
            for j in i + 1..functions.len() {
                assert_ne!(
                    functions[i], functions[j],
                    "Functions with different components should not be equal"
                );
            }
        }

        // Test that we can retrieve the correct strings
        unsafe {
            assert_eq!(string_set.get_string(fn1.file_name), "");
            assert_eq!(string_set.get_string(fn1.function_name), "");
            assert_eq!(string_set.get_string(fn2.file_name), "");
            assert_eq!(string_set.get_string(fn2.function_name), "normal_function");
            assert_eq!(string_set.get_string(fn3.file_name), "normal.rs");
            assert_eq!(string_set.get_string(fn3.function_name), "");
            assert_eq!(string_set.get_string(fn4.file_name), "normal.rs");
            assert_eq!(string_set.get_string(fn4.function_name), "normal_function");
        }
    }
}
