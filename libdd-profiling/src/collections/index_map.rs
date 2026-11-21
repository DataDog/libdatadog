// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! An insertion-ordered hash map over [`HashTable`], using a caller-chosen
//! [`Allocator`].
//!
//! Entries live in one dense array in insertion order, and the table's
//! buckets hold `u32` indexes into it. Each key therefore has a stable index,
//! `0..len` in the order keys were first inserted, which is what serializers
//! need to assign sequential IDs. Entries are never removed.
//!
//! Every allocation is fallible: operations that may allocate return
//! [`AllocError`] instead of aborting, and leave the map unchanged when they
//! fail. When the table fills up, it is rebuilt at twice the size from the
//! cached hashes, so keys are never hashed twice.

#[cfg(test)]
mod tests;

use crate::collections::alloc::{AllocError, Allocator};
use crate::collections::hash_table::{self, GROUP_CAPACITY, Group, HashTable, HashTableRef};
use core::alloc::Layout;
use core::hash::{BuildHasher, Hash};
use core::iter::FusedIterator;
use core::mem::{self, MaybeUninit};
use core::ptr::{self, NonNull};
use core::{fmt, slice};

/// The lookup trait: `Q: Equivalent<K>` means a `Q` can find a `K` key. Every
/// `Q` that `K` borrows as qualifies, and other types can implement it, such as
/// a pair of `&str`s for a pair of `String`s. Equivalent values must hash the
/// same.
pub use equivalent::Equivalent;

/// The default hasher builder: foldhash with a fixed seed.
///
/// It is fast and mixes both ends of the hash, which [`HashTable`] relies on: it
/// takes its tag from the top 7 bits and its start group from the rest. It is
/// deterministic and not designed to resist deliberate collisions; use
/// [`DefaultHashBuilder::with_seed`] with a secret seed where untrusted keys
/// could be used to degrade a map.
pub type DefaultHashBuilder = foldhash::fast::FixedState;

/// One entry. The hash is cached so the table can be rebuilt without hashing
/// keys again, and so most mismatches are rejected without comparing keys.
struct Slot<K, V> {
    hash: u64,
    key: K,
    value: V,
}

/// A hash map with stable insertion-order indexes and no removal.
///
/// At most `u32::MAX` entries fit, because the table stores `u32` indexes.
pub struct IndexMap<K, V, A: Allocator, S = DefaultHashBuilder> {
    /// The index: buckets hold positions in `slots`. Every entry in `slots`
    /// is in the table, so the table's length is `slots.len`.
    table: Table,
    slots: Slots<Slot<K, V>>,
    hash_builder: S,
    alloc: A,
}

impl<K, V, A: Allocator, S: Default> IndexMap<K, V, A, S> {
    /// An empty map that allocates nothing until the first insertion.
    #[must_use]
    pub fn new_in(alloc: A) -> Self {
        Self::with_hasher_in(S::default(), alloc)
    }

    /// An empty map with room for at least `capacity` entries.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated, or if `capacity`
    /// exceeds `u32::MAX`.
    pub fn try_with_capacity_in(capacity: usize, alloc: A) -> Result<Self, AllocError> {
        let mut map = Self::new_in(alloc);
        map.try_reserve(capacity)?;
        Ok(map)
    }
}

impl<K, V, A: Allocator, S> IndexMap<K, V, A, S> {
    /// An empty map that uses `hash_builder` to hash keys and allocates
    /// nothing until the first insertion.
    #[must_use]
    pub const fn with_hasher_in(hash_builder: S, alloc: A) -> Self {
        Self {
            table: Table::empty(),
            slots: Slots::new(),
            hash_builder,
            alloc,
        }
    }

    /// The number of entries.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.slots.len
    }

    /// Whether the map has no entries.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.slots.len == 0
    }

    /// The number of entries the map can hold without allocating.
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        let table = self.table.capacity();
        if table < self.slots.cap {
            table
        } else {
            self.slots.cap
        }
    }

    /// The allocator.
    #[inline]
    #[must_use]
    pub const fn allocator(&self) -> &A {
        &self.alloc
    }

    /// The hasher builder.
    #[inline]
    #[must_use]
    pub const fn hasher(&self) -> &S {
        &self.hash_builder
    }

    /// The key and value at `index`, the position the key was inserted at.
    #[inline]
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<(&K, &V)> {
        self.slots
            .as_slice()
            .get(index)
            .map(|slot| (&slot.key, &slot.value))
    }

    /// Iterates over the entries in insertion order.
    #[inline]
    #[must_use]
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            slots: self.slots.as_slice().iter(),
        }
    }

    /// Iterates over the keys in insertion order.
    #[inline]
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> + DoubleEndedIterator + FusedIterator {
        self.slots.as_slice().iter().map(|slot| &slot.key)
    }

    /// Iterates over the values in insertion order.
    #[inline]
    pub fn values(
        &self,
    ) -> impl ExactSizeIterator<Item = &V> + DoubleEndedIterator + FusedIterator {
        self.slots.as_slice().iter().map(|slot| &slot.value)
    }

    /// Makes room for at least `additional` more entries, so that inserting
    /// them does not allocate.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated, or if the map would
    /// exceed `u32::MAX` entries. The map is unchanged.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), AllocError> {
        let needed = self.slots.len.checked_add(additional).ok_or(AllocError)?;
        if u32::try_from(needed).is_err() {
            return Err(AllocError);
        }
        self.slots.try_reserve(additional, &self.alloc)?;
        if needed > self.table.capacity() {
            self.grow_table(needed)?;
        }
        Ok(())
    }

    /// Replaces the table with a larger one holding at least `min_entries`,
    /// and reinserts every index from its cached hash.
    fn grow_table(&mut self, min_entries: usize) -> Result<(), AllocError> {
        let groups = min_entries
            .div_ceil(GROUP_CAPACITY)
            .max(self.table.groups.saturating_mul(2))
            .max(1);
        let mut table = Table::allocate(&self.alloc, groups)?;
        let mut index = table.view_mut(0);
        let mut complete = true;
        for (position, slot) in self.slots.as_slice().iter().enumerate() {
            complete &= u32::try_from(position)
                .is_ok_and(|position| index.insert_unique(slot.hash, position).is_ok());
        }
        if !complete {
            // Unreachable: the new table holds more than `len` entries.
            // SAFETY: the table was just allocated by `self.alloc`.
            unsafe { table.free(&self.alloc) };
            return Err(AllocError);
        }
        let mut old = mem::replace(&mut self.table, table);
        // SAFETY: the old table was allocated by `self.alloc` and is unused.
        unsafe { old.free(&self.alloc) };
        Ok(())
    }
}

impl<K, V, A: Allocator, S: BuildHasher> IndexMap<K, V, A, S> {
    /// The index of the entry whose key equals `key`.
    fn find<Q>(&self, hash: u64, key: &Q) -> Option<usize>
    where
        Q: Equivalent<K> + ?Sized,
    {
        let slots = self.slots.as_slice();
        let index = self.table.view().find(hash, |&position| {
            slots
                .get(usize_from(position))
                .is_some_and(|slot| slot.hash == hash && key.equivalent(&slot.key))
        })?;
        Some(usize_from(*index))
    }

    /// The index of `key`, the position it was inserted at.
    #[inline]
    pub fn get_index_of<Q>(&self, key: &Q) -> Option<usize>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.find(self.hash_builder.hash_one(key), key)
    }

    /// The value for `key`.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.get_full(key).map(|(_, _, value)| value)
    }

    /// The index, stored key, and value for `key`.
    #[inline]
    pub fn get_full<Q>(&self, key: &Q) -> Option<(usize, &K, &V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        let index = self.get_index_of(key)?;
        let slot = self.slots.as_slice().get(index)?;
        Some((index, &slot.key, &slot.value))
    }

    /// The value for `key`, mutably.
    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        let index = self.get_index_of(key)?;
        self.slots
            .as_mut_slice()
            .get_mut(index)
            .map(|slot| &mut slot.value)
    }

    /// Whether the map has an entry for `key`.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.get_index_of(key).is_some()
    }

    /// The entry for `key`, for inspecting or inserting in place.
    ///
    /// When the key is absent, room for it is reserved before the entry is
    /// returned, so [`VacantEntry::insert`] cannot fail. Dropping a vacant
    /// entry leaves the map unchanged apart from that reserved capacity.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the key is absent and room for it cannot be
    /// allocated, or the map already has `u32::MAX` entries. The map is
    /// unchanged.
    pub fn try_entry(&mut self, key: K) -> Result<Entry<'_, K, V>, AllocError>
    where
        K: Hash + Eq,
    {
        let hash = self.hash_builder.hash_one(&key);
        if let Some(index) = self.find(hash, &key) {
            // `find` only returns indexes of existing entries.
            let slot = self.slots.as_mut_slice().get_mut(index).ok_or(AllocError)?;
            return Ok(Entry::Occupied(OccupiedEntry { slot, index }));
        }

        self.try_reserve(1)?;
        let index = u32::try_from(self.slots.len).map_err(|_| AllocError)?;
        let Self { table, slots, .. } = self;
        // Guaranteed by the reservation above.
        let Some((spare, len)) = slots.spare() else {
            return Err(AllocError);
        };
        Ok(Entry::Vacant(VacantEntry {
            table,
            spare,
            len,
            hash,
            key,
            index,
        }))
    }

    /// Inserts `value` for `key`. Returns the key's index, and the previous
    /// value if the key was already present (its stored key is kept).
    ///
    /// # Errors
    ///
    /// As for [`Self::try_entry`]. On error, `key` and `value` are dropped.
    pub fn try_insert(&mut self, key: K, value: V) -> Result<(usize, Option<V>), AllocError>
    where
        K: Hash + Eq,
    {
        match self.try_entry(key)? {
            Entry::Occupied(mut entry) => {
                let old = mem::replace(entry.get_mut(), value);
                Ok((entry.index(), Some(old)))
            }
            Entry::Vacant(entry) => {
                let index = entry.index();
                entry.insert(value);
                Ok((index, None))
            }
        }
    }
}

impl<K, V, A: Allocator, S> Drop for IndexMap<K, V, A, S> {
    fn drop(&mut self) {
        // SAFETY: both were allocated by `self.alloc` and are not used again.
        unsafe {
            self.slots.free(&self.alloc);
            self.table.free(&self.alloc);
        }
    }
}

impl<K, V, A: Allocator + Default, S: Default> Default for IndexMap<K, V, A, S> {
    fn default() -> Self {
        Self::new_in(A::default())
    }
}

impl<K: fmt::Debug, V: fmt::Debug, A: Allocator, S> fmt::Debug for IndexMap<K, V, A, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<'m, K, V, A: Allocator, S> IntoIterator for &'m IndexMap<K, V, A, S> {
    type Item = (&'m K, &'m V);
    type IntoIter = Iter<'m, K, V>;

    fn into_iter(self) -> Iter<'m, K, V> {
        self.iter()
    }
}

/// A key's place in the map, from [`IndexMap::try_entry`].
#[derive(Debug)]
pub enum Entry<'m, K, V> {
    /// The key is present.
    Occupied(OccupiedEntry<'m, K, V>),
    /// The key is absent, and room for it is reserved.
    Vacant(VacantEntry<'m, K, V>),
}

impl<K, V> Entry<'_, K, V> {
    /// The key's index: its existing position, or the one it will get.
    #[inline]
    #[must_use]
    pub const fn index(&self) -> usize {
        match self {
            Self::Occupied(entry) => entry.index(),
            Self::Vacant(entry) => entry.index(),
        }
    }
}

/// An existing entry.
#[derive(Debug)]
pub struct OccupiedEntry<'m, K, V> {
    slot: &'m mut Slot<K, V>,
    index: usize,
}

impl<'m, K, V> OccupiedEntry<'m, K, V> {
    /// The entry's index, the position its key was inserted at.
    #[inline]
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The stored key.
    #[inline]
    #[must_use]
    pub const fn key(&self) -> &K {
        &self.slot.key
    }

    /// The value.
    #[inline]
    #[must_use]
    pub const fn get(&self) -> &V {
        &self.slot.value
    }

    /// The value, mutably.
    #[inline]
    pub const fn get_mut(&mut self) -> &mut V {
        &mut self.slot.value
    }

    /// The value, mutably, for as long as the map is borrowed.
    #[inline]
    #[must_use]
    pub const fn into_mut(self) -> &'m mut V {
        &mut self.slot.value
    }
}

/// An absent key with room reserved for it.
pub struct VacantEntry<'m, K, V> {
    table: &'m mut Table,
    spare: &'m mut MaybeUninit<Slot<K, V>>,
    len: &'m mut usize,
    hash: u64,
    key: K,
    index: u32,
}

impl<'m, K, V> VacantEntry<'m, K, V> {
    /// The index the key will get, which is the map's current length.
    #[inline]
    #[must_use]
    pub const fn index(&self) -> usize {
        usize_from(self.index)
    }

    /// The key that would be inserted.
    #[inline]
    #[must_use]
    pub const fn key(&self) -> &K {
        &self.key
    }

    /// Gives the key back without inserting it.
    #[inline]
    #[must_use]
    pub fn into_key(self) -> K {
        self.key
    }

    /// Inserts the key with `value`. This cannot fail: room was reserved
    /// when the entry was created.
    #[inline]
    pub fn insert(self, value: V) -> &'m mut V {
        let Self {
            table,
            spare,
            len,
            hash,
            key,
            index,
        } = self;
        // The table holds every existing entry, and has room for one more.
        let inserted = table.view_mut(*len).insert_unique(hash, index);
        debug_assert!(inserted.is_ok(), "room was reserved by try_entry");
        let slot = spare.write(Slot { hash, key, value });
        *len += 1;
        &mut slot.value
    }
}

impl<K: fmt::Debug, V> fmt::Debug for VacantEntry<'_, K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VacantEntry")
            .field("index", &self.index)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for Slot<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("key", &self.key)
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

/// Iterator over a map's entries in insertion order.
#[derive(Clone)]
pub struct Iter<'m, K, V> {
    slots: slice::Iter<'m, Slot<K, V>>,
}

impl<'m, K, V> Iterator for Iter<'m, K, V> {
    type Item = (&'m K, &'m V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.slots.next().map(|slot| (&slot.key, &slot.value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.slots.size_hint()
    }
}

impl<K, V> DoubleEndedIterator for Iter<'_, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.slots.next_back().map(|slot| (&slot.key, &slot.value))
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}
impl<K, V> FusedIterator for Iter<'_, K, V> {}

impl<K, V> fmt::Debug for Iter<'_, K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Iter")
            .field("remaining", &self.slots.len())
            .finish()
    }
}

/// Widens a table index. Every supported target has at least 32-bit usize.
#[inline]
const fn usize_from(index: u32) -> usize {
    index as usize
}

/// The index table's memory, held as a raw pointer.
///
/// The map builds a [`HashTable`] or [`HashTableRef`] view for each operation
/// instead of storing one: a stored view would hold references into this
/// block, and moving the map into a function that frees it (such as `drop`)
/// would then free memory those references still claim.
struct Table {
    /// The block's start, aligned for [`Group`]. Dangling when `groups` is 0.
    ptr: NonNull<u8>,
    layout: Layout,
    groups: usize,
}

// SAFETY: the block is plain bytes uniquely owned by one map, which frees it
// with its own allocator; the pointer is never shared.
unsafe impl Send for Table {}
// SAFETY: `&Table` only creates read-only views.
unsafe impl Sync for Table {}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("groups", &self.groups)
            .finish_non_exhaustive()
    }
}

impl Table {
    /// No memory and no groups: always full.
    const fn empty() -> Self {
        Self {
            ptr: NonNull::<Group>::dangling().cast(),
            layout: Layout::new::<()>(),
            groups: 0,
        }
    }

    /// Allocates zeroed memory, which is an empty table, for `groups` groups.
    fn allocate<A: Allocator>(alloc: &A, groups: usize) -> Result<Self, AllocError> {
        let size = hash_table::bytes_for_groups::<u32>(groups).ok_or(AllocError)?;
        let layout = Layout::from_size_align(size, align_of::<Group>()).map_err(|_| AllocError)?;
        let ptr = alloc.allocate_zeroed(layout)?.cast::<u8>();
        Ok(Self {
            ptr,
            layout,
            groups,
        })
    }

    const fn capacity(&self) -> usize {
        hash_table::capacity_for_groups(self.groups)
    }

    fn view(&self) -> HashTableRef<'_, u32> {
        // SAFETY: the block holds `layout.size()` initialized bytes for as
        // long as `self` is borrowed (none at a dangling pointer).
        let bytes = unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.layout.size()) };
        // Cannot fail: the block is aligned and sized for `groups`.
        HashTableRef::new(bytes, self.groups).unwrap_or(HashTableRef::empty())
    }

    /// A mutable view; `len` is the number of occupied buckets.
    fn view_mut(&mut self, len: usize) -> HashTable<'_, u32> {
        // SAFETY: as in `view`, with exclusive access through `&mut self`.
        let bytes = unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) };
        // Cannot fail: the block is aligned and sized for `groups`.
        HashTable::reopen(bytes, self.groups, len).unwrap_or(HashTable::empty())
    }

    /// Frees the block, leaving an empty table.
    ///
    /// # Safety
    ///
    /// `alloc` must be the allocator the block came from.
    unsafe fn free<A: Allocator>(&mut self, alloc: &A) {
        let Self {
            ptr,
            layout,
            groups,
        } = mem::replace(self, Self::empty());
        if groups != 0 {
            // SAFETY: allocated by `alloc` with exactly this layout.
            unsafe { alloc.deallocate(ptr, layout) };
        }
    }
}

/// A growable buffer of initialized `T`s at the front of an allocation, like a
/// minimal `Vec<T, A>`. It does not own its allocator; the map passes its own.
struct Slots<T> {
    ptr: NonNull<T>,
    cap: usize,
    len: usize,
}

// SAFETY: `Slots` uniquely owns its elements, like `Vec<T>`.
unsafe impl<T: Send> Send for Slots<T> {}
// SAFETY: shared access only reads elements, through `&self`.
unsafe impl<T: Sync> Sync for Slots<T> {}

impl<T> Slots<T> {
    const fn new() -> Self {
        const { assert!(size_of::<T>() != 0, "slots always hold a hash") };
        Self {
            ptr: NonNull::dangling(),
            cap: 0,
            len: 0,
        }
    }

    const fn as_slice(&self) -> &[T] {
        // SAFETY: the first `len` elements are initialized, and `ptr` is
        // non-null and aligned (dangling only when `len` is 0).
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    const fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: as in `as_slice`, with exclusive access.
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Ensures room for `additional` more elements, growing geometrically.
    fn try_reserve<A: Allocator>(
        &mut self,
        additional: usize,
        alloc: &A,
    ) -> Result<(), AllocError> {
        let needed = self.len.checked_add(additional).ok_or(AllocError)?;
        if needed <= self.cap {
            return Ok(());
        }
        let cap = needed.max(self.cap.saturating_mul(2)).max(4);
        let layout = Layout::array::<T>(cap).map_err(|_| AllocError)?;
        let block = if self.cap == 0 {
            alloc.allocate(layout)?
        } else {
            let old = Layout::array::<T>(self.cap).map_err(|_| AllocError)?;
            // SAFETY: `ptr` is allocated by `alloc` with `old`, and `layout`
            // is larger with the same alignment. On error nothing changes.
            unsafe { alloc.grow(self.ptr.cast(), old, layout)? }
        };
        self.ptr = block.cast();
        self.cap = cap;
        Ok(())
    }

    /// The uninitialized element just past the end, with the length to bump
    /// once it is written, if there is room.
    fn spare(&mut self) -> Option<(&mut MaybeUninit<T>, &mut usize)> {
        if self.len >= self.cap {
            return None;
        }
        // SAFETY: `len < cap`, so this element is inside the allocation,
        // uninitialized, and not otherwise referenced.
        let slot = unsafe { &mut *self.ptr.as_ptr().add(self.len).cast::<MaybeUninit<T>>() };
        Some((slot, &mut self.len))
    }

    /// Drops the elements and frees the buffer.
    ///
    /// # Safety
    ///
    /// `alloc` must be the allocator the buffer grew with.
    unsafe fn free<A: Allocator>(&mut self, alloc: &A) {
        let elements: *mut [T] = self.as_mut_slice();
        self.len = 0;
        // SAFETY: the elements are initialized and, with `len` reset, are
        // dropped only here.
        unsafe { ptr::drop_in_place(elements) };
        if self.cap != 0
            && let Ok(layout) = Layout::array::<T>(self.cap)
        {
            // SAFETY: allocated by `alloc` with exactly this layout.
            unsafe { alloc.deallocate(self.ptr.cast(), layout) };
        }
        self.ptr = NonNull::dangling();
        self.cap = 0;
    }
}
