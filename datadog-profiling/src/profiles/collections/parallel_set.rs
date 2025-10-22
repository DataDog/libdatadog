// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::set::{Set, SetId, SET_MIN_CAPACITY};
use super::{Arc, SetError, SetOps};
use super::{SetHasher as Hasher, Sharded};
use core::any::TypeId;
use core::hash;
use datadog_alloc::Global;
use std::ffi::c_void;
use std::hash::BuildHasher;
use std::ptr;

#[derive(Debug)]
#[repr(C)]
pub struct ParallelSet<T: hash::Hash + Eq + 'static, const N: usize> {
    pub(crate) storage: Arc<Sharded<Set<T>, N>>,
}

impl<T: hash::Hash + Eq + 'static, const N: usize> ParallelSet<T, N> {
    const fn is_power_of_two_gt1() -> bool {
        N.is_power_of_two() && N > 1
    }

    pub fn try_new() -> Result<Self, SetError> {
        if !Self::is_power_of_two_gt1() {
            return Err(SetError::InvalidArgument);
        }
        let storage = Sharded::<Set<T>, N>::try_new_with_min_capacity(SET_MIN_CAPACITY)?;
        let storage = Arc::try_new(storage)?;
        Ok(Self { storage })
    }

    #[inline]
    fn storage(&self) -> &Sharded<Set<T>, N> {
        &self.storage
    }

    pub fn try_clone(&self) -> Result<Self, SetError> {
        let storage = self
            .storage
            .try_clone()
            .map_err(|_| SetError::ReferenceCountOverflow)?;
        Ok(Self { storage })
    }

    #[inline]
    fn select_shard(hash: u64) -> usize {
        (hash as usize) & (N - 1)
    }

    pub fn try_insert(&self, value: T) -> Result<SetId<T>, SetError> {
        let hash = Hasher::default().hash_one(&value);
        let idx = Self::select_shard(hash);
        let lock = &self.storage().shards[idx];

        let read_len = {
            let guard = lock.read();
            // SAFETY: `hash` was computed using this set's hasher over `&value`.
            if let Some(id) = unsafe { guard.find_with_hash(hash, &value) } {
                return Ok(id);
            }
            guard.len()
        };

        let mut guard = lock.write();
        if guard.len() != read_len {
            // SAFETY: `hash` was computed using this set's hasher over `&value`.
            if let Some(id) = unsafe { guard.find_with_hash(hash, &value) } {
                return Ok(id);
            }
        }

        // SAFETY: `hash` was computed using this set's hasher over `&value`,
        // and uniqueness has been enforced by the preceding read/write checks.
        unsafe { guard.insert_unique_uncontended_with_hash(hash, value) }
            .map_err(|_| SetError::OutOfMemory)
    }

    /// Returns the `SetId` for `value` if it exists in the parallel set, without inserting.
    /// Intended for tests and debugging; typical usage should prefer `try_insert`.
    pub fn find(&self, value: &T) -> Option<SetId<T>> {
        let hash = Hasher::default().hash_one(value);
        let idx = Self::select_shard(hash);
        let lock = &self.storage().shards[idx];
        let guard = lock.read();
        // SAFETY: `hash` computed using this set's hasher over `&value`.
        unsafe { guard.find_with_hash(hash, value) }
    }

    #[inline]
    pub fn element_type_id(&self) -> TypeId {
        self.storage.type_id
    }

    /// Returns a shared reference to the value for a given `SetId`.
    ///
    /// # Safety
    /// - The `id` must have been obtained from this exact `ParallelSet` (and shard) instance, and
    ///   thus point to a live `T` stored in its arena.
    /// # Safety
    /// - `id` must come from this exact `ParallelSet` instance (same shard) and still refer to a
    ///   live element in its arena.
    /// - The returned reference is immutable; do not concurrently mutate the same element via
    ///   interior mutability.
    pub unsafe fn get(&self, id: SetId<T>) -> &T {
        // We do not need to lock to read the value; storage is arena-backed and
        // values are immutable once inserted. Caller guarantees `id` belongs here.
        unsafe { id.0.as_ref() }
    }

    pub fn into_raw(self) -> ptr::NonNull<c_void> {
        Arc::into_raw(self.storage).cast()
    }

    /// # Safety
    /// - `this` must be produced by `into_raw` for a `ParallelSet<T, N>` with matching `T`, `N`,
    ///   and allocator.
    /// - After calling, do not use the original raw pointer again.
    pub unsafe fn from_raw(this: ptr::NonNull<c_void>) -> Self {
        let storage = unsafe { Arc::from_raw_in(this.cast(), Global) };
        Self { storage }
    }
}

// SAFETY: uses `RwLock<Set<T>>` to synchronize access. All reads/writes in
// this wrapper go through those locks. All non-mut methods of
// `ParallelSetStorage` and `Set` are safe to call under a read-lock, and all
// mut methods are safe to call under a write-lock.
unsafe impl<T: hash::Hash + Eq + 'static, const N: usize> Send for ParallelSet<T, N> {}
unsafe impl<T: hash::Hash + Eq + 'static, const N: usize> Sync for ParallelSet<T, N> {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet as StdHashSet;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: if cfg!(miri) { 4 } else { 64 },
            .. ProptestConfig::default()
        })]

        #[test]
        fn proptest_parallel_set_matches_std_hashset(
            values in proptest::collection::vec(any::<u64>(), 0..if cfg!(miri) { 32 } else { 512 })
        ) {
            type PSet = ParallelSet<u64, 4>;
            let set = PSet::try_new().unwrap();
            let mut shadow = StdHashSet::<u64>::new();

            for v in &values {
                shadow.insert(*v);
                let _ = set.try_insert(*v).unwrap();
            }

            // Compare lengths
            let len_pset = {
                let s = set.storage();
                let mut acc = 0usize;
                for shard in &s.shards { acc += shard.read().len(); }
                acc
            };
            prop_assert_eq!(len_pset, shadow.len());

            // Each shadow value must be present and equal
            for &v in &shadow {
                let id = set.find(&v);
                prop_assert!(id.is_some());
                let id = id.unwrap();
                // SAFETY: id just obtained from this set
                let fetched = unsafe { set.get(id) };
                prop_assert_eq!(*fetched, v);
            }
        }
    }

    #[test]
    fn auto_traits_send_sync() {
        fn require_send<T: Send>() {}
        fn require_sync<T: Sync>() {}
        type PSet = super::ParallelSet<u64, 4>;
        require_send::<PSet>();
        require_sync::<PSet>();
    }
}
