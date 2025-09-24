// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::any::TypeId;
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

    /// Returns the `TypeId` of the logical element type stored by this set.
    fn type_id(&self) -> TypeId;

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
pub struct Sharded<S: SetOps, const N: usize> {
    pub(crate) shards: [CachePadded<RwLock<S>>; N],
    pub(crate) type_id: TypeId,
}

impl<S: SetOps, const N: usize> Sharded<S, N> {
    #[inline]
    pub const fn is_power_of_two_gt1() -> bool {
        N.is_power_of_two() && N > 1
    }

    #[inline]
    pub fn select_shard(hash: u64) -> usize {
        (hash as usize) & (N - 1)
    }

    pub fn try_new_with_min_capacity(min_capacity: usize) -> Result<Self, SetError> {
        if !Self::is_power_of_two_gt1() {
            return Err(SetError::InvalidArgument);
        }
        let mut shards_uninit: [MaybeUninit<CachePadded<RwLock<S>>>; N] =
            unsafe { MaybeUninit::uninit().assume_init() };
        let mut i = 0usize;
        while i < N {
            match S::try_with_capacity(min_capacity) {
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
        let shards: [CachePadded<RwLock<S>>; N] =
            unsafe { core::mem::transmute_copy(&shards_uninit) };
        // If N=0, then we error at the very top of the function, so we know
        // there's at least one.
        let type_id = shards[0].read().type_id();
        Ok(Self { shards, type_id })
    }

    pub fn try_insert_common<'a>(
        &self,
        lookup: S::Lookup<'a>,
        owned: S::Owned<'a>,
    ) -> Result<S::Id, SetError>
    where
        S::Lookup<'a>: Hash + PartialEq,
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

    #[inline]
    pub fn element_type_id(&self) -> TypeId {
        self.type_id
    }
}

// SAFETY: relies on safety requirements of `SetOps`.
unsafe impl<S: SetOps, const N: usize> Send for Sharded<S, N> {}
unsafe impl<S: SetOps, const N: usize> Sync for Sharded<S, N> {}
