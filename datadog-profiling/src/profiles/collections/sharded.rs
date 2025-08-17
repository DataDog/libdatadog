// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::hash::Hash;
use core::mem::MaybeUninit;
use crossbeam_utils::CachePadded;
use parking_lot::RwLock;

use super::{SetError, SetHasher as Hasher};

/// Operations a set must provide for so that a sharded set can be built on
/// top of it.
///
/// # Safety
///
/// Implementors must ensure that all methods which take `&self` are safe to
/// call under a read-lock, and all `&mut self` methods are safe to call under
/// a write-lock, and are safe for `Send` and `Sync`.
pub unsafe trait SetOps {
    type Lookup<'a>: Copy
    where
        Self: 'a;

    /// Owned payload used for insertion. For some containers (e.g. slice-backed
    /// sets) this can be a borrowed view like `&'a [T]` because the container
    /// copies data into its own arena during insertion.
    type Owned<'a>
    where
        Self: 'a;

    type Id: Copy;

    fn try_with_capacity(capacity: usize) -> Result<Self, SetError>
    where
        Self: Sized;

    fn len(&self) -> usize;

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// # Safety
    /// Same safety contract as the underlying container's find_with_hash.
    unsafe fn find_with_hash(&self, hash: u64, key: Self::Lookup<'_>) -> Option<Self::Id>;

    /// # Safety
    /// Same safety contract as the underlying container's insert_unique_uncontended_with_hash.
    unsafe fn insert_unique_uncontended_with_hash(
        &mut self,
        hash: u64,
        key: Self::Owned<'_>,
    ) -> Result<Self::Id, SetError>;
}

#[derive(Debug)]
pub struct Sharded<I: SetOps, const N: usize> {
    pub(crate) shards: [CachePadded<RwLock<I>>; N],
}

impl<I: SetOps, const N: usize> Sharded<I, N> {
    #[inline]
    pub const fn is_power_of_two_gt1() -> bool {
        N.is_power_of_two() && N > 1
    }

    #[inline]
    pub fn select_shard(hash: u64) -> usize {
        (hash as usize) & (N - 1)
    }

    pub fn try_new_with_min_capacity(min_capacity: usize) -> Result<Self, SetError> {
        let mut shards_uninit: [MaybeUninit<CachePadded<RwLock<I>>>; N] =
            unsafe { MaybeUninit::uninit().assume_init() };
        let mut i = 0usize;
        while i < N {
            match I::try_with_capacity(min_capacity) {
                Ok(inner) => {
                    shards_uninit[i].write(CachePadded::new(RwLock::new(inner)));
                    i += 1;
                }
                Err(e) => {
                    for j in (0..i).rev() {
                        unsafe { shards_uninit[j].assume_init_drop() };
                    }
                    return Err(e);
                }
            }
        }
        let shards: [CachePadded<RwLock<I>>; N] =
            unsafe { core::mem::transmute_copy(&shards_uninit) };
        Ok(Self { shards })
    }

    pub fn try_insert_common<'a>(
        &self,
        lookup: I::Lookup<'a>,
        owned: I::Owned<'a>,
    ) -> Result<I::Id, SetError>
    where
        I::Lookup<'a>: Hash + PartialEq,
    {
        use std::hash::BuildHasher;
        let hash = Hasher::default().hash_one(lookup);
        let idx = Self::select_shard(hash);
        let lock = &self.shards[idx];

        let read_len = {
            let guard = lock.read();
            if let Some(id) = unsafe { guard.find_with_hash(hash, lookup) } {
                return Ok(id);
            }
            guard.len()
        };

        let mut guard = lock.write();
        if guard.len() != read_len {
            if let Some(id) = unsafe { guard.find_with_hash(hash, lookup) } {
                return Ok(id);
            }
        }

        unsafe { guard.insert_unique_uncontended_with_hash(hash, owned) }
    }
}

// SAFETY: relies on safety requirements of `SetOps`.
unsafe impl<I: SetOps, const N: usize> Send for Sharded<I, N> {}
unsafe impl<I: SetOps, const N: usize> Sync for Sharded<I, N> {}
