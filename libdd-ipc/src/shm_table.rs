// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free shared-memory aggregation table, generic over what its slots hold.
//!
//! All PHP worker processes open the same SHM file and add inputs to it with
//! [`ShmTable::add`]. The sidecar is the creator ([`ShmTable::create`]) and periodically drains
//! the inactive bucket. What a slot holds - its key, its value, and how an input is hashed,
//! matched and recorded - is a [`Schema`]; everything else here is shared by every table.
//!
//! ## SHM layout
//! ```text
//! [0 .. PAGE_SIZE)               ShmHeader  (one page)
//! [PAGE_SIZE .. PAGE_SIZE+R)     bucket 0 region
//! [PAGE_SIZE+R .. PAGE_SIZE+2R)  bucket 1 region
//!
//! Each bucket region (size R):
//!   [0 .. HDR_SIZE)              ShmBucketHeader
//!   [HDR_SIZE .. HDR_SIZE+S*E)   ShmEntry array  (S = slot_count, E = entry_size)
//!   [HDR_SIZE+S*E .. R)          string pool     (bump-allocated by writers)
//! ```
//!
//! ## Untrusted contents
//! Every byte of the segment is writable by every process that holds it, workers included, so
//! nothing read out of it may authorise an access. The dimensions are validated once against
//! the length actually mapped and then kept privately ([`Layout`]); string references are
//! checked against their own pool and copied out under a per-bucket budget; mutable fields are
//! integer atomics that get decoded rather than loaded as Rust types. A peer can corrupt its
//! own telemetry - that much is inherent in a buffer everyone may write - but it cannot make
//! either side reach outside the mapping, panic, or export bytes it never put in the pool.
//!
//! ## Schemas
//! The header records which [`Schema`] (and which version of it) laid out the slots, and an
//! opener refuses a segment of any other: the slot size it validates against would otherwise be
//! its own, not the creator's.
//!
//! ## Slot lifecycle
//! ```text
//! key_hash == SLOT_EMPTY (0)       → slot is free
//! key_hash == SLOT_INIT (u64::MAX) → slot is being claimed/written
//! key_hash == H (any other)        → slot is ready, key hash is H
//! ```
//!
//! A writer CAS(0→MAX) to claim, writes key + strings (no concurrent readers
//! yet), issues a **Release** fence, then stores `key_hash = H` (Release).
//!
//! ## Double-buffering
//! `ShmHeader::active_idx` tells which bucket PHP workers write to.
//! The sidecar swaps it, waits for `in_flight` to reach 0, then reads + clears.
//!
//! ## Table growth
//! When the active bucket is nearly full the sidecar:
//! 1. Creates a new SHM file at the *same path* (the old file is unlinked from the filesystem but
//!    remains accessible to processes that already have it open).
//! 2. Sets `ShmHeader::ready = 0` on the **old** mapping so workers know to re-open the path on
//!    their next `add` call.
//! 3. Holds onto the old table for ≥ 1 s, flushing it periodically, to absorb any inputs that
//!    arrived before workers noticed the reload flag.
//! 4. Drops the old table after that grace period.

use arc_swap::ArcSwapOption;
use std::ffi::{CStr, CString};
use std::hint;
use std::io;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering::*, fence};
use std::thread;

use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
pub(crate) use crate::rate_limiter::ShmSafe;

/// Version of everything here that is not a schema's: the header and the bucket regions.
pub(crate) const SHM_VERSION: u32 = 4;

pub(crate) const SLOT_EMPTY: u64 = 0;
pub(crate) const SLOT_INIT: u64 = u64::MAX;

/// Slot utilisation of the active bucket above which a table counts as nearly full.
///
/// Today this only triggers a warning. Once per flush interval the sidecar's stats flusher
/// compares `slot_usage()` for the active bucket against this ratio and, above it, logs that the
/// table is nearly full and suggests a larger slot count. Nothing else happens. The table is not
/// recreated or grown, since the "Table growth" steps in the module docs are not implemented, and
/// the sidecar always creates tables with `DEFAULT_SLOT_COUNT` slots, so acting on the warning
/// needs a code change. A bucket that does fill up keeps recording inputs for the keys it already
/// holds and silently drops inputs with new keys until the next bucket swap.
pub const RELOAD_FILL_RATIO: f64 = 0.80;

/// Max iterations when waiting for `in_flight` to reach zero (~100 µs).
const MAX_FLUSH_WAIT_ITERS: u32 = 100_000;
/// Spin iterations before yielding to the OS scheduler.
const YIELD_AFTER_SPINS: u32 = 8;
/// Max iterations `add` spends waiting on slots that are marked as being written. One
/// budget for the whole call, so the worst case does not scale with the table's size.
const MAX_SLOT_WAIT_ITERS: u32 = 10_000;

/// What the slots of one kind of table hold, and how inputs reach them.
///
/// `Key` and `Value` live in a mapping every peer can write, and are reached through references
/// made straight from its bytes, starting out zeroed. [`ShmSafe`] is what makes that sound, so
/// implementing a schema needs no `unsafe` of its own. Both should also be `#[repr(C)]`, so that
/// their layout is the same in every process that maps the segment. Anything read out of them is
/// a peer's to have written, and is decoded and bounds-checked rather than trusted.
pub trait Schema: 'static {
    /// Identifies the schema in the header, so that a segment is only opened as its own kind.
    const ID: u32;
    /// Bumped whenever `Key`, `Value`, or what is stored in them changes.
    const VERSION: u32;

    type Key: ShmSafe;
    type Value: ShmSafe;
    type Input<'a>;

    /// Hash of the input's key. Any value is fine; the table remaps the two it reserves.
    fn hash(input: &Self::Input<'_>) -> u64;

    /// Write the input's key into a slot the caller has just claimed.
    fn write_key(key: &Self::Key, pool: &Pool, cursor: &AtomicU32, input: &Self::Input<'_>);

    /// Does this slot hold the input's key? A key that does not decode never matches.
    fn key_matches(key: &Self::Key, pool: &Pool, input: &Self::Input<'_>) -> bool;

    /// Fold the input into the slot's value.
    fn record(value: &Self::Value, input: &Self::Input<'_>);
}

/// Byte range inside a bump-allocated string pool (offset relative to pool start).
///
/// A snapshot held in private memory: both fields have already been read out of the mapping
/// and nothing re-reads them, so a range that has been checked cannot move afterwards.
/// `offset == 0 && len == 0` represents an absent / empty string.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct StringRef {
    pub offset: u32,
    pub len: u32,
}

/// A [`StringRef`] as it is stored in the mapping.
///
/// Read only through [`WireStringRef::snapshot`]: a bounds check is worth nothing unless the
/// value it checked is the one that gets used, and these two words can change at any time.
#[repr(C)]
pub(crate) struct WireStringRef {
    pub(crate) offset: AtomicU32,
    pub(crate) len: AtomicU32,
}

// SAFETY: `#[repr(C)]` and made only of integer atomics.
unsafe impl ShmSafe for WireStringRef {}

impl WireStringRef {
    pub(crate) fn snapshot(&self) -> StringRef {
        StringRef {
            offset: self.offset.load(Relaxed),
            len: self.len.load(Relaxed),
        }
    }

    pub(crate) fn store(&self, sr: StringRef) {
        self.offset.store(sr.offset, Relaxed);
        self.len.store(sr.len, Relaxed);
    }
}

/// One slot in the hash table.
#[repr(C)]
pub(crate) struct ShmEntry<S: Schema> {
    pub(crate) key_hash: AtomicU64,
    pub(crate) key: S::Key,
    pub(crate) value: S::Value,
}

/// Per-bucket control header.
#[repr(C)]
pub(crate) struct ShmBucketHeader {
    pub(crate) start_nanos: AtomicU64,
    pub(crate) in_flight: AtomicI64,
    pub(crate) string_cursor: AtomicU32,
}

/// Global SHM header (first page of the mapping).
///
/// Only [`ShmHeader::version`] and the schema fields are read for their own sake; the dimensions
/// are snapshotted once by [`ShmTable::open`] and thereafter live in a private [`Layout`]. They
/// stay in the mapping because an opener has nowhere else to learn them from, not because
/// anything keeps consulting them.
#[repr(C)]
pub(crate) struct ShmHeader {
    /// Layout version; checked by [`ShmTable::open`].  Mismatch returns an error.
    ///
    /// First field, and a `u32` in every version, so that a client of any vintage can read it
    /// before it knows anything else about the layout: that is what stops an older client from
    /// interpreting a newer segment as its own format.
    pub(crate) version: AtomicU32,
    /// Set to 1 by the sidecar when workers should re-open the SHM at the
    /// same path (a new, larger mapping has been created there).
    pub(crate) ready: AtomicU8,
    /// Index of the bucket currently being written to by PHP workers; only its low bit counts.
    pub(crate) active_idx: AtomicU8,
    /// [`Schema::ID`] of the schema that laid out the slots; checked by [`ShmTable::open`].
    pub(crate) schema_id: AtomicU32,
    /// [`Schema::VERSION`] of that schema; checked by [`ShmTable::open`].
    pub(crate) schema_version: AtomicU32,
    /// Width of each time bucket in nanoseconds (e.g. 10 s = 10_000_000_000).
    pub(crate) bucket_size_nanos: AtomicU64,
    /// Number of aggregation slots per bucket (hash-table capacity).
    pub(crate) slot_count: AtomicU32,
    /// Byte size of one full bucket region (header + slots + string pool), page-aligned.
    pub(crate) bucket_region_size: AtomicU32,
    /// Byte capacity of the per-bucket string pool.
    pub(crate) string_pool_size: AtomicU32,
    /// Monotonic counter incremented on every successful flush, used as the stats sequence number.
    pub(crate) flush_seq: AtomicU64,
}

/// Where `ready` is in the header of every version, so that a creator can retire a segment of
/// any vintage without trusting the rest of its layout.
const READY_OFFSET: usize = 4;

// Peers of other versions find these two fields by position, so they must never move.
const _: () = assert!(std::mem::offset_of!(ShmHeader, version) == 0);
const _: () = assert!(std::mem::offset_of!(ShmHeader, ready) == READY_OFFSET);

/// Where everything in the mapping is, kept in private memory.
///
/// Every field here also exists in [`ShmHeader`]. The creator takes them from its own
/// allocation parameters; an opener snapshots the header once and accepts it only if it
/// describes a segment fitting the length it mapped. No address, loop bound or divisor is
/// recomputed from the header afterwards - `slot_count` reads like configuration, but the
/// flush loop's bound and its slot addresses both come from it.
pub(crate) struct Layout<S> {
    /// Nonzero: flushing divides by it.
    pub(crate) bucket_size_nanos: u64,
    /// Nonzero: [`ShmTable::add`] takes a remainder by it.
    pub(crate) slot_count: u32,
    pub(crate) bucket_region_size: u32,
    pub(crate) string_pool_size: u32,
    /// Offset of bucket 0; bucket 1 begins `bucket_region_size` later.
    pub(crate) bucket0_start: usize,
    /// Offset of the entry array within a bucket region.
    pub(crate) entries_offset: usize,
    /// Offset of the string pool within a bucket region.
    pub(crate) pool_offset: usize,
    /// The entry size and alignment every offset above was computed for.
    _schema: PhantomData<fn() -> S>,
}

impl<S> Clone for Layout<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for Layout<S> {}

impl<S: Schema> Layout<S> {
    /// Accept these dimensions for a mapping of `mapped_len` bytes, or reject them.
    ///
    /// `mapped_len` is the length this process passed to `mmap`, not anything read out of the
    /// segment, which is what makes it a usable bound: a peer can grow or shrink the backing
    /// file but cannot change how much of it we mapped.
    pub(crate) fn new(
        bucket_size_nanos: u64,
        slot_count: u32,
        string_pool_size: u32,
        bucket_region_size: u32,
        mapped_len: usize,
    ) -> Option<Self> {
        let bucket0_start = page_size::get();
        let entries_offset = bucket_hdr_size();
        let pool_offset = entries_offset
            .checked_add((slot_count as usize).checked_mul(size_of::<ShmEntry<S>>())?)?;
        let needed = pool_offset.checked_add(string_pool_size as usize)?;
        let total = bucket0_start.checked_add((bucket_region_size as usize).checked_mul(2)?)?;

        if bucket_size_nanos == 0
            || slot_count == 0
            // The header shares bucket 0's first page with nothing else.
            || bucket0_start < size_of::<ShmHeader>()
            // Bucket 1 starts one region in, so a region size that is not a multiple of the
            // entry alignment would leave its slots misaligned - and a misaligned atomic
            // access is undefined behaviour, not a slow one.
            || !(bucket_region_size as usize).is_multiple_of(align_of::<ShmEntry<S>>())
            // Each region starts on a multiple of `bucket_hdr_size`'s 8-byte rounding, which
            // only lines the entries up for schemas whose alignment divides it.
            || !8usize.is_multiple_of(align_of::<ShmEntry<S>>())
            || (bucket_region_size as usize) < needed
            || total > mapped_len
        {
            return None;
        }

        Some(Layout {
            bucket_size_nanos,
            slot_count,
            bucket_region_size,
            string_pool_size,
            bucket0_start,
            entries_offset,
            pool_offset,
            _schema: PhantomData,
        })
    }

    /// The layout of a segment created with these dimensions, and the size to map for it.
    pub(crate) fn for_new(
        bucket_size_nanos: u64,
        slot_count: u32,
        string_pool_size: u32,
    ) -> Option<(Self, usize)> {
        let raw = bucket_hdr_size()
            .checked_add((slot_count as usize).checked_mul(size_of::<ShmEntry<S>>())?)?
            .checked_add(string_pool_size as usize)?;
        let page = page_size::get();
        let region = u32::try_from(raw.div_ceil(page).checked_mul(page)?).ok()?;
        let total = page.checked_add((region as usize).checked_mul(2)?)?;
        let layout = Self::new(
            bucket_size_nanos,
            slot_count,
            string_pool_size,
            region,
            total,
        )?;
        Some((layout, total))
    }
}

/// One bucket region, resolved against the private [`Layout`].
pub(crate) struct Bucket<'a, S: Schema> {
    pub(crate) hdr: &'a ShmBucketHeader,
    pub(crate) entries: &'a [ShmEntry<S>],
    pub(crate) pool: &'a Pool,
}

/// A bucket's string pool.
///
/// Comparisons and copies use bulk byte accesses over ranges checked by [`pool_slice`].
/// Exported bytes are copied into private memory before UTF-8 validation.
///
/// Reading through a `&[u8]` while another process may write the same bytes is technically
/// undefined behaviour. We accept it because the protocol keeps published strings stable: a
/// writer copies a string in before publishing its key, and nothing overwrites it until the
/// bucket is drained. The exceptions are a forced flush racing an `add`, a stalled writer, or
/// a corrupt peer.
///
/// There is no cheap sound alternative: Rust has no atomic memcpy, `memcmp` races the same way
/// under C's memory model, and per-byte atomic loads are too slow for `add`. A race can only
/// garble bytes inside a bounds-checked range, and those bytes are never used as offsets or
/// lengths.
pub type Pool = [AtomicU8];

pub(crate) fn bucket_hdr_size() -> usize {
    // Align to 8 bytes (AtomicU64 alignment).
    let s = size_of::<ShmBucketHeader>();
    (s + 7) & !7
}

/// Resolve a string reference against the pool it is an offset into.
///
/// The bounds are the pool's own, not the mapping's: a reference that merely landed somewhere
/// inside the segment would still be reading another bucket's slots, or the header. `sr` is
/// already a snapshot, so the range cannot move between this check and the access.
pub(crate) fn pool_slice(pool: &Pool, sr: StringRef) -> Option<&Pool> {
    let start = sr.offset as usize;
    pool.get(start..start.checked_add(sr.len as usize)?)
}

/// Compare a pool string against `expected` without copying it out.
///
/// A reference that does not resolve compares unequal, which sends `add` on to the next
/// slot rather than reporting a match on bytes it could not read.
pub(crate) fn pool_eq(pool: &Pool, sr: StringRef, expected: &str) -> bool {
    match pool_slice(pool, sr) {
        Some(bytes) if bytes.len() == expected.len() => {
            // SAFETY: the range is in bounds, as just checked; for the race this slice ignores,
            // see `Pool`. A racing write can only change whether this slot matches.
            let bytes =
                unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), bytes.len()) };
            bytes == expected.as_bytes()
        }
        _ => false,
    }
}

/// Copy a pool string into an owned `String`, charging its bytes to `budget`.
///
/// Lossy rather than strict about UTF-8: dropping a whole entry over one bad sequence would
/// lose stats that are otherwise fine. A reference that does not resolve, or does not fit the
/// budget, is not one this writer made, and yields `None`; an empty field is still `Some("")`.
///
/// `budget` is one allowance for a whole bucket. Per-reference bounds alone would still permit
/// `slots * fields * pool_size` bytes of output, since a peer can repeat one large reference
/// across every field of every slot; a pool's capacity is the most real data a bucket can
/// hold, so that is the allowance. Output can still reach a small multiple of it, one invalid
/// byte expanding to a three-byte replacement character.
pub(crate) fn pool_string(pool: &Pool, sr: StringRef, budget: &mut usize) -> Option<String> {
    let bytes = pool_slice(pool, sr)?;
    if bytes.len() > *budget {
        return None;
    }
    *budget -= bytes.len();
    // SAFETY: the range is in bounds, as just checked; for the race this slice ignores, see
    // `Pool`. Validate only the private copy: a peer can keep rewriting the shared bytes.
    let bytes = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), bytes.len()) };
    let owned = bytes.to_vec();
    Some(match String::from_utf8(owned) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// Keep a schema's hash clear of the two values that mark a slot's state.
fn slot_hash(hash: u64) -> u64 {
    match hash {
        SLOT_EMPTY => 1,
        SLOT_INIT => SLOT_INIT - 1,
        v => v,
    }
}

/// Bump-allocate `s` in the pool, returning an empty reference if it does not fit.
///
/// `cursor` lives in the mapping, so a peer can set it to anything; a forged value can only
/// make the reservation fail, because the copy goes through [`pool_slice`] at the exact offset
/// the successful exchange returned rather than at whatever the cursor says by then.
pub(crate) fn alloc_str(pool: &Pool, cursor: &AtomicU32, s: &str) -> StringRef {
    let Ok(len) = u32::try_from(s.len()) else {
        return StringRef::default();
    };
    if len == 0 {
        return StringRef::default();
    }
    let pool_size = pool.len() as u32;
    let mut spins = 0u32;
    loop {
        let old = cursor.load(Relaxed);
        if old.saturating_add(len) > pool_size {
            return StringRef::default();
        }
        if cursor
            .compare_exchange_weak(old, old + len, Relaxed, Relaxed)
            .is_ok()
        {
            let sr = StringRef { offset: old, len };
            let Some(dst) = pool_slice(pool, sr) else {
                return StringRef::default();
            };
            // Copy into the checked reservation before publishing the key.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    s.as_ptr(),
                    dst.as_ptr().cast::<u8>().cast_mut(),
                    dst.len(),
                );
            }
            return sr;
        }
        spins += 1;
        if spins.is_multiple_of(YIELD_AFTER_SPINS) {
            thread::yield_now();
        } else {
            hint::spin_loop();
        }
    }
}

/// The groups drained out of one bucket, with the time range they cover.
pub(crate) struct DrainedBucket<T> {
    pub(crate) start: u64,
    pub(crate) duration: u64,
    pub(crate) groups: Vec<T>,
}

/// Shared-memory aggregation table.
///
/// Sidecar owners stay on their mapping; workers follow replacements by name.
pub struct ShmTable<S: Schema> {
    pub(crate) mem: ArcSwapOption<ShmTableMapping<S>>,
    path: Option<CString>,
}

pub(crate) struct ShmTableMapping<S: Schema> {
    mem: MappedMem<NamedShmHandle>,
    /// Validated once for this mapping, never reread from the shared header.
    pub(crate) layout: Layout<S>,
}

impl<S: Schema> Clone for ShmTable<S> {
    fn clone(&self) -> Self {
        Self {
            mem: ArcSwapOption::new(self.mem.load_full()),
            path: self.path.clone(),
        }
    }
}

impl<S: Schema> ShmTable<S> {
    /// Create a fresh segment, then signal workers to reopen it.
    pub fn create(
        path: CString,
        bucket_size_nanos: u64,
        slot_count: usize,
        string_pool_bytes: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            mem: ArcSwapOption::from_pointee(ShmTableMapping::create(
                path,
                bucket_size_nanos,
                slot_count,
                string_pool_bytes,
            )?),
            path: None,
        })
    }

    /// Open a segment and follow replacements on subsequent operations.
    pub fn open(path: &CStr) -> io::Result<Self> {
        Ok(Self {
            mem: ArcSwapOption::from_pointee(ShmTableMapping::open(path)?),
            path: Some(path.to_owned()),
        })
    }

    /// Open on first use, retrying failed opens and following replacements.
    pub fn new_reader(path: CString) -> Self {
        Self {
            mem: ArcSwapOption::empty(),
            path: Some(path),
        }
    }

    pub(crate) fn with_current<R>(&self, f: impl FnOnce(&ShmTableMapping<S>) -> R) -> Option<R> {
        let current = self.mem.load();
        if let Some(mem) = current
            .as_ref()
            .filter(|mem| self.path.is_none() || !mem.needs_reload())
        {
            return Some(f(mem));
        }
        let mem = Arc::new(ShmTableMapping::open(self.path.as_ref()?).ok()?);
        // A slow opener must not overwrite another thread's replacement.
        self.mem.compare_and_swap(&current, Some(mem));
        let current = self.mem.load();
        current
            .as_ref()
            .filter(|mem| !mem.needs_reload())
            .map(|mem| f(mem))
    }

    /// Whether the cached mapping is absent or retired.
    pub fn needs_reload(&self) -> bool {
        self.mem
            .load()
            .as_ref()
            .is_none_or(|mem| mem.needs_reload())
    }

    /// Unlink the cached mapping only while its name still refers to it.
    pub fn unlink(&self) {
        if let Some(mem) = self.mem.load().as_ref() {
            mem.mem.unlink();
        }
    }

    /// Submit an input to shared memory. Returns false if no current mapping is available.
    pub fn add(&self, input: &S::Input<'_>) -> bool {
        self.with_current(|mem| mem.add(input)).is_some()
    }

    /// Return `(used_slots, total_slots)` for the active bucket, or `(0, 0)` if unavailable.
    pub fn slot_usage(&self) -> (usize, usize) {
        self.with_current(ShmTableMapping::slot_usage)
            .unwrap_or_default()
    }

    /// Tell workers to reopen after the replacement is ready.
    pub fn signal_reload(&self) {
        if let Some(mem) = self.mem.load().as_ref() {
            mem.header().ready.store(0, Release);
        }
    }
}

impl<S: Schema> ShmTableMapping<S> {
    fn create(
        path: CString,
        bucket_size_nanos: u64,
        slot_count: usize,
        string_pool_bytes: usize,
    ) -> io::Result<Self> {
        let slot_count = u32::try_from(slot_count.max(1)).unwrap_or(u32::MAX);
        let string_pool_size = u32::try_from(string_pool_bytes).unwrap_or(u32::MAX);
        let (layout, total) = Layout::for_new(bucket_size_nanos, slot_count, string_pool_size)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SHM table: dimensions do not describe a mappable segment",
                )
            })?;

        let (handle, replaced) = NamedShmHandle::create_replacing(path, total)?;
        let mem = handle.map()?;

        let this = Self { mem, layout };

        let hdr = this.header();
        hdr.version.store(SHM_VERSION, Relaxed);
        hdr.schema_id.store(S::ID, Relaxed);
        hdr.schema_version.store(S::VERSION, Relaxed);
        hdr.bucket_size_nanos
            .store(layout.bucket_size_nanos, Relaxed);
        hdr.slot_count.store(layout.slot_count, Relaxed);
        hdr.bucket_region_size
            .store(layout.bucket_region_size, Relaxed);
        hdr.string_pool_size.store(layout.string_pool_size, Relaxed);
        // Signal readiness LAST — workers see ready=0 until this store and fall back
        // to IPC, preventing writes to a partially-initialized table.
        hdr.ready.store(1, Release);

        // Retire old segments only after the replacement is ready.
        for old in replaced {
            if let Ok(old) = old.map() {
                if old.get_size() > READY_OFFSET {
                    // SAFETY: in bounds, as just checked, and the pointer is the mapping's own,
                    // valid for reads and writes; no slice or reference over the shared bytes is
                    // made. An `AtomicU8` has no alignment requirement, and every process
                    // accesses `ready` atomically, so this does not race with a non-atomic
                    // access. Only that byte is touched, not the rest of the old segment's
                    // untrusted header.
                    let ready = unsafe {
                        AtomicU8::from_ptr(old.ptr.as_ptr().cast::<u8>().add(READY_OFFSET))
                    };
                    ready.store(0, Release);
                }
            }
        }

        Ok(this)
    }

    /// Validate dimensions against the mapped length before using them.
    fn open(path: &CStr) -> io::Result<Self> {
        let handle = NamedShmHandle::open(path)?;
        let mem = handle.map()?;
        let mapped_len = mem.as_slice().len();
        let invalid = |msg: &'static str| io::Error::new(io::ErrorKind::InvalidData, msg);

        if mapped_len < size_of::<ShmHeader>() {
            return Err(invalid("SHM table: mapping shorter than its header"));
        }
        // SAFETY: the mapping is page-aligned and long enough for the header, as just checked.
        // Every field is an integer atomic, so whatever bytes are in it are a valid value.
        let hdr = unsafe { &*(mem.as_slice().as_ptr() as *const ShmHeader) };
        // Acquire: the creator stores the dimensions read below before releasing `ready`.
        if hdr.ready.load(Acquire) == 0 {
            return Err(invalid("SHM table: not yet ready"));
        }
        if hdr.version.load(Relaxed) != SHM_VERSION {
            return Err(invalid("SHM table: incompatible version"));
        }
        if hdr.schema_id.load(Relaxed) != S::ID {
            return Err(invalid("SHM table: segment holds a different schema"));
        }
        if hdr.schema_version.load(Relaxed) != S::VERSION {
            return Err(invalid("SHM table: incompatible schema version"));
        }
        let layout = Layout::new(
            hdr.bucket_size_nanos.load(Relaxed),
            hdr.slot_count.load(Relaxed),
            hdr.string_pool_size.load(Relaxed),
            hdr.bucket_region_size.load(Relaxed),
            mapped_len,
        )
        .ok_or_else(|| invalid("SHM table: header does not describe this mapping"))?;

        Ok(Self { mem, layout })
    }

    pub(crate) fn header(&self) -> &ShmHeader {
        // SAFETY: both constructors accept a `Layout` only for a mapping whose first page
        // holds the header, and a mapping's length does not change after it is made. The base
        // is page-aligned, and every field is an integer atomic - valid for any bit pattern.
        unsafe { &*(self.mem.as_slice().as_ptr() as *const ShmHeader) }
    }

    /// The bucket region at `idx`, of which only the low bit counts: `active_idx` lives in the
    /// mapping and a peer can leave any byte in it, while a segment holds exactly two buckets.
    pub(crate) fn bucket(&self, idx: u8) -> Bucket<'_, S> {
        let l = &self.layout;
        let start = l.bucket0_start + (idx & 1) as usize * l.bucket_region_size as usize;
        let base = self.mem.as_slice().as_ptr();
        // SAFETY: `Layout::new` accepted this layout only for a mapping holding both bucket
        // regions in full, so `start .. start + bucket_region_size` is mapped, and the entry
        // array and the pool lie within that region by construction. Alignment holds all the
        // way down: the base is page-aligned, `bucket0_start` and `bucket_region_size` are
        // multiples of the entry alignment, `entries_offset` is 8-aligned, and the pool is
        // bytes. Every field reached through these references is an integer atomic, which
        // `Schema`'s safety contract extends to the key and value.
        unsafe {
            Bucket {
                hdr: &*(base.add(start) as *const ShmBucketHeader),
                entries: std::slice::from_raw_parts(
                    base.add(start + l.entries_offset) as *const ShmEntry<S>,
                    l.slot_count as usize,
                ),
                pool: std::slice::from_raw_parts(
                    base.add(start + l.pool_offset) as *const AtomicU8,
                    l.string_pool_size as usize,
                ),
            }
        }
    }

    pub(crate) fn needs_reload(&self) -> bool {
        self.header().ready.load(Acquire) == 0
    }

    /// Add an input to the currently-active bucket.  Thread-safe.
    pub(crate) fn add(&self, input: &S::Input<'_>) {
        let hdr = self.header();

        // Claim in-flight on the active bucket, with double-check against swap.
        let active = hdr.active_idx.load(Acquire);
        let mut bucket = self.bucket(active);
        bucket.hdr.in_flight.fetch_add(1, Acquire);

        let active2 = hdr.active_idx.load(Acquire);
        if (active ^ active2) & 1 != 0 {
            bucket.hdr.in_flight.fetch_sub(1, Release);
            bucket = self.bucket(active2);
            bucket.hdr.in_flight.fetch_add(1, Acquire);
        }

        let hash = slot_hash(S::hash(input));
        let slots = bucket.entries.len();
        // `Layout` guarantees a nonzero slot count, so this is not a division by zero, and
        // every index below stays inside the entry slice.
        let mut slot = (hash % slots as u64) as usize;
        let mut done = false;
        // Waiting on a slot that is being written is only worth it while somebody is actually
        // writing it. A stale SLOT_INIT is never cleared - the flush skips markers rather than
        // reclaiming slots it may not own - so an unbounded wait can be held open by one write
        // a peer performs once, and in the sidecar this runs on the IPC handler's own thread.
        // Past the budget the probe keeps looking but stops waiting; an input is lost, not the
        // caller. The budget spans the whole call, so it also bounds a table poisoned throughout.
        let mut waits = 0u32;
        for _ in 0..slots {
            let entry = &bucket.entries[slot];

            loop {
                match entry.key_hash.load(Acquire) {
                    SLOT_EMPTY => {
                        if entry
                            .key_hash
                            .compare_exchange(SLOT_EMPTY, SLOT_INIT, Acquire, Relaxed)
                            .is_ok()
                        {
                            S::write_key(&entry.key, bucket.pool, &bucket.hdr.string_cursor, input);
                            // Release on the store synchronises the key write with any
                            // subsequent Acquire load of the hash — no separate fence needed.
                            entry.key_hash.store(hash, Release);
                            S::record(&entry.value, input);
                            done = true;
                            break;
                        }
                        if waits >= MAX_SLOT_WAIT_ITERS {
                            break;
                        }
                        waits += 1;
                        if waits.is_multiple_of(YIELD_AFTER_SPINS) {
                            thread::yield_now();
                        } else {
                            hint::spin_loop();
                        }
                    }
                    SLOT_INIT => {
                        if waits >= MAX_SLOT_WAIT_ITERS {
                            break;
                        }
                        waits += 1;
                        if waits.is_multiple_of(YIELD_AFTER_SPINS) {
                            thread::yield_now();
                        } else {
                            hint::spin_loop();
                        }
                    }
                    h if h == hash => {
                        if S::key_matches(&entry.key, bucket.pool, input) {
                            S::record(&entry.value, input);
                            done = true;
                        }
                        break;
                    }
                    _ => break, // hash collision, probe next
                }
            }

            if done {
                break;
            }
            slot = (slot + 1) % slots;
        }

        bucket.hdr.in_flight.fetch_sub(1, Release);
    }

    pub(crate) fn slot_usage(&self) -> (usize, usize) {
        let bucket = self.bucket(self.header().active_idx.load(Acquire));
        let used = bucket
            .entries
            .iter()
            .filter(|entry| {
                let h = entry.key_hash.load(Relaxed);
                h != SLOT_EMPTY && h != SLOT_INIT
            })
            .count();

        (used, bucket.entries.len())
    }

    /// Drain the inactive bucket, or both buckets if `force` is true.
    ///
    /// `drain_entry` takes one occupied slot's value and turns it into a group, or `None` to
    /// leave it out; see [`ShmTableMapping::drain_bucket`].
    pub(crate) fn drain_buckets<T>(
        &self,
        force: bool,
        mut drain_entry: impl FnMut(&ShmEntry<S>, &Pool, &mut usize) -> Option<T>,
    ) -> Vec<DrainedBucket<T>> {
        let mut buckets = Vec::new();

        if force {
            for idx in 0u8..2 {
                if let Some(b) = self.drain_bucket(idx, &mut drain_entry) {
                    buckets.push(b);
                }
            }
        } else {
            let old_active = self.header().active_idx.fetch_xor(1, AcqRel);
            if let Some(b) = self.drain_bucket(old_active, &mut drain_entry) {
                buckets.push(b);
            }
        }

        buckets
    }

    /// Drain one bucket and reset it for reuse.
    ///
    /// `drain_entry` must take the slot's value with `swap` rather than load-then-clear: reading
    /// and then clearing would drop whatever arrived in between, and clearing in bulk would mean
    /// a non-atomic write over counters that workers may still be incrementing. When the slot's
    /// key cannot be decoded it should still drain the value before returning `None`, since
    /// leaving it behind would fold it into whichever key next occupies the slot.
    fn drain_bucket<T>(
        &self,
        bucket_idx: u8,
        drain_entry: &mut impl FnMut(&ShmEntry<S>, &Pool, &mut usize) -> Option<T>,
    ) -> Option<DrainedBucket<T>> {
        let bucket = self.bucket(bucket_idx);

        // Wait for in-flight writers (bounded to tolerate dead workers).
        // The intermediate loads only need Relaxed; a single fence(Acquire) after
        // the loop synchronizes with the Release in each writer's in_flight.fetch_sub,
        // and covers all subsequent SHM reads in this function and callees.
        let mut spins = 0u32;
        while bucket.hdr.in_flight.load(Relaxed) != 0 && spins < MAX_FLUSH_WAIT_ITERS {
            spins += 1;
            if spins.is_multiple_of(YIELD_AFTER_SPINS) {
                thread::yield_now();
            } else {
                hint::spin_loop();
            }
        }
        fence(Acquire);

        let bucket_start_ts = bucket.hdr.start_nanos.load(Relaxed);
        // One allowance for the whole bucket, not per string; see `pool_string`.
        let mut budget = self.layout.string_pool_size as usize;

        let mut groups = Vec::new();

        for entry in bucket.entries {
            let h = entry.key_hash.load(Relaxed);
            if h == SLOT_EMPTY || h == SLOT_INIT {
                continue;
            }

            if let Some(group) = drain_entry(entry, bucket.pool, &mut budget) {
                groups.push(group);
            }
            entry.key_hash.store(SLOT_EMPTY, Release);
        }

        bucket.hdr.string_cursor.store(0, Release);

        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        // The bucket width comes from the private layout, which guarantees it is nonzero: the
        // header's copy is a peer's to rewrite, and a zero here would panic the flush.
        let bucket_size_nanos = self.layout.bucket_size_nanos;
        bucket
            .hdr
            .start_nanos
            .store(now_ns - (now_ns % bucket_size_nanos), Release);

        if groups.is_empty() {
            return None;
        }

        Some(DrainedBucket {
            start: bucket_start_ts,
            duration: bucket_size_nanos,
            groups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm_stats::{DEFAULT_SLOT_COUNT, DEFAULT_STRING_POOL_BYTES, ShmSpanConcentrator};
    use std::sync::atomic::Ordering::SeqCst;

    fn test_path() -> CString {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        CString::new(format!(
            "/ddtable-shm-t-{}-{}",
            unsafe { libc::getpid() },
            COUNTER.fetch_add(1, SeqCst)
        ))
        .unwrap()
    }

    /// A schema with a different slot layout from the span stats one, for checking that a
    /// segment only opens as the schema that created it.
    struct Counter;

    impl Schema for Counter {
        const ID: u32 = u32::MAX;
        const VERSION: u32 = 1;

        type Key = AtomicU64;
        type Value = AtomicU64;
        type Input<'a> = u64;

        fn hash(input: &u64) -> u64 {
            *input
        }

        fn write_key(key: &AtomicU64, _pool: &Pool, _cursor: &AtomicU32, input: &u64) {
            key.store(*input, Relaxed);
        }

        fn key_matches(key: &AtomicU64, _pool: &Pool, input: &u64) -> bool {
            key.load(Relaxed) == *input
        }

        fn record(value: &AtomicU64, _input: &u64) {
            value.fetch_add(1, Relaxed);
        }
    }

    /// The slot size an opener validates against is its own schema's; opening another schema's
    /// segment would put every slot boundary in the wrong place.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_segment_opens_only_as_its_own_schema() {
        let counter_path = test_path();
        let counter = ShmTable::<Counter>::create(counter_path.clone(), 10, 8, 64).unwrap();
        assert!(ShmSpanConcentrator::open(&counter_path).is_err());
        assert!(ShmTable::<Counter>::open(&counter_path).is_ok());

        let stats_path = test_path();
        let _stats = ShmSpanConcentrator::create(
            stats_path.clone(),
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        assert!(ShmTable::<Counter>::open(&stats_path).is_err());

        counter
            .mem
            .load_full()
            .unwrap()
            .header()
            .schema_version
            .store(Counter::VERSION + 1, Relaxed);
        assert!(ShmTable::<Counter>::open(&counter_path).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_schema_records_and_drains_through_the_table() {
        let table = ShmTable::<Counter>::create(test_path(), 10, 8, 64).unwrap();
        for input in [3, 3, 5] {
            assert!(table.add(&input));
        }
        let mut drained: Vec<(u64, u64)> = table
            .mem
            .load_full()
            .unwrap()
            .drain_buckets(true, |entry, _pool, _budget| {
                Some((entry.key.load(Relaxed), entry.value.swap(0, Relaxed)))
            })
            .into_iter()
            .flat_map(|b| b.groups)
            .collect();
        drained.sort();
        assert_eq!(drained, [(3, 2), (5, 1)]);
    }
}
