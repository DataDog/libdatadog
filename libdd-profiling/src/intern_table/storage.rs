// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! How a record type's values are laid out: [`Inline`] or [`Arena`].

use super::{InternError, usize_from};
use crate::collections::hash_table::Bucket;
use core::marker::PhantomData;
use core::ptr::{self, NonNull};

/// How an [`InternTable`](super::InternTable) lays out its records. It is
/// sealed: the two layouts are [`Inline`] and [`Arena`].
///
/// Only [`Storage::Item`] is meant for use outside this crate; the rest is
/// how the table reaches into its memory.
pub trait Storage: sealed::Sealed + Sized {
    /// The stored form of a value, compared to find duplicates.
    type Item<'a>: Copy;
    /// What the region hash tables store.
    #[doc(hidden)]
    type Bucket: Bucket;
    /// What the id map stores per id.
    #[doc(hidden)]
    type Slot: Bucket;
    /// Memory beyond the id map.
    #[doc(hidden)]
    type Extra: Copy;
    /// Whether the table needs an arena.
    #[doc(hidden)]
    const ARENA: bool;

    /// Picks this storage's part of the table's memory.
    #[doc(hidden)]
    fn extra(arena: ArenaMem) -> Self::Extra;
    /// The id a bucket stands for.
    #[doc(hidden)]
    fn id(bucket: &Self::Bucket) -> u32;
    /// The bucket for a new record.
    #[doc(hidden)]
    fn bucket(slot: Self::Slot, id: u32) -> Self::Bucket;

    /// Arena bytes a new record of `item` needs: 0 for inline records.
    ///
    /// # Errors
    ///
    /// [`InternError::TooLong`] if `item` cannot be one record.
    #[doc(hidden)]
    fn record_size(item: Self::Item<'_>) -> Result<usize, InternError>;

    /// Whether `bucket`'s record equals `item`.
    ///
    /// # Safety
    ///
    /// `bucket` was written by this table, so its record is published.
    #[doc(hidden)]
    unsafe fn matches(mem: &Mem<Self>, bucket: &Self::Bucket, item: Self::Item<'_>) -> bool;

    /// Writes a new record's arena bytes at `offset`, which has room for
    /// [`Self::record_size`] committed bytes, and returns its slot.
    ///
    /// # Safety
    ///
    /// The caller holds the writer lock, and no one reads those bytes yet.
    #[doc(hidden)]
    unsafe fn store(
        mem: &Mem<Self>,
        item: Self::Item<'_>,
        offset: usize,
    ) -> Result<Self::Slot, InternError>;

    /// Reads a record back from its slot.
    ///
    /// # Safety
    ///
    /// `slot` belongs to a published record.
    #[doc(hidden)]
    unsafe fn load(mem: &Mem<Self>, slot: Self::Slot) -> Self::Item<'_>;

    /// The arena offset just past a record: 0 for inline records. Recovery
    /// uses it to restore the arena cursor from the last published record.
    ///
    /// # Safety
    ///
    /// `slot` belongs to a published record.
    #[doc(hidden)]
    unsafe fn record_end(mem: &Mem<Self>, slot: Self::Slot) -> Option<usize>;
}

mod sealed {
    pub trait Sealed {}
    impl<T> Sealed for super::Inline<T> {}
    impl Sealed for super::Arena {}
}

/// Fixed-size records of type `T`, stored directly in the id map. The hash
/// bucket holds only the id.
pub struct Inline<T>(PhantomData<fn() -> T>);

/// Variable-length byte records in an arena. The id map holds each record's
/// arena offset, and the hash bucket holds the offset and the id, so finding
/// a duplicate does not go through the id map.
///
/// A record is a little-endian `u16` length followed by the bytes, padded to
/// 2-byte alignment. Offsets are stored halved, so a `u32` reaches 8 GiB.
pub struct Arena(core::convert::Infallible);

/// Pointers to the parts of a table's memory that records use.
pub struct Mem<S: Storage> {
    /// The id map: `max_ids` slots.
    pub ids: NonNull<S::Slot>,
    pub max_ids: u32,
    /// Storage-specific memory: nothing for inline records.
    pub extra: S::Extra,
}

// SAFETY: `Mem` points into a table's reservation, which holds only plain
// data accessed under the table's protocol (module documentation of
// `intern_table`); the pointers carry no thread affinity.
unsafe impl<S: Storage> Send for Mem<S> {}
// SAFETY: as above.
unsafe impl<S: Storage> Sync for Mem<S> {}

impl<S: Storage> Mem<S> {
    /// The slot of `id`, or `None` if it is out of range.
    ///
    /// # Safety
    ///
    /// The slot was written, and no one writes it concurrently.
    pub unsafe fn read_slot(&self, id: u32) -> Option<S::Slot> {
        // SAFETY: in range of the id map; the caller rules out a racing write.
        (id < self.max_ids).then(|| unsafe { self.ids.add(usize_from(id)).read() })
    }

    /// Writes the slot of `id`, if it is in range.
    ///
    /// # Safety
    ///
    /// The slot's page is committed, and no one reads or writes the slot
    /// concurrently.
    pub const unsafe fn write_slot(&self, id: u32, slot: S::Slot) {
        if id < self.max_ids {
            // SAFETY: in range of the id map; the caller rules out races.
            unsafe { self.ids.add(usize_from(id)).write(slot) };
        }
    }
}

/// The arena: `len` bytes at `base`. Inline tables get an empty one.
#[derive(Clone, Copy)]
pub struct ArenaMem {
    pub base: NonNull<u8>,
    pub len: usize,
}

impl ArenaMem {
    /// The record at `offset`: its bytes, and the offset just past it.
    /// `None` if it would run past the arena.
    ///
    /// # Safety
    ///
    /// The record was written, and no one writes it concurrently.
    unsafe fn record<'m>(&self, half: u32) -> Option<(&'m [u8], usize)> {
        let offset = usize_from(half).checked_mul(2)?;
        let start = offset.checked_add(2).filter(|&s| s <= self.len)?;
        // SAFETY: `offset + 2` is within the arena.
        let prefix = unsafe { self.base.add(offset).cast::<[u8; 2]>().read() };
        let len = usize::from(u16::from_le_bytes(prefix));
        let end = start.checked_add(len).filter(|&e| e <= self.len)?;
        // SAFETY: `start..end` is within the arena, and the caller rules out
        // writes to it.
        let bytes = unsafe { core::slice::from_raw_parts(self.base.add(start).as_ptr(), len) };
        Some((bytes, end.next_multiple_of(2)))
    }
}

impl<T: Bucket + Eq> Storage for Inline<T> {
    type Item<'a> = T;
    type Bucket = u32;
    type Slot = T;
    type Extra = ();
    const ARENA: bool = false;

    fn extra(_: ArenaMem) {}

    fn id(bucket: &u32) -> u32 {
        *bucket
    }

    fn bucket(_: T, id: u32) -> u32 {
        id
    }

    fn record_size(_: T) -> Result<usize, InternError> {
        Ok(0)
    }

    unsafe fn matches(mem: &Mem<Self>, bucket: &u32, item: T) -> bool {
        // SAFETY: the bucket's id is published.
        unsafe { mem.read_slot(*bucket) }.is_some_and(|stored| stored == item)
    }

    unsafe fn store(_: &Mem<Self>, item: T, _: usize) -> Result<T, InternError> {
        Ok(item)
    }

    unsafe fn load(_: &Mem<Self>, slot: T) -> T {
        slot
    }

    unsafe fn record_end(_: &Mem<Self>, _: T) -> Option<usize> {
        Some(0)
    }
}

impl Storage for Arena {
    type Item<'a> = &'a [u8];
    type Bucket = [u32; 2];
    type Slot = u32;
    type Extra = ArenaMem;
    const ARENA: bool = true;

    fn extra(arena: ArenaMem) -> ArenaMem {
        arena
    }

    fn id(bucket: &[u32; 2]) -> u32 {
        bucket[1]
    }

    fn bucket(slot: u32, id: u32) -> [u32; 2] {
        [slot, id]
    }

    fn record_size(item: &[u8]) -> Result<usize, InternError> {
        let len = u16::try_from(item.len()).map_err(|_| InternError::TooLong)?;
        Ok((2 + usize::from(len)).next_multiple_of(2))
    }

    unsafe fn matches(mem: &Mem<Self>, bucket: &[u32; 2], item: &[u8]) -> bool {
        // SAFETY: the bucket's record is published.
        unsafe { mem.extra.record(bucket[0]) }.is_some_and(|(stored, _)| stored == item)
    }

    unsafe fn store(mem: &Mem<Self>, item: &[u8], offset: usize) -> Result<u32, InternError> {
        let arena = &mem.extra;
        let len = u16::try_from(item.len()).map_err(|_| InternError::TooLong)?;
        let half = u32::try_from(offset / 2).map_err(|_| InternError::ArenaFull)?;
        let end = offset
            .checked_add(2 + item.len())
            .filter(|&end| offset.is_multiple_of(2) && end <= arena.len)
            .ok_or(InternError::ArenaFull)?;
        // SAFETY: `offset..end` is within the arena and committed (the
        // caller's contract), and no one reads it yet.
        unsafe {
            let record = arena.base.add(offset);
            record.cast::<[u8; 2]>().write(len.to_le_bytes());
            ptr::copy_nonoverlapping(item.as_ptr(), record.add(2).as_ptr(), end - offset - 2);
        }
        Ok(half)
    }

    unsafe fn load(mem: &Mem<Self>, slot: u32) -> &[u8] {
        // SAFETY: the slot's record is published.
        unsafe { mem.extra.record(slot) }.map_or(&[], |(bytes, _)| bytes)
    }

    unsafe fn record_end(mem: &Mem<Self>, slot: u32) -> Option<usize> {
        // SAFETY: the slot's record is published.
        unsafe { mem.extra.record(slot) }.map(|(_, end)| end)
    }
}
