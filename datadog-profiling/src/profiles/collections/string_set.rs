// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::ThinStr;
use core::hash;
use datadog_alloc::{AllocError, ChainAllocator, VirtualAllocator};
use hashbrown::HashTable;
use parking_lot::RwLock;
use std::hash::BuildHasher;
use std::hint::unreachable_unchecked;
use std::ops::Deref;
use std::sync::atomic::AtomicUsize;
use crossbeam_utils::CachePadded;

type Hasher = hash::BuildHasherDefault<rustc_hash::FxHasher>;

/// Represents a handle to a string that can be retrieved by the string table.
/// The exact representation is not a public detail.
#[repr(C)]
pub struct StringId(ThinStr<'static>);

/// Holds unique strings and provides [`StringId`]s to fetch them later.
pub struct StringSet {
    /// The bytes of each string stored in `strings` are allocated here.
    arena: RwLock<ChainAllocator<VirtualAllocator>>,

    /// The unordered hash set of unique strings.
    /// The static lifetimes are a lie; they are tied to the `arena`, which is
    /// only moved if the string set is moved e.g.
    /// [`StringSet::into_lending_iterator`].
    /// References to the underlying strings should generally not be handed,
    /// but if they are, they should be bound to the string set's lifetime or
    /// the lending iterator's lifetime.
    ///
    /// The reason for 16 locks comes from using 4 bits from the hash of the
    /// item being inserted. Note that we purposefully don't use the most
    /// significant bits of the hash because that's what the hash table uses
    /// in its SIMD hashing, so if we used the high bits, we'd be forcing a
    /// bunch of hash collisions.
    strings: [RwLock<HashTable<ThinStr<'static>>>; 16],

    /// The number of strings in the set. Of course, the set is parallel,
    /// so this is a moment-in-time length.
    len: CachePadded<AtomicUsize>,
}


impl StringSet {
    /// Creates a new string set, which initially holds the empty string and
    /// no others.
    pub fn try_new() -> Result<Self, AllocError> {
        // Keep this in the megabyte range. It's virtual, so we do not need
        // to worry much about unused amounts, but asking for wildly too much
        // up front, like in gigabyte+ range, is not good either.
        const SIZE_HINT: usize = 2 * 1024 * 1024;
        let arena = ChainAllocator::new_in(SIZE_HINT, VirtualAllocator {});

        let mut strings = HashTable::new();
        // The initial capacities for Rust's hash map (and set) currently go
        // like this: 3, 7, 14, 28.
        // The smaller values definitely can cause too much reallocation when
        // walking a real stack for the first time, particularly because we'll
        // be holding a write lock while reallocating.
        if strings
            // SAFETY: we just made the empty hash table, so there's nothing that
            // needs to be rehashed.
            .try_reserve(200, |_| unsafe { unreachable_unchecked() })
            .is_err()
        {
            return Err(AllocError);
        }

        const WELL_KNOWN: [ThinStr; 5] = [
            ThinStr::new(),
            ThinStr::end_timestamp_ns(),
            ThinStr::local_root_span_id(),
            ThinStr::trace_endpoint(),
            ThinStr::span_id(),
        ];
        for str in WELL_KNOWN {
            let hash = Hasher::default().hash_one(str);
            strings.insert_unique(hash, str, |t| Hasher::default().hash_one(*t));
        }

        let lock = RwLock::new(StringSetImpl { arena, strings });
        Ok(Self { lock })
    }

    /// Returns the number of strings currently held in the string set.
    #[inline]
    #[allow(clippy::len_without_is_empty, unused)]
    pub fn len(&self) -> usize {
        self.lock.read().strings.len()
    }

    /// Adds the string to the string set if it isn't present already, and
    /// returns a reference to the newly inserted string.
    pub fn insert(&self, str: &str) -> Result<StringId, AllocError> {
        let hash = Hasher::default().hash_one(str);
        let read_lock = self.lock.read();
        let read_len = read_lock.strings.len();
        if let Some(interned_str) = read_lock
            .strings
            .find(hash, |thin_str| thin_str.deref() == str)
        {
            return Ok(StringId(*interned_str));
        }
        drop(read_lock);

        // No match. Acquire the write lock and check the size of the set.
        // The StringSet doesn't have deletes, only inserts, so if the size
        // has changed, then some other thread went before this one, and the
        // string needs to be searched for again.
        let mut write_lock = self.lock.write();
        let write_len = write_lock.strings.len();
        if read_len != write_len {
            if let Some(interned_str) = write_lock
                .strings
                .find(hash, |thin_str| thin_str.deref() == str)
            {
                return Ok(StringId(*interned_str));
            }
        }

        // Still no match. Reserve room in the set, allocate in virtual memory,
        // and insert the new object into the set.
        if write_lock
            .strings
            .try_reserve(1, |thin_str| Hasher::default().hash_one(*thin_str))
            .is_err()
        {
            return Err(AllocError);
        }

        // Make a new string in the arena, and fudge its
        // lifetime to appease the borrow checker.
        let new_str = {
            let obj = ThinStr::try_allocate_for(str, &write_lock.arena)?;
            let uninit = unsafe { &mut *obj.as_ptr() };
            // SAFETY: `try_allocate_for` allocates memory of the
            // right size and layout.
            let s = unsafe { ThinStr::from_str_in_unchecked(str, uninit) };

            // SAFETY: all references to this value get re-narrowed to
            // the lifetime of the string set. The string set will
            // keep the arena alive, making the access safe.
            unsafe { core::mem::transmute::<ThinStr<'_>, ThinStr<'static>>(s) }
        };

        // Add it to the set. The memory was previously reserved.
        // SAFETY: The try_reserve above means any necessary re-hashing has
        // already been done, so the hash closure cannot be called.
        write_lock
            .strings
            .insert_unique(hash, new_str, |_| unsafe { unreachable_unchecked() });

        Ok(StringId(new_str))
    }

    /// Returns the number of bytes used by the arena allocator used to hold
    /// string data. Note that the string set uses more memory than is in the
    /// arena.
    #[inline]
    pub fn arena_used_bytes(&self) -> usize {
        self.lock.read().arena.used_bytes()
    }

    /// Creates a `&str` from the `id`, binding it to the lifetime of
    /// the set.
    ///
    /// # Safety
    /// The `thin_str` must live in this string set.
    #[inline]
    pub unsafe fn get(&self, id: StringId) -> &str {
        // todo: debug_assert it exists in the memory region?
        // SAFETY: see function's safety conditions.
        unsafe { core::mem::transmute(id.0.deref()) }
    }
}
