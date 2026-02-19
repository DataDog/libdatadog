// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared-memory string intern table.
//!
//! Operates on a caller-provided, zero-initialized memory region (e.g. a
//! `MAP_SHARED | MAP_ANONYMOUS` mmap). The table is append-only and never
//! shrinks.
//!
//! # Layout
//!
//! The region is statically partitioned into five sub-regions:
//!
//! ```text
//! +--------+----------+-----------+-----------+-----------------------------+
//! | Header | HT struct| HT data   | Directory |     String Byte Arena       |
//! +--------+----------+-----------+-----------+-----------------------------+
//! ```
//!
//! - **Header**: spinlock, string count, arena bytes used.
//! - **HT struct**: the `hashbrown::HashTable` struct itself, in SHM so it persists across fork and
//!   is shared by all processes.
//! - **HT data**: control bytes + slot data allocated by hashbrown via
//!   [`FixedAllocator`](crate::fixed_allocator::FixedAllocator).
//! - **Directory**: array of `{ offset: u32, len: u32 }` entries indexed by `ShmStringId`.
//! - **String Byte Arena**: append-only byte storage.
//!
//! Pages in the directory and byte arena are lazily faulted by the kernel.
//! No page-size assumptions are made.
//!
//! # Thread / Process Safety
//!
//! - **Reads** (`get`, `len`) are lock-free and use `Acquire` ordering on `string_count` to see a
//!   consistent snapshot.
//! - **Writes** (`intern`) are internally serialized via an atomic spinlock in the header. Safe to
//!   call from multiple processes/threads.

use crate::fixed_allocator::FixedAllocator;
use crate::string_id::ShmStringId;
use core::mem;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use hashbrown::HashTable;
use std::io;

// ---------------------------------------------------------------------------
// Type alias for the hash table stored in SHM.
// ---------------------------------------------------------------------------

type ShmHashTable = HashTable<u32, FixedAllocator>;

// ---------------------------------------------------------------------------
// Constants -- all sizing is derived from these, easy to adjust.
// ---------------------------------------------------------------------------

/// Total mmap reservation for the SHM string table.
pub const SHM_REGION_SIZE: usize = 6 * 1024 * 1024; // 6 MiB

/// Maximum interned strings. hashbrown at 7/8 load factor over 2^16 slots
/// supports exactly this many items.
pub const SHM_MAX_STRINGS: usize = (1_usize << 16) * 7 / 8; // 57,344

/// Size of the header sub-region (lock + counters + padding).
const HEADER_SIZE: usize = 64;

/// Offset of the `HashTable` struct within the SHM region.
const HT_STRUCT_OFFSET: usize = HEADER_SIZE;

/// Size of the `HashTable` struct.
const HT_STRUCT_SIZE: usize = mem::size_of::<ShmHashTable>();

/// Offset of the hash data region (control bytes + slots), rounded up to
/// 64-byte alignment for cache-friendliness.
const HT_DATA_OFFSET: usize = (HT_STRUCT_OFFSET + HT_STRUCT_SIZE + 63) & !63;

/// Size of the hash data region. hashbrown needs approximately
/// `capacity * (1 + sizeof(T)) + 16` bytes. With capacity 65,536 and
/// T = u32 (4 bytes): ~327,696 bytes. We use 384 KiB for headroom.
const HT_DATA_SIZE: usize = 384 * 1024;

/// Each directory entry: { offset: u32, len: u32 } = 8 bytes.
const DIR_ENTRY_SIZE: usize = 8;

/// Start of the string directory.
const DIR_OFFSET: usize = HT_DATA_OFFSET + HT_DATA_SIZE;

/// Size of the string directory.
const DIRECTORY_REGION_SIZE: usize = SHM_MAX_STRINGS * DIR_ENTRY_SIZE;

/// Start of the string byte arena.
const ARENA_OFFSET: usize = DIR_OFFSET + DIRECTORY_REGION_SIZE;

// Compile-time checks.
const _: () = assert!(
    ARENA_OFFSET < SHM_REGION_SIZE,
    "sub-regions exceed SHM_REGION_SIZE"
);
const _: () = assert!(
    HT_STRUCT_OFFSET % mem::align_of::<ShmHashTable>() == 0,
    "HashTable struct offset is not properly aligned"
);

/// Available bytes in the string byte arena.
const ARENA_SIZE: usize = SHM_REGION_SIZE - ARENA_OFFSET;

// ---------------------------------------------------------------------------
// Well-known strings -- pre-interned during init for stable, low indices.
// Matches libdatadog's WELL_KNOWN_STRING_REFS ordering.
// ---------------------------------------------------------------------------

/// Strings pre-interned by [`ShmStringTable::init`], in order, starting at
/// index 0.
const WELL_KNOWN_STRINGS: [&str; 7] = [
    "",
    "end_timestamp_ns",
    "local root span id",
    "trace endpoint",
    "span id",
    "thread id",
    "thread name",
];

// ---------------------------------------------------------------------------
// Header (lives at offset 0 in the region)
// ---------------------------------------------------------------------------

/// Offsets within the header. Accessed as `AtomicU32` via pointer arithmetic.
mod header {
    /// Spinlock: 0 = unlocked, 1 = locked.
    pub const LOCK_OFFSET: usize = 0;
    /// Number of interned strings (also the next `ShmStringId` to assign).
    pub const STRING_COUNT_OFFSET: usize = 4;
    /// Bytes consumed in the byte arena.
    pub const ARENA_USED_OFFSET: usize = 8;
}

// ---------------------------------------------------------------------------
// Directory entry (conceptually `{ offset: u32, len: u32 }`)
// ---------------------------------------------------------------------------

/// Reads a directory entry.
///
/// # Safety
/// `dir_base` must point to a valid directory region, and `index` must be
/// less than the current `string_count`.
#[inline]
unsafe fn dir_read(dir_base: *const u8, index: u32) -> (u32, u32) {
    let entry_ptr = dir_base.add(index as usize * DIR_ENTRY_SIZE);
    let offset = core::ptr::read(entry_ptr as *const u32);
    let len = core::ptr::read(entry_ptr.add(4) as *const u32);
    (offset, len)
}

/// Writes a directory entry.
///
/// # Safety
/// `dir_base` must point to a valid, writable directory region, and `index`
/// must be less than `SHM_MAX_STRINGS`.
#[inline]
unsafe fn dir_write(dir_base: *mut u8, index: u32, offset: u32, len: u32) {
    let entry_ptr = dir_base.add(index as usize * DIR_ENTRY_SIZE);
    core::ptr::write(entry_ptr as *mut u32, offset);
    core::ptr::write(entry_ptr.add(4) as *mut u32, len);
}

// ---------------------------------------------------------------------------
// ShmStringTable
// ---------------------------------------------------------------------------

/// A string intern table backed by a caller-provided shared memory region.
///
/// The table is append-only: strings are never removed or modified. Readers
/// can safely access strings concurrently with writers (the writer bumps
/// `string_count` with `Release` ordering after all data is written;
/// readers load it with `Acquire`).
///
/// Writes are internally serialized via an atomic spinlock in the header.
pub struct ShmStringTable {
    /// Pointer to the start of the full region.
    pub(crate) base: NonNull<u8>,
}

// SAFETY: The underlying memory is a shared mapping. Reads use Acquire
// ordering on string_count. Writes are serialized by the internal atomic
// spinlock. The only interior mutability is through atomics and the
// spinlock-protected write path.
unsafe impl Send for ShmStringTable {}
unsafe impl Sync for ShmStringTable {}

impl ShmStringTable {
    // -- Well-known string IDs (stable across all tables) -------------------

    /// The empty string, always index 0 (from zero-initialized memory).
    pub const EMPTY: ShmStringId = ShmStringId::new_const(0);
    /// `"end_timestamp_ns"`, always index 1.
    pub const END_TIMESTAMP_NS: ShmStringId = ShmStringId::new_const(1);
    /// `"local root span id"`, always index 2.
    pub const LOCAL_ROOT_SPAN_ID: ShmStringId = ShmStringId::new_const(2);
    /// `"trace endpoint"`, always index 3.
    pub const TRACE_ENDPOINT: ShmStringId = ShmStringId::new_const(3);
    /// `"span id"`, always index 4.
    pub const SPAN_ID: ShmStringId = ShmStringId::new_const(4);
    /// `"thread id"`, always index 5.
    pub const THREAD_ID: ShmStringId = ShmStringId::new_const(5);
    /// `"thread name"`, always index 6.
    pub const THREAD_NAME: ShmStringId = ShmStringId::new_const(6);

    // -- Sub-region pointers ------------------------------------------------

    #[inline]
    fn header_ptr(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    #[inline]
    fn lock(&self) -> &AtomicU32 {
        unsafe { &*(self.header_ptr().add(header::LOCK_OFFSET) as *const AtomicU32) }
    }

    #[inline]
    fn string_count(&self) -> &AtomicU32 {
        unsafe { &*(self.header_ptr().add(header::STRING_COUNT_OFFSET) as *const AtomicU32) }
    }

    #[inline]
    fn arena_used(&self) -> &AtomicU32 {
        unsafe { &*(self.header_ptr().add(header::ARENA_USED_OFFSET) as *const AtomicU32) }
    }

    /// Returns a pointer to the `HashTable` struct in SHM.
    /// Caller may create a mutable reference only while holding the spinlock.
    ///
    /// # Safety
    /// Must only be called while the spinlock is held, or during single-
    /// threaded initialization. The returned pointer must not be used to form
    /// a long-lived `&mut` that outlives the lock.
    #[inline]
    unsafe fn hash_table(&self) -> *mut ShmHashTable {
        self.base.as_ptr().add(HT_STRUCT_OFFSET) as *mut ShmHashTable
    }

    #[inline]
    fn dir_base(&self) -> *mut u8 {
        unsafe { self.base.as_ptr().add(DIR_OFFSET) }
    }

    #[inline]
    fn arena_base(&self) -> *mut u8 {
        unsafe { self.base.as_ptr().add(ARENA_OFFSET) }
    }

    // -- Spinlock -----------------------------------------------------------

    #[inline]
    fn spin_lock(&self) {
        loop {
            match self
                .lock()
                .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(_) => core::hint::spin_loop(),
            }
        }
    }

    #[inline]
    fn spin_unlock(&self) {
        self.lock().store(0, Ordering::Release);
    }

    // -- Hash helpers -------------------------------------------------------

    /// Hashes a string using FNV-1a (deterministic). We need a deterministic
    /// hash because the hash table is in SHM, shared across processes.
    /// Rust's default hasher is randomized per process.
    #[inline]
    fn hash_str(s: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in s {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Reads the string bytes for a given directory index.
    ///
    /// # Safety
    /// `index` must be < current string_count, and the directory/arena must
    /// be properly populated for that index.
    #[inline]
    unsafe fn read_string_bytes(&self, index: u32) -> &[u8] {
        let (offset, len) = dir_read(self.dir_base(), index);
        let ptr = self.arena_base().add(offset as usize);
        core::slice::from_raw_parts(ptr, len as usize)
    }

    // -- FFI helpers --------------------------------------------------------

    /// Panic-free init for the FFI layer. Returns `None` on any error.
    ///
    /// # Safety
    /// Same requirements as [`init`].
    #[cfg(feature = "ffi")]
    #[inline(always)]
    pub(crate) unsafe fn init_ffi(region_ptr: *mut u8, region_len: usize) -> Option<Self> {
        if region_ptr.is_null() || region_len < SHM_REGION_SIZE {
            return None;
        }

        let base = NonNull::new(region_ptr)?;
        let table = Self { base };

        // Zero the header.
        core::ptr::write_bytes(table.header_ptr(), 0, HEADER_SIZE);

        // Construct the HashTable with a FixedAllocator.
        let ht_data_ptr = NonNull::new(base.as_ptr().add(HT_DATA_OFFSET))?;
        let alloc = FixedAllocator::new(ht_data_ptr, HT_DATA_SIZE);
        let ht: ShmHashTable = HashTable::try_with_capacity_in(SHM_MAX_STRINGS, alloc).ok()?;

        // Write the HashTable struct into its SHM slot.
        let ht_dest = base.as_ptr().add(HT_STRUCT_OFFSET) as *mut ShmHashTable;
        core::ptr::write(ht_dest, ht);

        // Pre-intern well-known strings so they get stable, low indices.
        for s in WELL_KNOWN_STRINGS {
            let bytes = s.as_bytes();
            let hash = Self::hash_str(bytes);
            table.spin_lock();
            let result = table.intern_locked(bytes, hash);
            table.spin_unlock();
            result?;
        }

        Some(table)
    }

    // -- Public API ---------------------------------------------------------

    /// Initialize a new table in the given memory region.
    ///
    /// Constructs the `hashbrown::HashTable` in the SHM region and
    /// pre-reserves capacity for [`SHM_MAX_STRINGS`] entries.
    ///
    /// # Safety
    /// - `region` must point to a valid, zero-initialized, writable memory region of at least
    ///   [`SHM_REGION_SIZE`] bytes (e.g. a fresh `mmap(MAP_SHARED | MAP_ANONYMOUS)` mapping).
    /// - The region must remain valid and mapped for the lifetime of the returned `ShmStringTable`
    ///   and any `ShmStringId`s produced from it.
    /// - The caller must not concurrently initialize the same region.
    ///
    /// # Region lifetime
    /// This crate does not take ownership of the region. The **caller** is responsible for
    /// unmapping/freeing it (e.g. `munmap`) after all users of the table and any derived
    /// `ShmStringId`s are done.
    pub unsafe fn init(region: NonNull<[u8]>) -> io::Result<Self> {
        if region.len() < SHM_REGION_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "region too small for ShmStringTable",
            ));
        }

        let base = NonNull::new(region.as_ptr() as *mut u8)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "null region pointer"))?;

        let table = Self { base };

        // Zero the header.
        core::ptr::write_bytes(table.header_ptr(), 0, HEADER_SIZE);

        // Construct the HashTable with a FixedAllocator pointing at the
        // hash data sub-region.
        let ht_data_ptr = NonNull::new(base.as_ptr().add(HT_DATA_OFFSET))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "null hash data pointer"))?;
        let alloc = FixedAllocator::new(ht_data_ptr, HT_DATA_SIZE);
        let ht: ShmHashTable = HashTable::try_with_capacity_in(SHM_MAX_STRINGS, alloc)
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;

        // Write the HashTable struct into its SHM slot.
        let ht_dest = base.as_ptr().add(HT_STRUCT_OFFSET) as *mut ShmHashTable;
        core::ptr::write(ht_dest, ht);
        // Do NOT drop the local `ht` -- ownership has moved to SHM via
        // ptr::write. The local was consumed by write (it takes by value for
        // Copy types, but HashTable is not Copy, so `ht` is moved out and
        // the compiler won't drop it).

        // Pre-intern well-known strings so they get stable, low indices.
        for s in WELL_KNOWN_STRINGS {
            table.intern(s)?;
        }

        Ok(table)
    }

    /// Intern a string, returning the existing id if already present, or a
    /// new id if this is the first time.
    ///
    /// Returns `Err(io::ErrorKind::StorageFull)` if the table is at capacity
    /// (directory full or arena full).
    ///
    /// Internally serialized via the atomic spinlock -- safe to call from
    /// multiple processes/threads concurrently.
    pub fn intern(&self, s: &str) -> io::Result<ShmStringId> {
        let bytes = s.as_bytes();
        let hash = Self::hash_str(bytes);

        self.spin_lock();
        // SAFETY: we hold the spinlock, so we have exclusive write access.
        let result = unsafe { self.intern_locked(bytes, hash) };
        self.spin_unlock();

        result.ok_or_else(|| io::Error::from(io::ErrorKind::StorageFull))
    }

    /// FFI-friendly intern that takes raw bytes (must be valid UTF-8) and
    /// returns the `ShmStringId` index as `i64`, or `-1` on error.
    /// Avoids `io::Result` to stay within the no-panic subset.
    #[cfg(feature = "ffi")]
    #[inline(always)]
    pub(crate) fn intern_ffi(&self, bytes: &[u8]) -> i64 {
        let hash = Self::hash_str(bytes);
        self.spin_lock();
        let result = unsafe { self.intern_locked(bytes, hash) };
        self.spin_unlock();
        match result {
            Some(id) => id.index() as i64,
            None => -1,
        }
    }

    /// The actual intern logic, called while holding the spinlock.
    /// Returns `None` on capacity/overflow errors.
    ///
    /// # Safety
    /// Must only be called while the spinlock is held.
    unsafe fn intern_locked(&self, bytes: &[u8], hash: u64) -> Option<ShmStringId> {
        let ht = &mut *self.hash_table();
        let count = self.string_count().load(Ordering::Relaxed);

        // Probe the hash table for an existing entry.
        if let Some(&existing_index) = ht.find(hash, |&idx| self.read_string_bytes(idx) == bytes) {
            return ShmStringId::new(existing_index);
        }

        // Not found -- insert a new entry.

        // Check directory capacity.
        if count as usize >= SHM_MAX_STRINGS {
            return None;
        }

        // Append bytes to arena.
        let arena_used = self.arena_used().load(Ordering::Relaxed);
        let new_arena_used = arena_used as usize + bytes.len();
        if new_arena_used > ARENA_SIZE {
            return None;
        }

        // Write string bytes to arena.
        let arena_ptr = self.arena_base().add(arena_used as usize);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), arena_ptr, bytes.len());

        // Write directory entry.
        dir_write(self.dir_base(), count, arena_used, bytes.len() as u32);

        // Use the fallible insert to avoid the panic path in
        // insert_unique's internal reserve logic.
        if ht.try_insert_unique_within_capacity(hash, count).is_err() {
            return None;
        }

        // Update arena_used.
        self.arena_used()
            .store(new_arena_used as u32, Ordering::Relaxed);

        // Bump string_count with Release so readers see all prior writes.
        self.string_count().store(count + 1, Ordering::Release);

        ShmStringId::new(count)
    }

    /// Look up a string by id.
    ///
    /// Returns the empty string if `id` is out of bounds (defensive -- should
    /// not happen with valid ids from [`intern`]).
    pub fn get(&self, id: ShmStringId) -> &str {
        let index = id.index();
        let count = self.string_count().load(Ordering::Acquire);

        if index >= count {
            return "";
        }

        // SAFETY: index < count, and the writer used Release on string_count
        // after writing all data for this index.
        let bytes = unsafe { self.read_string_bytes(index) };

        // We only intern valid UTF-8 (from &str), so this is always valid.
        unsafe { core::str::from_utf8_unchecked(bytes) }
    }

    /// Current number of interned strings (Acquire load).
    ///
    /// Includes the reserved index 0 (empty string), so this is always >= 1
    /// after initialization.
    #[inline]
    pub fn len(&self) -> u32 {
        self.string_count().load(Ordering::Acquire)
    }

    /// Returns `true` if the table contains only the reserved empty string.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a heap-allocated, zero-initialized, 64-byte-aligned
    /// buffer and return a `NonNull<[u8]>` suitable for `ShmStringTable::init`.
    ///
    /// Uses `Vec<u64>` to guarantee at least 8-byte alignment (matching what
    /// mmap provides in production). The 64-byte header size ensures all
    /// sub-region offsets are well-aligned.
    fn make_region(size: usize) -> (Vec<u64>, NonNull<[u8]>) {
        let u64_count = (size + 7) / 8;
        let mut buf = vec![0u64; u64_count];
        let ptr = NonNull::new(buf.as_mut_ptr() as *mut u8).unwrap();
        let slice = NonNull::slice_from_raw_parts(ptr, u64_count * 8);
        (buf, slice)
    }

    #[test]
    fn init_and_len() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };
        // 7 well-known strings (including "").
        assert_eq!(table.len(), 7);
        assert!(!table.is_empty());
    }

    #[test]
    fn well_known_strings() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        assert_eq!(table.get(ShmStringTable::EMPTY), "");
        assert_eq!(
            table.get(ShmStringTable::END_TIMESTAMP_NS),
            "end_timestamp_ns"
        );
        assert_eq!(
            table.get(ShmStringTable::LOCAL_ROOT_SPAN_ID),
            "local root span id"
        );
        assert_eq!(table.get(ShmStringTable::TRACE_ENDPOINT), "trace endpoint");
        assert_eq!(table.get(ShmStringTable::SPAN_ID), "span id");
        assert_eq!(table.get(ShmStringTable::THREAD_ID), "thread id");
        assert_eq!(table.get(ShmStringTable::THREAD_NAME), "thread name");

        // Interning a well-known string deduplicates to the pre-assigned id.
        let id = table.intern("trace endpoint").unwrap();
        assert_eq!(id, ShmStringTable::TRACE_ENDPOINT);
        let id = table.intern("thread id").unwrap();
        assert_eq!(id, ShmStringTable::THREAD_ID);
    }

    #[test]
    fn region_too_small() {
        let (_buf, region) = make_region(1024);
        let result = unsafe { ShmStringTable::init(region) };
        assert!(result.is_err());
    }

    #[test]
    fn intern_and_get() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };
        let base = table.len();

        let id = table.intern("hello").unwrap();
        assert_eq!(table.get(id), "hello");
        assert_eq!(table.len(), base + 1);
    }

    #[test]
    fn intern_deduplicates() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };
        let base = table.len();

        let id1 = table.intern("hello").unwrap();
        let id2 = table.intern("hello").unwrap();
        assert_eq!(id1, id2);
        assert_eq!(table.len(), base + 1);
    }

    #[test]
    fn intern_distinct_strings() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };
        let base = table.len();

        let id_a = table.intern("alpha").unwrap();
        let id_b = table.intern("beta").unwrap();
        let id_c = table.intern("gamma").unwrap();

        assert_ne!(id_a, id_b);
        assert_ne!(id_b, id_c);
        assert_ne!(id_a, id_c);

        assert_eq!(table.get(id_a), "alpha");
        assert_eq!(table.get(id_b), "beta");
        assert_eq!(table.get(id_c), "gamma");
        assert_eq!(table.len(), base + 3);
    }

    #[test]
    fn empty_string_is_index_zero() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        // Index 0 should resolve to empty string.
        let id0 = ShmStringId::new(0).unwrap();
        assert_eq!(table.get(id0), "");
    }

    #[test]
    fn intern_empty_string() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        // Interning "" should deduplicate against the reserved index 0
        // or produce a new entry that also resolves to "".
        let id = table.intern("").unwrap();
        assert_eq!(table.get(id), "");
    }

    #[test]
    fn get_out_of_bounds_returns_empty() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        let bad_id = ShmStringId::new(99999).unwrap();
        assert_eq!(table.get(bad_id), "");
    }

    #[test]
    fn second_handle_sees_existing_data() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        let id_hello = table.intern("hello").unwrap();
        let id_world = table.intern("world").unwrap();

        // After fork, the child inherits the same ShmStringTable value.
        // Simulate this by just copying the struct (same base pointer,
        // same shared memory).
        let table2 = ShmStringTable { base: table.base };
        assert_eq!(table2.get(id_hello), "hello");
        assert_eq!(table2.get(id_world), "world");
        assert_eq!(table2.len(), table.len());
    }

    #[test]
    fn intern_many_strings() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };
        let base = table.len();

        #[cfg(not(miri))]
        let n = 1024;
        #[cfg(miri)]
        let n = 512;
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            let s = format!("string_{:04}", i);
            ids.push(table.intern(&s).unwrap());
        }

        // Verify all lookups.
        for (i, id) in ids.iter().enumerate() {
            let expected = format!("string_{:04}", i);
            assert_eq!(table.get(*id), expected);
        }

        // Verify dedup.
        for i in 0..n {
            let s = format!("string_{:04}", i);
            let id2 = table.intern(&s).unwrap();
            assert_eq!(id2, ids[i]);
        }

        assert_eq!(table.len(), n as u32 + base);
    }

    #[test]
    fn intern_utf8() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        let id = table.intern("héllo wörld 日本語").unwrap();
        assert_eq!(table.get(id), "héllo wörld 日本語");
    }

    #[test]
    fn constants_are_consistent() {
        assert!(SHM_MAX_STRINGS > 0);
        assert!(ARENA_OFFSET < SHM_REGION_SIZE);
        assert!(ARENA_SIZE > 0);
        // Arena should be the bulk of the region.
        assert!(ARENA_SIZE > SHM_REGION_SIZE / 2);
        // HT struct must fit before HT data.
        assert!(HT_STRUCT_OFFSET + HT_STRUCT_SIZE <= HT_DATA_OFFSET);
    }

    // -- Fuzz tests -----------------------------------------------------------

    /// Fuzz: intern a sequence of arbitrary strings, verify every id round-trips
    /// through `get`, and confirm deduplication holds.
    #[test]
    fn fuzz_intern_get_roundtrip() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        bolero::check!()
            .with_type::<Vec<String>>()
            .for_each(|strings| {
                let mut seen = std::collections::HashMap::<String, ShmStringId>::new();

                for s in strings {
                    match table.intern(s) {
                        Ok(id) => {
                            // Round-trip: get must return the original string.
                            assert_eq!(table.get(id), s.as_str());

                            // Dedup: same string must yield the same id.
                            if let Some(&prev_id) = seen.get(s) {
                                assert_eq!(id, prev_id);
                            }
                            seen.insert(s.clone(), id);
                        }
                        Err(e) => {
                            // StorageFull is the only acceptable error.
                            assert_eq!(e.kind(), io::ErrorKind::StorageFull);
                        }
                    }
                }

                // All previously interned strings must still be retrievable.
                for (s, id) in &seen {
                    assert_eq!(table.get(*id), s.as_str());
                }
            });
    }

    /// Fuzz: `get` with arbitrary `ShmStringId` values never panics or
    /// returns garbage -- it returns either a valid interned string or "".
    #[test]
    fn fuzz_get_arbitrary_ids() {
        let (_buf, region) = make_region(SHM_REGION_SIZE);
        let table = unsafe { ShmStringTable::init(region).unwrap() };

        // Intern a few strings so there's something to hit.
        let _ = table.intern("hello");
        let _ = table.intern("world");

        bolero::check!().with_type::<u32>().for_each(|&raw_id| {
            if let Some(id) = ShmStringId::new(raw_id) {
                let s = table.get(id);
                // Must be valid UTF-8 (it's a &str).
                // Out-of-bounds returns "".
                if raw_id >= table.len() {
                    assert_eq!(s, "");
                }
            }
        });
    }
}
