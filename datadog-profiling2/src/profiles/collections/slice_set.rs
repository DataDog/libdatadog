// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{SetError, SetOps, ThinSlice};
use core::any::TypeId;
use core::hash;
use datadog_alloc::{ChainAllocator, VirtualAllocator};
use hashbrown::HashTable;
use std::hash::{BuildHasher, Hash};
use std::hint::unreachable_unchecked;

use super::SetHasher as Hasher;

/// Holds unique slices and provides handles to fetch them later.
pub struct SliceSet<T: Copy + hash::Hash + Eq + 'static> {
    /// The bytes of each slice stored in `slices` are allocated here.
    pub(crate) arena: ChainAllocator<VirtualAllocator>,

    /// The unordered hash set of unique slices.
    /// The static lifetimes are a lie; they are tied to the `arena`, which is
    /// only moved if the slice set is moved.
    /// References to the underlying slices should generally not be handed,
    /// but if they are, they should be bound to the slice set's lifetime.
    pub(crate) slices: HashTable<ThinSlice<'static, T>>,
}

impl<T: Copy + hash::Hash + Eq + 'static> SliceSet<T> {
    const SIZE_HINT: usize = 1024 * 1024;

    pub fn try_with_capacity(capacity: usize) -> Result<Self, SetError> {
        let arena = ChainAllocator::new_in(Self::SIZE_HINT, VirtualAllocator {});

        let mut slices = HashTable::new();
        // SAFETY: we just made the empty hash table, so there's nothing that
        // needs to be rehashed.
        slices.try_reserve(capacity, |_| unsafe { unreachable_unchecked() })?;

        Ok(SliceSet { arena, slices })
    }

    /// # Safety
    ///
    /// The slice must not already exist within the set.
    pub unsafe fn insert_unique_uncontended(
        &mut self,
        slice: &[T],
    ) -> Result<ThinSlice<'static, T>, SetError> {
        let hash = Hasher::default().hash_one(slice);
        self.insert_unique_uncontended_with_hash(hash, slice)
    }

    /// # Safety
    ///  1. The hash must be the same as if the slice was re-hashed with the hasher the slice set
    ///     would use.
    ///  2. The slice must not already exist within the set.
    #[inline(never)]
    pub unsafe fn insert_unique_uncontended_with_hash(
        &mut self,
        hash: u64,
        slice: &[T],
    ) -> Result<ThinSlice<'static, T>, SetError> {
        let obj = ThinSlice::try_allocate_for(slice, &self.arena)?;
        let uninit = unsafe { &mut *obj.as_ptr() };
        let new_slice = ThinSlice::try_from_slice_in(slice, uninit)?;

        self.slices
            .try_reserve(1, |thin| Hasher::default().hash_one(thin.as_slice()))?;

        // Add it to the set. The memory was previously reserved.
        // SAFETY: The try_reserve above means any necessary re-hashing has
        // already been done, so the hash closure cannot be called.
        self.slices
            .insert_unique(hash, new_slice, |_| unsafe { unreachable_unchecked() });

        Ok(new_slice)
    }

    /// Adds the slice to the slice set if it isn't present already, and
    /// returns a handle to the slice that can be used to retrieve it later.
    pub fn try_insert(&mut self, slice: &[T]) -> Result<ThinSlice<'static, T>, SetError>
    where
        T: hash::Hash,
    {
        let hash = Hasher::default().hash_one(slice);

        // SAFETY: the slice's hash is correct, we use the same hasher as
        // SliceSet uses.
        if let Some(id) = unsafe { self.find_with_hash(hash, slice) } {
            return Ok(id);
        }

        // SAFETY: we just checked above that the slice isn't in the set.
        unsafe { self.insert_unique_uncontended(slice) }
    }

    /// # Safety
    /// The hash must be the same as if the slice was re-hashed with the
    /// hasher the slice set would use.
    #[inline(never)]
    pub(crate) unsafe fn find_with_hash(
        &self,
        hash: u64,
        slice: &[T],
    ) -> Option<ThinSlice<'static, T>>
    where
        T: PartialEq,
    {
        let interned_slice = self
            .slices
            .find(hash, |thin_slice| thin_slice.as_slice() == slice)?;
        Some(*interned_slice)
    }

    /// Returns an iterator over all slices in the set.
    pub fn iter(&self) -> impl Iterator<Item = ThinSlice<'_, T>> + '_ {
        self.slices.iter().copied()
    }

    /// Returns the number of slices in the set.
    pub fn len(&self) -> usize {
        self.slices.len()
    }

    /// Returns true if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.slices.is_empty()
    }

    /// Returns the capacity of the hash table.
    pub fn capacity(&self) -> usize {
        self.slices.capacity()
    }
}

unsafe impl<T: Copy + Hash + Eq + 'static> SetOps for SliceSet<T> {
    type Lookup<'a> = &'a [T];
    type Owned<'a> = &'a [T];
    type Id = ThinSlice<'static, T>;

    fn try_with_capacity(capacity: usize) -> Result<Self, SetError> {
        SliceSet::try_with_capacity(capacity)
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    unsafe fn find_with_hash(&self, hash: u64, key: Self::Lookup<'_>) -> Option<Self::Id> {
        unsafe { self.find_with_hash(hash, key) }
    }

    unsafe fn insert_unique_uncontended_with_hash(
        &mut self,
        hash: u64,
        key: Self::Owned<'_>,
    ) -> Result<Self::Id, SetError> {
        unsafe { self.insert_unique_uncontended_with_hash(hash, key) }
    }
}
