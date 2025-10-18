// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::SetHasher as Hasher;
use super::{SetError, SetOps};
use core::hint::unreachable_unchecked;
use core::{any::TypeId, fmt, mem, ptr};
use datadog_alloc::{Allocator, ChainAllocator, VirtualAllocator};
use hashbrown::HashTable;
use std::ffi::c_void;
use std::hash::{BuildHasher, Hash};

pub const SET_MIN_CAPACITY: usize = 14;

#[repr(transparent)]
#[derive(Debug, Eq, Hash, PartialEq)]
pub struct SetId<T>(pub(crate) ptr::NonNull<T>);

impl<T> SetId<T> {
    /// Cast to another type. Although this is safe, using the result is not
    /// necessarily safe.
    #[inline]
    #[must_use]
    pub fn cast<U>(self) -> SetId<U> {
        SetId(self.0.cast())
    }

    pub fn into_raw(self) -> ptr::NonNull<c_void> {
        self.0.cast()
    }

    /// Re-creates a [`SetId`] from calling [`SetId::into_raw`].
    ///
    /// # Safety
    ///
    /// The set it belongs to must still be alive, and the repr should be
    /// unchanged since it was created by [`SetId::into_raw`].
    pub unsafe fn from_raw(raw: ptr::NonNull<c_void>) -> Self {
        Self(raw.cast::<T>())
    }
}

// This is different from derive(Clone), because derive(Clone) will be Clone
// only if T is Clone, and that's not true here--the type is always Clone.
impl<T> Clone for SetId<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for SetId<T> {}

impl<T: Hash + Eq + 'static> fmt::Debug for Set<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Set").field("table", &self.table).finish()
    }
}

pub struct Set<T: Hash + Eq + 'static> {
    pub(crate) arena: ChainAllocator<VirtualAllocator>,
    pub(crate) table: HashTable<ptr::NonNull<T>>,
}

impl<T: Eq + Hash + 'static> Set<T> {
    const SIZE_HINT: usize = 1024 * 1024;

    pub fn try_new() -> Result<Self, SetError> {
        Self::try_with_capacity(SET_MIN_CAPACITY)
    }

    #[inline]
    pub(crate) fn allocate_one(&mut self, value: T) -> Result<ptr::NonNull<T>, SetError> {
        let layout = core::alloc::Layout::new::<T>();
        // Allocate raw bytes for a single `T`
        let obj = self.arena.allocate(layout)?; // Result<NonNull<[u8]>, AllocError>
        let raw_slice_ptr: *mut [u8] = obj.as_ptr();
        let raw = raw_slice_ptr as *mut u8 as *mut T;

        // SAFETY: `raw` points to allocated, properly aligned memory for `T`.
        unsafe { ptr::write(raw, value) };

        // SAFETY: cannot be null as it was just allocated.
        Ok(unsafe { ptr::NonNull::new_unchecked(raw) })
    }

    pub fn try_insert(&mut self, value: T) -> Result<SetId<T>, SetError> {
        let hash = Hasher::default().hash_one(&value);
        // SAFETY: hash computed by this set's hasher for value.
        if let Some(existing) = unsafe { self.find_with_hash(hash, &value) } {
            return Ok(existing);
        }
        // SAFETY: hash computed by this set's hasher, uniqueness is enforced
        // by a prior find.
        unsafe { self.insert_unique_uncontended_with_hash(hash, value) }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// Returns the `SetId` for `value` if it exists in the set, without inserting.
    ///
    /// This is primarily intended for tests and debugging. In typical usage
    /// you should prefer `try_insert` which handles both existence checks and
    /// insertion atomically in the intended access pattern.
    pub fn find(&self, value: &T) -> Option<SetId<T>> {
        let hash = Hasher::default().hash_one(value);
        // SAFETY: `hash` was computed using this set's hasher over `&value`.
        unsafe { self.find_with_hash(hash, value) }
    }

    /// Returns a shared reference to the value for a given `SetId`.
    ///
    /// # Safety
    /// - The `id` must have been obtained from this exact `Set<T>` instance (or remain valid for
    ///   it). Using an id from another set, or after the backing arena is torn down, is undefined
    ///   behavior.
    /// # Safety
    /// - `id` must have been obtained from this exact `Set<T>` instance and still refer to a live
    ///   element in its arena.
    pub unsafe fn get(&self, id: SetId<T>) -> &T {
        // SAFETY: Caller guarantees the `SetId` belongs to this set and points
        // to a live, properly aligned `T` in the arena.
        unsafe { id.0.as_ref() }
    }
}

impl<T: Hash + Eq + 'static> Drop for Set<T> {
    fn drop(&mut self) {
        if mem::needs_drop::<T>() {
            for nn in self.table.iter() {
                // SAFETY: Elements in the table were allocated and initialized
                // via `allocate_one` and remain valid for the lifetime of this
                // set (arena-backed). We only drop if `T` requires dropping.
                unsafe { ptr::drop_in_place(nn.as_ptr()) };
            }
        }
    }
}

unsafe impl<T: Hash + Eq + 'static> SetOps for Set<T> {
    type Lookup<'a> = &'a T;
    type Owned<'a> = T;
    type Id = SetId<T>;

    fn try_with_capacity(capacity: usize) -> Result<Self, SetError> {
        let arena = ChainAllocator::new_in(Self::SIZE_HINT, VirtualAllocator {});
        let mut table = HashTable::new();

        // SAFETY: new empty table cannot require rehash, callback unreachable.
        table.try_reserve(capacity, |_| unsafe { unreachable_unchecked() })?;
        Ok(Self { arena, table })
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    unsafe fn find_with_hash(&self, hash: u64, key: Self::Lookup<'_>) -> Option<Self::Id> {
        let found = self
            .table
            // SAFETY: NonNull<T> inside table points to live, properly aligned Ts.
            .find(hash, |nn| unsafe { nn.as_ref() == key })?;
        Some(SetId(*found))
    }

    unsafe fn insert_unique_uncontended_with_hash(
        &mut self,
        hash: u64,
        value: Self::Owned<'_>,
    ) -> Result<Self::Id, SetError> {
        // Reserve table space BEFORE allocating the new value so we don't
        // need to drop it on reserve failure.
        // SAFETY: NonNull<T> entries are valid; closure only hashes existing entries.
        self.table
            .try_reserve(1, |nnv| Hasher::default().hash_one(unsafe { nnv.as_ref() }))?;
        let nn = self.allocate_one(value)?;
        // SAFETY: reserve above guarantees no rehash occurs; closure unreachable.
        self.table
            .insert_unique(hash, nn, |_| unsafe { unreachable_unchecked() });
        Ok(SetId(nn))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet as StdHashSet;
    use std::sync::{Arc, Weak};

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: if cfg!(miri) { 4 } else { 64 },
            .. ProptestConfig::default()
        })]

        #[test]
        fn proptest_matches_std_hashset(values in proptest::collection::vec(any::<u64>(), 0..if cfg!(miri) { 32 } else { 512 })) {
            let mut set = Set::<u64>::try_new().unwrap();
            let mut shadow = StdHashSet::<u64>::new();

            for v in &values {
                shadow.insert(*v);
                let _ = set.try_insert(*v).unwrap();
            }

            prop_assert_eq!(set.len(), shadow.len());

            for &v in &shadow {
                let id = set.find(&v).unwrap();
                // SAFETY: id just obtained from this set
                let fetched = unsafe { set.get(id) };
                prop_assert_eq!(*fetched, v);
            }
        }
    }

    #[test]
    fn set_drops_elements_on_drop() {
        let mut set = Set::<Arc<u64>>::try_new().unwrap();
        let mut weaks: Vec<Weak<u64>> = Vec::new();

        let total = if cfg!(miri) { 8 } else { 64 };
        for i in 0..total {
            let arc = Arc::new(i as u64);
            weaks.push(Arc::downgrade(&arc));
            // Transfer ownership into the set
            let _ = set.try_insert(arc).unwrap();
        }

        drop(set);

        for (idx, w) in weaks.iter().enumerate() {
            assert!(w.upgrade().is_none(), "weak at {idx} still alive");
        }
    }
}
