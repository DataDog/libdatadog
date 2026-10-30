// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free shared-memory span stats concentrator.
//!
//! All PHP worker processes open the same SHM file and call
//! [`ShmSpanConcentrator::add_span`].  The sidecar is the creator
//! ([`ShmSpanConcentrator::create`]) and periodically calls
//! [`ShmSpanConcentrator::flush`] to drain the inactive bucket.
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
//! the length actually mapped and then kept privately; string references are
//! checked against their own pool and copied out under a per-bucket budget; mutable fields are
//! integer atomics that get decoded rather than loaded as Rust types. A peer can corrupt its
//! own telemetry - that much is inherent in a buffer everyone may write - but it cannot make
//! either side reach outside the mapping, panic, or export bytes it never put in the pool.
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
//! ## Additional metric tags
//! They share the peer tag arrays of a slot. A bucket admits at most
//! `additional_tags_cardinality_limit` entries with unmasked additional metric tags: a writer
//! takes room under that limit before it claims a free slot. Finding none left, it waits while
//! other writers still fill slots they took room for - one may be filling in its very key - and
//! then replaces the values with `TRACER_BLOCKED_VALUE` and probes again. It does the same when
//! no free slot is left. Masked keys are always admitted.
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
//!    their next `add_span` call.
//! 3. Holds onto the old concentrator for ≥ 1 s, flushing it periodically, to absorb any spans that
//!    arrived before workers noticed the reload flag.
//! 4. Drops the old concentrator after that grace period.

use arc_swap::ArcSwapOption;
use std::ffi::{CStr, CString};
use std::hash::{Hash, Hasher};
use std::hint;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering::*, fence};
use std::thread;
use zwohash::ZwoHasher;

use libdd_ddsketch::DDSketch;
use libdd_trace_protobuf::pb;
use libdd_trace_stats::span_concentrator::{
    ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN, ADDITIONAL_METRIC_TAGS_MAX_KEYS, FixedAggregationKey,
    FlushResult, FlushableConcentrator, TRACER_BLOCKED_VALUE, additional_metric_tag_value_too_long,
    cardinality_limit_telemetry::{CollapsedFieldSet, CollapsedFieldsMetrics},
};
use tracing::warn;

use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle};

const SHM_VERSION: u32 = 4;

/// Maximum (key, value) tag pairs per aggregation slot: peer tags and additional metric tags
/// share the same arrays.
pub const MAX_PEER_TAGS: usize = 16;

/// Maximum additional metric tag (key, value) pairs per aggregation slot. Peer tags get the
/// slots they leave free.
pub const MAX_ADDITIONAL_TAGS: usize = ADDITIONAL_METRIC_TAGS_MAX_KEYS;
const _: () = assert!(MAX_ADDITIONAL_TAGS <= MAX_PEER_TAGS);

/// Number of histogram bins (ok + error each) per aggregation group.
pub const N_BINS: usize = 256;

/// Upper bound of the highest histogram bin (100 s in nanoseconds).
const MAX_DURATION_NS: u64 = 100_000_000_000;

const SLOT_EMPTY: u64 = 0;
const SLOT_INIT: u64 = u64::MAX;

/// Default aggregation slots per bucket.
pub const DEFAULT_SLOT_COUNT: usize = 256;
/// Default per-bucket string pool size.
pub const DEFAULT_STRING_POOL_BYTES: usize = 512 * 1024;

/// The sidecar should recreate the SHM when slot utilisation exceeds this ratio.
pub const RELOAD_FILL_RATIO: f64 = 0.80;

/// Max iterations when waiting for `in_flight` to reach zero (~100 µs).
const MAX_FLUSH_WAIT_ITERS: u32 = 100_000;
/// Spin iterations before yielding to the OS scheduler.
const YIELD_AFTER_SPINS: u32 = 8;
/// Max iterations `add_span` spends waiting on slots that are marked as being written. One
/// budget for the whole call, so the worst case does not scale with the table's size.
const MAX_SLOT_WAIT_ITERS: u32 = 10_000;

fn bin_for_duration(nanos: i64) -> usize {
    if nanos <= 0 {
        return 0;
    }
    let d = nanos as u64;
    if d >= MAX_DURATION_NS {
        return N_BINS - 1;
    }
    let scale = (MAX_DURATION_NS as f64).ln() / (N_BINS as f64 - 2.0);
    let b = 1.0 + (d as f64).ln() / scale;
    (b as usize).clamp(1, N_BINS - 2)
}

fn bin_representative(bin: usize) -> f64 {
    if bin == 0 {
        return 0.0;
    }
    let scale = (MAX_DURATION_NS as f64).ln() / (N_BINS as f64 - 2.0);
    ((bin as f64 - 0.5) * scale).exp()
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
struct WireStringRef {
    offset: AtomicU32,
    len: AtomicU32,
}

impl WireStringRef {
    fn snapshot(&self) -> StringRef {
        StringRef {
            offset: self.offset.load(Relaxed),
            len: self.len.load(Relaxed),
        }
    }

    fn store(&self, sr: StringRef) {
        self.offset.store(sr.offset, Relaxed);
        self.len.store(sr.len, Relaxed);
    }
}

/// Number of fixed (non-peer-tag) string fields in an aggregation key.
const FIXED_STRS: usize = 8;

/// The fixed string fields of a key, in the order [`ShmKeyHeader::strs`] stores them.
///
/// Writing and matching a key both go through here; draining it destructures the same array
/// positionally, so that order is repeated there and
/// `a_key_round_trips_every_fixed_string_field` is what holds the two together.
fn fixed_strs<'a>(f: &FixedAggregationKey<&'a str>) -> [&'a str; FIXED_STRS] {
    [
        f.resource_name,
        f.service_name,
        f.operation_name,
        f.span_type,
        f.span_kind,
        f.http_method,
        f.http_endpoint,
        f.service_source,
    ]
}

/// `grpc_status_code` encoding for "no code recorded".
const GRPC_STATUS_NONE: u32 = u32::MAX;

/// The scalar key fields, snapshotted out of the mapping and validated.
struct KeyScalars {
    http_status_code: u32,
    grpc_status_code: Option<u8>,
    is_synthetics_request: bool,
    is_trace_root: pb::Trilean,
    /// Together with `additional_tag_count` at most [`MAX_PEER_TAGS`].
    peer_tag_count: usize,
    additional_tag_count: usize,
}

/// Aggregation key as stored in the mapping - position-independent, no raw pointers.
///
/// Integer atomics throughout, which is about validity more than atomicity: a `bool` that is
/// neither 0 nor 1, or a `Trilean` outside `0..=2`, is an invalid value whose existence as a
/// typed reference is already undefined behaviour, before anything inspects it. The scalars
/// travel as raw integers and become Rust types in [`ShmKeyHeader::scalars`], after checking.
#[repr(C)]
struct ShmKeyHeader {
    /// In [`fixed_strs`] order.
    strs: [WireStringRef; FIXED_STRS],
    /// `peer_tag_count` peer tags, followed by `additional_tag_count` additional metric tags.
    tag_keys: [WireStringRef; MAX_PEER_TAGS],
    tag_values: [WireStringRef; MAX_PEER_TAGS],
    http_status_code: AtomicU32,
    /// [`GRPC_STATUS_NONE`], or a code in `0..=255`.
    grpc_status_code: AtomicU32,
    /// 0 or 1.
    is_synthetics_request: AtomicU8,
    /// A [`pb::Trilean`] discriminant: 0 not set, 1 true, 2 false.
    is_trace_root: AtomicU8,
    /// Together with `additional_tag_count` at most [`MAX_PEER_TAGS`].
    peer_tag_count: AtomicU8,
    /// At most [`MAX_ADDITIONAL_TAGS`].
    additional_tag_count: AtomicU8,
}

impl ShmKeyHeader {
    /// Snapshot the scalar fields, rejecting any encoding this writer does not produce.
    ///
    /// `None` means the slot is not a key we wrote, so its key cannot be reported and cannot
    /// match an input either. Nothing here decides where memory is accessed - the tag counts
    /// included, since the arrays they count are zipped rather than indexed - so this is about
    /// not fabricating invalid Rust values, not about bounds.
    fn scalars(&self) -> Option<KeyScalars> {
        let peer_tag_count = self.peer_tag_count.load(Relaxed) as usize;
        let additional_tag_count = self.additional_tag_count.load(Relaxed) as usize;
        if additional_tag_count > MAX_ADDITIONAL_TAGS
            || peer_tag_count + additional_tag_count > MAX_PEER_TAGS
        {
            return None;
        }
        Some(KeyScalars {
            http_status_code: self.http_status_code.load(Relaxed),
            grpc_status_code: match self.grpc_status_code.load(Relaxed) {
                GRPC_STATUS_NONE => None,
                code => Some(u8::try_from(code).ok()?),
            },
            is_synthetics_request: match self.is_synthetics_request.load(Relaxed) {
                0 => false,
                1 => true,
                _ => return None,
            },
            is_trace_root: match self.is_trace_root.load(Relaxed) {
                0 => pb::Trilean::NotSet,
                1 => pb::Trilean::True,
                2 => pb::Trilean::False,
                _ => return None,
            },
            peer_tag_count,
            additional_tag_count,
        })
    }

    /// Publish the scalar fields in the encoding [`ShmKeyHeader::scalars`] accepts.
    fn store_scalars(
        &self,
        f: &FixedAggregationKey<&str>,
        peer_tag_count: usize,
        additional_tag_count: usize,
    ) {
        self.http_status_code.store(f.http_status_code, Relaxed);
        self.grpc_status_code.store(
            f.grpc_status_code.map_or(GRPC_STATUS_NONE, u32::from),
            Relaxed,
        );
        self.is_synthetics_request
            .store(u8::from(f.is_synthetics_request), Relaxed);
        self.is_trace_root.store(
            match f.is_trace_root {
                pb::Trilean::NotSet => 0,
                pb::Trilean::True => 1,
                pb::Trilean::False => 2,
            },
            Relaxed,
        );
        self.peer_tag_count
            .store(peer_tag_count.min(MAX_PEER_TAGS) as u8, Relaxed);
        self.additional_tag_count
            .store(additional_tag_count.min(MAX_ADDITIONAL_TAGS) as u8, Relaxed);
    }
}

/// Per-group stats.  `AtomicU64` is `#[repr(transparent)]` over `u64`, so the
/// layout is identical to plain integers and zero-initialised mmap memory is
/// valid for `AtomicU64::new(0)`.
#[repr(C, align(8))]
struct ShmStats {
    /// Total number of spans in this group.
    hits: AtomicU64,
    /// Number of error spans in this group.
    errors: AtomicU64,
    /// Sum of all span durations (nanoseconds).
    duration_sum: AtomicU64,
    /// Number of top-level spans (service-entry or measured).
    top_level_hits: AtomicU64,
    /// Histogram bins for non-error span durations.
    ok_bins: [AtomicU64; N_BINS],
    /// Histogram bins for error span durations.
    error_bins: [AtomicU64; N_BINS],
}

/// One slot in the hash table.
#[repr(C)]
struct ShmEntry {
    key_hash: AtomicU64,
    key: ShmKeyHeader,
    stats: ShmStats,
}

/// Per-bucket control header.
#[repr(C)]
struct ShmBucketHeader {
    start_nanos: AtomicU64,
    in_flight: AtomicI64,
    /// Room taken under the additional metric tags cardinality limit, see
    /// [`ShmBucketHeader::take_room`].
    additional_tag_room: AtomicU64,
    string_cursor: AtomicU32,
    /// Spans whose additional metric tag values were replaced by [`TRACER_BLOCKED_VALUE`] for
    /// lack of room under the cardinality limit, or in the table.
    additional_tag_collapsed: AtomicU32,
    /// Spans with additional metric tag values replaced for being too long.
    additional_tag_oversized: AtomicU32,
    /// Set by the first writer blocking additional metric tag values, which alone logs it.
    additional_tag_warned: AtomicU8,
}

/// What [`ShmBucketHeader::take_room`] found under the cardinality limit.
enum Room {
    /// Taken: to be kept with [`ShmBucketHeader::commit_room`] once the slot is filled, or given
    /// back with [`ShmBucketHeader::release_room`].
    Taken,
    /// None left, but writers that took some are still filling their slots.
    Pending,
    /// None left.
    Full,
}

/// `additional_tag_room` holds the room taken in its low half, and in its high half how much of
/// it writers are still filling slots for. One word, so that a writer finding no room left also
/// learns whether another writer might be about to add its very key.
fn pack_room(taken: u32, pending: u32) -> u64 {
    u64::from(pending) << 32 | u64::from(taken)
}

fn unpack_room(room: u64) -> (u32, u32) {
    // Truncating to the low half is the point.
    (room as u32, (room >> 32) as u32)
}

impl ShmBucketHeader {
    fn take_room(&self, limit: u32) -> Room {
        let taken = self.update_room(|taken, pending| {
            (taken < limit).then(|| (taken + 1, pending.saturating_add(1)))
        });
        match taken {
            Ok(()) => Room::Taken,
            Err((_, pending)) if pending > 0 => Room::Pending,
            Err(_) => Room::Full,
        }
    }

    /// Keep room whose slot is filled. Release, so that a writer seeing the room no longer
    /// pending also sees the key in the slot.
    fn commit_room(&self) {
        let _ = self.update_room(|taken, pending| Some((taken, pending.saturating_sub(1))));
    }

    /// Give back room whose slot another writer claimed first.
    fn release_room(&self) {
        let _ = self.update_room(|taken, pending| {
            Some((taken.saturating_sub(1), pending.saturating_sub(1)))
        });
    }

    /// Replace `(taken, pending)` with what `update` makes of it, or return the value it
    /// refused. Updates saturate, since a flush resets the word even under a writer that missed
    /// its deadline.
    ///
    /// A loop of its own, as `fetch_update` is deprecated in favor of `try_update`, which the
    /// MSRV does not have yet.
    fn update_room(
        &self,
        update: impl Fn(u32, u32) -> Option<(u32, u32)>,
    ) -> Result<(), (u32, u32)> {
        let mut room = self.additional_tag_room.load(Acquire);
        loop {
            let (taken, pending) = unpack_room(room);
            let Some((new_taken, new_pending)) = update(taken, pending) else {
                return Err((taken, pending));
            };
            match self.additional_tag_room.compare_exchange_weak(
                room,
                pack_room(new_taken, new_pending),
                AcqRel,
                Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(current) => room = current,
            }
        }
    }
}

/// Global SHM header (first page of the mapping).
///
/// Only [`ShmHeader::version`] is read for its own sake; the dimensions are snapshotted once
/// by [`ShmSpanConcentrator::open`] and thereafter live in a private [`Layout`]. They stay in
/// the mapping because an opener has nowhere else to learn them from, not because anything
/// keeps consulting them.
#[repr(C)]
struct ShmHeader {
    /// Layout version; checked by [`ShmSpanConcentrator::open`].  Mismatch returns an error.
    ///
    /// First field, and a `u32` in every version, so that a client of any vintage can read it
    /// before it knows anything else about the layout: that is what stops an older client from
    /// interpreting a newer segment as its own format.
    version: AtomicU32,
    /// Set to 1 by the sidecar when workers should re-open the SHM at the
    /// same path (a new, larger mapping has been created there).
    ready: AtomicU8,
    /// Index of the bucket currently being written to by PHP workers; only its low bit counts.
    active_idx: AtomicU8,
    /// Width of each time bucket in nanoseconds (e.g. 10 s = 10_000_000_000).
    bucket_size_nanos: AtomicU64,
    /// Number of aggregation slots per bucket (hash-table capacity).
    slot_count: AtomicU32,
    /// Byte size of one full bucket region (header + slots + string pool), page-aligned.
    bucket_region_size: AtomicU32,
    /// Byte capacity of the per-bucket string pool.
    string_pool_size: AtomicU32,
    /// Monotonic counter incremented on every successful flush, used as the stats sequence number.
    flush_seq: AtomicU64,
}

/// Where everything in the mapping is, kept in private memory.
///
/// Every field here also exists in [`ShmHeader`]. The creator takes them from its own
/// allocation parameters; an opener snapshots the header once and accepts it only if it
/// describes a segment fitting the length it mapped. No address, loop bound or divisor is
/// recomputed from the header afterwards - `slot_count` reads like configuration, but the
/// flush loop's bound and its slot addresses both come from it.
#[derive(Clone, Copy)]
struct Layout {
    /// Nonzero: flushing divides by it.
    bucket_size_nanos: u64,
    /// Nonzero: [`ShmSpanConcentrator::add_span`] takes a remainder by it.
    slot_count: u32,
    bucket_region_size: u32,
    string_pool_size: u32,
    /// Offset of bucket 0; bucket 1 begins `bucket_region_size` later.
    bucket0_start: usize,
    /// Offset of the entry array within a bucket region.
    entries_offset: usize,
    /// Offset of the string pool within a bucket region.
    pool_offset: usize,
}

impl Layout {
    /// Accept these dimensions for a mapping of `mapped_len` bytes, or reject them.
    ///
    /// `mapped_len` is the length this process passed to `mmap`, not anything read out of the
    /// segment, which is what makes it a usable bound: a peer can grow or shrink the backing
    /// file but cannot change how much of it we mapped.
    fn new(
        bucket_size_nanos: u64,
        slot_count: u32,
        string_pool_size: u32,
        bucket_region_size: u32,
        mapped_len: usize,
    ) -> Option<Self> {
        let bucket0_start = page_size::get();
        let entries_offset = bucket_hdr_size();
        let pool_offset = entries_offset
            .checked_add((slot_count as usize).checked_mul(size_of::<ShmEntry>())?)?;
        let needed = pool_offset.checked_add(string_pool_size as usize)?;
        let total = bucket0_start.checked_add((bucket_region_size as usize).checked_mul(2)?)?;

        if bucket_size_nanos == 0
            || slot_count == 0
            // The header shares bucket 0's first page with nothing else.
            || bucket0_start < size_of::<ShmHeader>()
            // Bucket 1 starts one region in, so a region size that is not a multiple of the
            // entry alignment would leave its slots misaligned - and a misaligned atomic
            // access is undefined behaviour, not a slow one.
            || !(bucket_region_size as usize).is_multiple_of(align_of::<ShmEntry>())
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
        })
    }

    /// The layout of a segment created with these dimensions, and the size to map for it.
    fn for_new(
        bucket_size_nanos: u64,
        slot_count: u32,
        string_pool_size: u32,
    ) -> Option<(Self, usize)> {
        let raw = bucket_hdr_size()
            .checked_add((slot_count as usize).checked_mul(size_of::<ShmEntry>())?)?
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
struct Bucket<'a> {
    hdr: &'a ShmBucketHeader,
    entries: &'a [ShmEntry],
    pool: &'a Pool,
}

/// A bucket's string pool.
///
/// Comparisons and copies use bulk byte accesses over ranges checked by [`pool_slice`].
/// Exported bytes are copied into private memory before UTF-8 validation.
type Pool = [AtomicU8];

fn bucket_hdr_size() -> usize {
    // Align to 8 bytes (AtomicU64 alignment).
    let s = size_of::<ShmBucketHeader>();
    (s + 7) & !7
}

/// Resolve a string reference against the pool it is an offset into.
///
/// The bounds are the pool's own, not the mapping's: a reference that merely landed somewhere
/// inside the segment would still be reading another bucket's slots, or the header. `sr` is
/// already a snapshot, so the range cannot move between this check and the access.
fn pool_slice(pool: &Pool, sr: StringRef) -> Option<&Pool> {
    let start = sr.offset as usize;
    pool.get(start..start.checked_add(sr.len as usize)?)
}

/// Compare a pool string against `expected` without copying it out.
///
/// A reference that does not resolve compares unequal, which sends `add_span` on to the next
/// slot rather than reporting a match on bytes it could not read.
fn pool_eq(pool: &Pool, sr: StringRef, expected: &str) -> bool {
    match pool_slice(pool, sr) {
        Some(bytes) if bytes.len() == expected.len() => {
            // Deliberately use non-atomic byte equality for this fixed, checked range.
            // A peer may change the match result; no byte is interpreted as metadata or UTF-8.
            let bytes =
                unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), bytes.len()) };
            bytes == expected.as_bytes()
        }
        _ => false,
    }
}

/// Copy a pool string into an owned `String`, charging its bytes to `budget`.
///
/// Lossy rather than strict: dropping a whole entry over one bad sequence would lose stats
/// that are otherwise fine. A reference that does not resolve, or does not fit the budget,
/// yields an empty string - which is what an absent field looks like anyway.
///
/// `budget` is one allowance for a whole bucket. Per-reference bounds alone would still permit
/// `slots * fields * pool_size` bytes of output, since a peer can repeat one large reference
/// across every field of every slot; a pool's capacity is the most real data a bucket can
/// hold, so that is the allowance. Output can still reach a small multiple of it, one invalid
/// byte expanding to a three-byte replacement character.
fn pool_string(pool: &Pool, sr: StringRef, budget: &mut usize) -> String {
    let Some(bytes) = pool_slice(pool, sr) else {
        return String::new();
    };
    if bytes.len() > *budget {
        return String::new();
    }
    *budget -= bytes.len();
    // Validate only the private copy: a peer can keep rewriting the shared bytes.
    let bytes = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), bytes.len()) };
    let owned = bytes.to_vec();
    match String::from_utf8(owned) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

fn hash_key(key: &SlotKey<'_>) -> u64 {
    let mut h = ZwoHasher::default();
    key.fixed.hash(&mut h);
    key.peer_tags.len().hash(&mut h);
    for &(k, v) in key.tags() {
        k.hash(&mut h);
        v.hash(&mut h);
    }
    match h.finish() {
        SLOT_EMPTY => 1,
        SLOT_INIT => SLOT_INIT - 1,
        v => v,
    }
}

/// Spend one of `waits` on a slot that is being written - a spin-loop hint, or a thread yield
/// every [`YIELD_AFTER_SPINS`] of them - or return false once [`MAX_SLOT_WAIT_ITERS`] are spent.
fn wait_for_slot(waits: &mut u32) -> bool {
    if *waits >= MAX_SLOT_WAIT_ITERS {
        return false;
    }
    *waits += 1;
    if waits.is_multiple_of(YIELD_AFTER_SPINS) {
        thread::yield_now();
    } else {
        hint::spin_loop();
    }
    true
}

/// Bump-allocate `s` in the pool, returning an empty reference if it does not fit.
///
/// `cursor` lives in the mapping, so a peer can set it to anything; a forged value can only
/// make the reservation fail, because the copy goes through [`pool_slice`] at the exact offset
/// the successful exchange returned rather than at whatever the cursor says by then.
fn alloc_str(pool: &Pool, cursor: &AtomicU32, s: &str) -> StringRef {
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

/// Pre-extracted span stats for one span, ready to be fed into [`ShmSpanConcentrator::add_span`].
pub struct ShmSpanInput<'a> {
    /// Aggregation key fields (everything except peer tags).
    pub fixed: FixedAggregationKey<&'a str>,
    /// (key, value) peer-tag pairs. Only the first `MAX_PEER_TAGS - n` are kept, `n` being the
    /// number of additional metric tags kept.
    pub peer_tags: &'a [(&'a str, &'a str)],
    /// (key, value) additional metric tag pairs, sorted by key without duplicate keys. Values may
    /// be empty. Only the first `MAX_ADDITIONAL_TAGS` are kept. Not checked: tags breaking this
    /// only split the caller's entries or repeat keys in them.
    pub additional_tags: &'a [(&'a str, &'a str)],
    /// How many entries with unmasked additional metric tags a bucket admits. Once reached, the
    /// values of a new key's additional metric tags are replaced by [`TRACER_BLOCKED_VALUE`].
    /// Note that different callers may technically have different limits. Callers shouldn't.
    /// But it lets it work seamlessly if they do.
    pub additional_tags_cardinality_limit: u32,
    // stats
    pub duration_ns: i64,
    pub is_error: bool,
    pub is_top_level: bool,
}

/// Owned (serializable) version of [`ShmSpanInput`].
///
/// The IPC fallback sends this to the sidecar, which creates the concentrator if needed
/// and adds the span.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct OwnedShmSpanInput {
    pub fixed: FixedAggregationKey<String>,
    pub peer_tags: Vec<(String, String)>,
    pub additional_tags: Vec<(String, String)>,
    pub additional_tags_cardinality_limit: u32,
    pub duration_ns: i64,
    pub is_error: bool,
    pub is_top_level: bool,
}

impl OwnedShmSpanInput {
    /// Borrow as a [`ShmSpanInput`] for passing to [`ShmSpanConcentrator::add_span`].
    ///
    /// `tag_buf` is a caller-supplied scratch buffer; it must outlive the returned value.
    pub fn as_shm_input<'a>(
        &'a self,
        tag_buf: &'a mut Vec<(&'a str, &'a str)>,
    ) -> ShmSpanInput<'a> {
        tag_buf.clear();
        for (k, v) in self.peer_tags.iter().chain(&self.additional_tags) {
            tag_buf.push((k.as_str(), v.as_str()));
        }
        let tag_buf: &'a [(&'a str, &'a str)] = tag_buf;
        let (peer_tags, additional_tags) = tag_buf.split_at(self.peer_tags.len());
        ShmSpanInput {
            fixed: self.fixed.convert(|s: &str| s),
            peer_tags,
            additional_tags,
            additional_tags_cardinality_limit: self.additional_tags_cardinality_limit,
            duration_ns: self.duration_ns,
            is_error: self.is_error,
            is_top_level: self.is_top_level,
        }
    }
}

/// The aggregation key of a span as a slot stores it: tag counts within the arrays, and
/// additional metric tag values already masked where they have to be.
struct SlotKey<'a> {
    fixed: &'a FixedAggregationKey<&'a str>,
    peer_tags: &'a [(&'a str, &'a str)],
    additional_tags: &'a [(&'a str, &'a str)],
}

impl<'a> SlotKey<'a> {
    /// Peer tags followed by additional metric tags, the order the slot arrays store them in.
    fn tags(&self) -> impl Iterator<Item = &'a (&'a str, &'a str)> + use<'a> {
        self.peer_tags.iter().chain(self.additional_tags)
    }

    /// Whether a new key has to fit under the additional metric tags cardinality limit. Keys
    /// whose values are all masked never do, they are what a key over the limit turns into.
    fn is_cardinality_limited(&self) -> bool {
        self.additional_tags
            .iter()
            .any(|&(_, v)| v != TRACER_BLOCKED_VALUE)
    }
}

/// How [`ShmStatsMapping::insert`] ended.
enum Insert {
    /// The span was counted.
    Done,
    /// No slot could take the span.
    Lost,
    /// The key is new and the bucket has no room left for another key with unmasked additional
    /// metric tags.
    OverLimit,
}

/// Shared-memory span stats concentrator.
///
/// Sidecar owners stay on their mapping; workers follow replacements by name.
pub struct ShmSpanConcentrator {
    mem: ArcSwapOption<ShmStatsMapping>,
    path: Option<CString>,
}

struct ShmStatsMapping {
    mem: MappedMem<NamedShmHandle>,
    /// Validated once for this mapping, never reread from the shared header.
    layout: Layout,
}

impl Clone for ShmSpanConcentrator {
    fn clone(&self) -> Self {
        Self {
            mem: ArcSwapOption::new(self.mem.load_full()),
            path: self.path.clone(),
        }
    }
}

impl ShmSpanConcentrator {
    /// Create a fresh segment, then signal workers to reopen it.
    pub fn create(
        path: CString,
        bucket_size_nanos: u64,
        slot_count: usize,
        string_pool_bytes: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            mem: ArcSwapOption::from_pointee(ShmStatsMapping::create(
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
            mem: ArcSwapOption::from_pointee(ShmStatsMapping::open(path)?),
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

    fn with_current<R>(&self, f: impl FnOnce(&ShmStatsMapping) -> R) -> Option<R> {
        let current = self.mem.load();
        if let Some(mem) = current
            .as_ref()
            .filter(|mem| self.path.is_none() || !mem.needs_reload())
        {
            return Some(f(mem));
        }
        let mem = Arc::new(ShmStatsMapping::open(self.path.as_ref()?).ok()?);
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

    /// Submit a span to shared memory. Returns false if no current mapping is available.
    pub fn add_span(&self, input: &ShmSpanInput<'_>) -> bool {
        self.with_current(|mem| mem.add_span(input)).is_some()
    }

    /// Return `(used_slots, total_slots)` for the active bucket, or `(0, 0)` if unavailable.
    pub fn slot_usage(&self) -> (usize, usize) {
        self.with_current(ShmStatsMapping::slot_usage)
            .unwrap_or_default()
    }

    /// Tell workers to reopen after the replacement is ready.
    pub fn signal_reload(&self) {
        if let Some(mem) = self.mem.load().as_ref() {
            mem.header().ready.store(0, Release);
        }
    }

    /// Drain the inactive bucket, or both buckets if `force` is true.
    pub fn drain_buckets(&self, force: bool) -> Vec<pb::ClientStatsBucket> {
        self.drain(force).buckets
    }

    fn drain(&self, force: bool) -> Drained {
        self.with_current(|mem| mem.drain_buckets(force))
            .unwrap_or_default()
    }

    /// Flush the inactive bucket (both on `force`), returning `None` if empty or unavailable.
    pub fn flush(
        &self,
        force: bool,
        hostname: String,
        env: String,
        version: String,
        service: String,
        runtime_id: String,
    ) -> Option<pb::ClientStatsPayload> {
        self.with_current(|mem| mem.flush(force, hostname, env, version, service, runtime_id))?
    }
}

impl ShmStatsMapping {
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
                    "SHM span concentrator: dimensions do not describe a mappable segment",
                )
            })?;

        let (handle, replaced) = NamedShmHandle::create_replacing(path, total)?;
        let mem = handle.map()?;

        let this = Self { mem, layout };

        let hdr = this.header();
        hdr.version.store(SHM_VERSION, Relaxed);
        hdr.bucket_size_nanos
            .store(layout.bucket_size_nanos, Relaxed);
        hdr.slot_count.store(layout.slot_count, Relaxed);
        hdr.bucket_region_size
            .store(layout.bucket_region_size, Relaxed);
        hdr.string_pool_size.store(layout.string_pool_size, Relaxed);
        // Signal readiness LAST — workers see ready=0 until this store and fall back
        // to IPC, preventing writes to a partially-initialized concentrator.
        hdr.ready.store(1, Release);

        // Retire old segments only after the replacement is ready.
        for old in replaced {
            if let Ok(old) = old.map()
                && old.as_slice().len() >= size_of::<ShmHeader>()
            {
                // SAFETY: page-aligned, in bounds, and all fields are integer atomics.
                // Touch only `ready`; the old segment's layout is untrusted.
                let hdr = unsafe { &*(old.as_slice().as_ptr() as *const ShmHeader) };
                hdr.ready.store(0, Release);
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
            return Err(invalid(
                "SHM span concentrator: mapping shorter than its header",
            ));
        }
        // SAFETY: the mapping is page-aligned and long enough for the header, as just checked.
        // Every field is an integer atomic, so whatever bytes are in it are a valid value.
        let hdr = unsafe { &*(mem.as_slice().as_ptr() as *const ShmHeader) };
        // Acquire: the creator stores the dimensions read below before releasing `ready`.
        if hdr.ready.load(Acquire) == 0 {
            return Err(invalid("SHM span concentrator: not yet ready"));
        }
        if hdr.version.load(Relaxed) != SHM_VERSION {
            return Err(invalid("SHM span concentrator: incompatible version"));
        }
        let layout = Layout::new(
            hdr.bucket_size_nanos.load(Relaxed),
            hdr.slot_count.load(Relaxed),
            hdr.string_pool_size.load(Relaxed),
            hdr.bucket_region_size.load(Relaxed),
            mapped_len,
        )
        .ok_or_else(|| invalid("SHM span concentrator: header does not describe this mapping"))?;

        Ok(Self { mem, layout })
    }

    fn header(&self) -> &ShmHeader {
        // SAFETY: both constructors accept a `Layout` only for a mapping whose first page
        // holds the header, and a mapping's length does not change after it is made. The base
        // is page-aligned, and every field is an integer atomic - valid for any bit pattern.
        unsafe { &*(self.mem.as_slice().as_ptr() as *const ShmHeader) }
    }

    /// The bucket region at `idx`, of which only the low bit counts: `active_idx` lives in the
    /// mapping and a peer can leave any byte in it, while a segment holds exactly two buckets.
    fn bucket(&self, idx: u8) -> Bucket<'_> {
        let l = &self.layout;
        let start = l.bucket0_start + (idx & 1) as usize * l.bucket_region_size as usize;
        let base = self.mem.as_slice().as_ptr();
        // SAFETY: `Layout::new` accepted this layout only for a mapping holding both bucket
        // regions in full, so `start .. start + bucket_region_size` is mapped, and the entry
        // array and the pool lie within that region by construction. Alignment holds all the
        // way down: the base is page-aligned, `bucket0_start` and `bucket_region_size` are
        // multiples of the entry alignment, `entries_offset` is 8-aligned, and the pool is
        // bytes. Every field reached through these references is an integer atomic.
        unsafe {
            Bucket {
                hdr: &*(base.add(start) as *const ShmBucketHeader),
                entries: std::slice::from_raw_parts(
                    base.add(start + l.entries_offset) as *const ShmEntry,
                    l.slot_count as usize,
                ),
                pool: std::slice::from_raw_parts(
                    base.add(start + l.pool_offset) as *const AtomicU8,
                    l.string_pool_size as usize,
                ),
            }
        }
    }

    fn needs_reload(&self) -> bool {
        self.header().ready.load(Acquire) == 0
    }

    /// Add a span to the currently-active bucket.  Thread-safe.
    fn add_span(&self, input: &ShmSpanInput<'_>) {
        let mut additional_tags = [("", ""); MAX_ADDITIONAL_TAGS];
        let mut additional_tag_count = 0;
        let mut oversized_key = None;
        for (tag, &(k, v)) in additional_tags.iter_mut().zip(input.additional_tags) {
            if additional_metric_tag_value_too_long(v) {
                oversized_key.get_or_insert(k);
                *tag = (k, TRACER_BLOCKED_VALUE);
            } else {
                *tag = (k, v);
            }
            additional_tag_count += 1;
        }
        let peer_tags = &input.peer_tags[..input
            .peer_tags
            .len()
            .min(MAX_PEER_TAGS - additional_tag_count)];

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

        // Waiting on a slot that is being written is only worth it while somebody is actually
        // writing it. A stale SLOT_INIT is never cleared - the flush skips markers rather than
        // reclaiming slots it may not own - so an unbounded wait can be held open by one write
        // a peer performs once, and in the sidecar this runs on the IPC handler's own thread.
        // Past the budget the probe keeps looking but stops waiting; a span is lost, not the
        // caller. The budget spans the whole call, so it also bounds a table poisoned throughout.
        let mut waits = 0u32;
        let key = SlotKey {
            fixed: &input.fixed,
            peer_tags,
            additional_tags: &additional_tags[..additional_tag_count],
        };
        let limited = key.is_cardinality_limited();
        let limit = limited.then_some(input.additional_tags_cardinality_limit);
        let inserted = Self::insert(&bucket, &key, input, limit, &mut waits);
        let masked = match inserted {
            Insert::Done => false,
            Insert::OverLimit => true,
            // A full table is no reason to drop the span when its masked entry exists.
            Insert::Lost => limited,
        };
        let recorded = if masked {
            // The span still counts, under its additional metric tag keys with masked values.
            for tag in &mut additional_tags[..additional_tag_count] {
                tag.1 = TRACER_BLOCKED_VALUE;
            }
            let key = SlotKey {
                fixed: &input.fixed,
                peer_tags,
                additional_tags: &additional_tags[..additional_tag_count],
            };
            matches!(
                Self::insert(&bucket, &key, input, None, &mut waits),
                Insert::Done
            )
        } else {
            matches!(inserted, Insert::Done)
        };

        // A span the table could not take at all had no value substituted either.
        let mut log_blocked = false;
        if recorded && (masked || oversized_key.is_some()) {
            if masked {
                bucket.hdr.additional_tag_collapsed.fetch_add(1, Relaxed);
            }
            if oversized_key.is_some() {
                bucket.hdr.additional_tag_oversized.fetch_add(1, Relaxed);
            }
            log_blocked = bucket.hdr.additional_tag_warned.swap(1, Relaxed) == 0;
        }

        bucket.hdr.in_flight.fetch_sub(1, Release);

        // Logged after leaving the bucket, so that a flush does not wait on the log.
        if log_blocked {
            if let Some(key) = oversized_key {
                warn!(
                    "additional_metric_tags: value for key '{key}' exceeds \
                     {ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN} characters; substituting \
                     {TRACER_BLOCKED_VALUE}. Further blocked values in this stats bucket are not \
                     logged"
                );
            } else {
                let keys: Vec<&str> = additional_tags[..additional_tag_count]
                    .iter()
                    .map(|&(k, _)| k)
                    .collect();
                if matches!(inserted, Insert::Lost) {
                    warn!(
                        "additional_metric_tags: the stats bucket has no free slot left for a new \
                         entry; substituting {TRACER_BLOCKED_VALUE} for the values of {keys:?}. \
                         Further blocked values in this stats bucket are not logged"
                    );
                } else {
                    warn!(
                        "additional_metric_tags: the stats bucket holds its limit of {} entries \
                         with additional metric tags, or waiting for room timed out; \
                         substituting {TRACER_BLOCKED_VALUE} for the values of {keys:?}. Further \
                         blocked values in this stats bucket are not logged",
                        input.additional_tags_cardinality_limit
                    );
                }
            }
        }
    }

    /// Count `input` into the slot holding `key`, claiming a free slot when the key is new.
    ///
    /// A new key must fit under `limit` entries with unmasked additional metric tags, unless
    /// `limit` is `None`. A key already in the bucket is found before any free slot, so it keeps
    /// counting whatever the limit. `waits` counts the iterations spent waiting on slots that are
    /// being written, out of a budget of [`MAX_SLOT_WAIT_ITERS`] for the whole `add_span` call
    /// (see [`wait_for_slot`]).
    fn insert(
        bucket: &Bucket<'_>,
        key: &SlotKey<'_>,
        input: &ShmSpanInput<'_>,
        limit: Option<u32>,
        waits: &mut u32,
    ) -> Insert {
        let hash = hash_key(key);
        let slots = bucket.entries.len();
        // `Layout` guarantees a nonzero slot count, so this is not a division by zero, and
        // every index below stays inside the entry slice.
        let mut slot = (hash % slots as u64) as usize;
        for _ in 0..slots {
            let entry = &bucket.entries[slot];

            loop {
                match entry.key_hash.load(Acquire) {
                    SLOT_EMPTY => {
                        // A claimed slot has to be filled with this key, so take the room under
                        // the limit first, and give it back if another writer claims the slot.
                        if let Some(limit) = limit {
                            match bucket.hdr.take_room(limit) {
                                Room::Taken => {}
                                // The writer holding the last room may be about to fill this
                                // very slot with this very key: look again once it had a chance.
                                Room::Pending => {
                                    if wait_for_slot(waits) {
                                        continue;
                                    }
                                    // Technically we may extremely rarely not get a slot due to a
                                    // regular timeout, but generally, this is a protection against
                                    // a killed concurrent writer.
                                    return Insert::OverLimit;
                                }
                                // Unless the last room went to this key, in this slot: room is
                                // committed after the key is stored, so the slot shows it now.
                                Room::Full => {
                                    if entry.key_hash.load(Acquire) == SLOT_EMPTY {
                                        return Insert::OverLimit;
                                    }
                                    continue;
                                }
                            }
                        }
                        if entry
                            .key_hash
                            .compare_exchange(SLOT_EMPTY, SLOT_INIT, Acquire, Relaxed)
                            .is_ok()
                        {
                            Self::write_key(bucket, entry, key);
                            // Release on the store synchronises the key write with any
                            // subsequent Acquire load of the hash — no separate fence needed.
                            entry.key_hash.store(hash, Release);
                            if limit.is_some() {
                                bucket.hdr.commit_room();
                            }
                            Self::update_stats(entry, input);
                            return Insert::Done;
                        }
                        if limit.is_some() {
                            bucket.hdr.release_room();
                        }
                        if !wait_for_slot(waits) {
                            break;
                        }
                    }
                    SLOT_INIT => {
                        if !wait_for_slot(waits) {
                            break;
                        }
                    }
                    h if h == hash => {
                        if Self::key_matches(bucket, entry, key) {
                            Self::update_stats(entry, input);
                            return Insert::Done;
                        }
                        break;
                    }
                    _ => break, // hash collision, probe next
                }
            }

            slot = (slot + 1) % slots;
        }
        Insert::Lost
    }

    /// Write `key` into a slot this thread has just claimed.
    fn write_key(bucket: &Bucket<'_>, entry: &ShmEntry, key: &SlotKey<'_>) {
        let k = &entry.key;
        let cursor = &bucket.hdr.string_cursor;
        for (wire, s) in k.strs.iter().zip(fixed_strs(key.fixed)) {
            wire.store(alloc_str(bucket.pool, cursor, s));
        }
        // `add_span` keeps both tag counts within the arrays; zipped rather than indexed all
        // the same, so the arrays themselves are the limit.
        for (&(tk, tv), (wire_k, wire_v)) in key.tags().zip(k.tag_keys.iter().zip(&k.tag_values)) {
            wire_k.store(alloc_str(bucket.pool, cursor, tk));
            wire_v.store(alloc_str(bucket.pool, cursor, tv));
        }
        k.store_scalars(key.fixed, key.peer_tags.len(), key.additional_tags.len());
    }

    /// Does this slot hold `key`?
    ///
    /// A slot whose scalars do not decode, or whose strings do not resolve inside the pool,
    /// does not match: `add_span` then probes on rather than folding its span into an entry
    /// whose key it could not read.
    fn key_matches(bucket: &Bucket<'_>, entry: &ShmEntry, key: &SlotKey<'_>) -> bool {
        let k = &entry.key;
        let Some(scalars) = k.scalars() else {
            return false;
        };
        let f = key.fixed;
        scalars.http_status_code == f.http_status_code
            && scalars.grpc_status_code == f.grpc_status_code
            && scalars.is_synthetics_request == f.is_synthetics_request
            && scalars.is_trace_root == f.is_trace_root
            && scalars.peer_tag_count == key.peer_tags.len()
            && scalars.additional_tag_count == key.additional_tags.len()
            && k.strs
                .iter()
                .zip(fixed_strs(f))
                .all(|(wire, s)| pool_eq(bucket.pool, wire.snapshot(), s))
            && key.tags().zip(k.tag_keys.iter().zip(&k.tag_values)).all(
                |(&(ik, iv), (wire_k, wire_v))| {
                    pool_eq(bucket.pool, wire_k.snapshot(), ik)
                        && pool_eq(bucket.pool, wire_v.snapshot(), iv)
                },
            )
    }

    fn update_stats(entry: &ShmEntry, input: &ShmSpanInput<'_>) {
        let s = &entry.stats;
        s.hits.fetch_add(1, Relaxed);
        if input.is_error {
            s.errors.fetch_add(1, Relaxed);
        }
        s.duration_sum.fetch_add(input.duration_ns as u64, Relaxed);
        if input.is_top_level {
            s.top_level_hits.fetch_add(1, Relaxed);
        }
        let bin = bin_for_duration(input.duration_ns);
        if input.is_error {
            s.error_bins[bin].fetch_add(1, Relaxed);
        } else {
            s.ok_bins[bin].fetch_add(1, Relaxed);
        }
    }

    fn slot_usage(&self) -> (usize, usize) {
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

    fn drain_buckets(&self, force: bool) -> Drained {
        let mut drained = Drained::default();
        let blocked = &mut drained.blocked_additional_tags;

        if force {
            for idx in 0u8..2 {
                if let Some(b) = self.drain_bucket(idx, blocked) {
                    drained.buckets.push(b);
                }
            }
        } else {
            let old_active = self.header().active_idx.fetch_xor(1, AcqRel);
            if let Some(b) = self.drain_bucket(old_active, blocked) {
                drained.buckets.push(b);
            }
        }

        drained
    }

    fn flush(
        &self,
        force: bool,
        hostname: String,
        env: String,
        version: String,
        service: String,
        runtime_id: String,
    ) -> Option<pb::ClientStatsPayload> {
        let stat_buckets = self.drain_buckets(force).buckets;
        if stat_buckets.is_empty() {
            return None;
        }

        let seq = self.header().flush_seq.fetch_add(1, Relaxed);
        Some(pb::ClientStatsPayload {
            hostname,
            env,
            version,
            stats: stat_buckets,
            runtime_id,
            service,
            sequence: seq,
            ..Default::default()
        })
    }

    /// Drain one bucket, adding its spans with blocked additional metric tag values to
    /// `blocked_additional_tags`.
    fn drain_bucket(
        &self,
        bucket_idx: u8,
        blocked_additional_tags: &mut BlockedAdditionalTags,
    ) -> Option<pb::ClientStatsBucket> {
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

        let mut grouped: Vec<pb::ClientGroupedStats> = Vec::new();

        for entry in bucket.entries {
            let h = entry.key_hash.load(Relaxed);
            if h == SLOT_EMPTY || h == SLOT_INIT {
                continue;
            }

            if let Some(gs) = Self::drain_entry(entry, bucket.pool, &mut budget)
                && gs.hits > 0
            {
                grouped.push(gs);
            }
            entry.key_hash.store(SLOT_EMPTY, Release);
        }

        // The cardinality limit and its warning apply per bucket: the next one starts afresh.
        bucket.hdr.additional_tag_room.store(0, Relaxed);
        bucket.hdr.additional_tag_warned.store(0, Relaxed);
        let take =
            |counter: &AtomicU32| usize::try_from(counter.swap(0, Relaxed)).unwrap_or(usize::MAX);
        blocked_additional_tags.collapsed += take(&bucket.hdr.additional_tag_collapsed);
        blocked_additional_tags.oversized += take(&bucket.hdr.additional_tag_oversized);
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

        if grouped.is_empty() {
            return None;
        }

        Some(pb::ClientStatsBucket {
            start: bucket_start_ts,
            duration: bucket_size_nanos,
            stats: grouped,
            agent_time_shift: 0,
        })
    }

    /// Take one slot's stats and clear them, building the group to export.
    ///
    /// Drained with `swap`: reading and then clearing would drop whatever arrived in between,
    /// and clearing in bulk would mean a non-atomic write over counters that workers may still
    /// be incrementing.
    ///
    /// `None` when the slot's key is not one this writer could have produced - the entry is
    /// dropped rather than the whole flush. Its stats are drained even then, since leaving
    /// them behind would fold them into whichever key next occupies the slot.
    fn drain_entry(
        entry: &ShmEntry,
        pool: &Pool,
        budget: &mut usize,
    ) -> Option<pb::ClientGroupedStats> {
        let s = &entry.stats;
        // fence(Acquire) in drain_bucket's spin-wait loop already synchronises these reads.
        let hits = s.hits.swap(0, Relaxed);
        let errors = s.errors.swap(0, Relaxed);
        let duration_sum = s.duration_sum.swap(0, Relaxed);
        let top_level_hits = s.top_level_hits.swap(0, Relaxed);

        let mut ok_sketch = DDSketch::default();
        let mut err_sketch = DDSketch::default();
        for bin in 0..N_BINS {
            let ok_count = s.ok_bins[bin].swap(0, Relaxed);
            let err_count = s.error_bins[bin].swap(0, Relaxed);
            let rep = bin_representative(bin);
            if ok_count > 0 {
                let _ = ok_sketch.add_with_count(rep, ok_count as f64);
            }
            if err_count > 0 {
                let _ = err_sketch.add_with_count(rep, err_count as f64);
            }
        }

        let k = &entry.key;
        let scalars = k.scalars()?;
        // In fixed_strs order.
        let [
            resource,
            service,
            name,
            span_type,
            span_kind,
            http_method,
            http_endpoint,
            source,
        ] = std::array::from_fn::<String, FIXED_STRS, _>(|i| {
            pool_string(pool, k.strs[i].snapshot(), budget)
        });

        let mut tags = k
            .tag_keys
            .iter()
            .zip(&k.tag_values)
            .map(|(wire_k, wire_v)| {
                let tag_key = pool_string(pool, wire_k.snapshot(), budget);
                let tag_value = pool_string(pool, wire_v.snapshot(), budget);
                format!("{tag_key}:{tag_value}")
            });
        let peer_tags: Vec<String> = tags.by_ref().take(scalars.peer_tag_count).collect();
        let additional_metric_tags: Vec<String> = tags.take(scalars.additional_tag_count).collect();

        Some(pb::ClientGroupedStats {
            service,
            name,
            resource,
            http_status_code: scalars.http_status_code,
            r#type: span_type,
            db_type: String::new(),
            hits,
            errors,
            duration: duration_sum,
            ok_summary: ok_sketch.encode_to_vec(),
            error_summary: err_sketch.encode_to_vec(),
            synthetics: scalars.is_synthetics_request,
            top_level_hits,
            span_kind,
            peer_tags,
            is_trace_root: scalars.is_trace_root.into(),
            http_method,
            http_endpoint,
            grpc_status_code: scalars
                .grpc_status_code
                .map(|c| c.to_string())
                .unwrap_or_default(),
            service_source: source,
            span_derived_primary_tags: vec![],
            additional_metric_tags,
        })
    }
}

/// Spans whose additional metric tag values were replaced by [`TRACER_BLOCKED_VALUE`].
#[derive(Default)]
struct BlockedAdditionalTags {
    /// For lack of room under the cardinality limit, or in the table.
    collapsed: usize,
    /// For being too long.
    oversized: usize,
}

/// What draining yields: the buckets, and the spans in them with blocked additional metric tags.
#[derive(Default)]
struct Drained {
    buckets: Vec<pb::ClientStatsBucket>,
    blocked_additional_tags: BlockedAdditionalTags,
}

impl FlushableConcentrator for ShmSpanConcentrator {
    fn flush_buckets(&mut self, force: bool) -> FlushResult<pb::ClientStatsBucket> {
        // The SHM concentrator does not perform client-side obfuscation, and of the cardinality
        // limits it only applies the additional metric tags ones.
        let drained = self.drain(force);
        let mut collapsed_fields_metrics = CollapsedFieldsMetrics::zero();
        let mut additional_tags = CollapsedFieldSet::empty();
        additional_tags.add(CollapsedFieldSet::ADDITIONAL_TAGS);
        collapsed_fields_metrics.add(additional_tags, drained.blocked_additional_tags.collapsed);
        collapsed_fields_metrics
            .add_oversized_additional_tags(drained.blocked_additional_tags.oversized);
        FlushResult {
            obfuscated_buckets: vec![],
            unobfuscated_buckets: drained.buckets,
            collapsed_spans: 0,
            collapsed_fields_metrics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn mapping(c: &ShmSpanConcentrator) -> Arc<ShmStatsMapping> {
        c.mem.load_full().unwrap()
    }

    fn test_path() -> CString {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        CString::new(format!(
            "/ddtrace-shm-t-{}-{}",
            unsafe { libc::getpid() },
            COUNTER.fetch_add(1, SeqCst)
        ))
        .unwrap()
    }

    fn span<'a>(service: &'a str, resource: &'a str, dur: i64) -> ShmSpanInput<'a> {
        ShmSpanInput {
            fixed: FixedAggregationKey {
                service_name: service,
                resource_name: resource,
                operation_name: "op",
                span_type: "web",
                span_kind: "server",
                http_method: "GET",
                http_endpoint: "/",
                service_source: "",
                http_status_code: 200,
                is_synthetics_request: false,
                is_trace_root: pb::Trilean::True,
                grpc_status_code: None,
            },
            peer_tags: &[],
            additional_tags: &[],
            additional_tags_cardinality_limit: 100,
            duration_ns: dur,
            is_error: false,
            is_top_level: true,
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_add_and_flush() {
        let c = ShmSpanConcentrator::create(
            test_path(),
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        c.add_span(&span("svc", "res", 1_000_000));
        c.add_span(&span("svc", "res", 2_000_000));
        let bytes = c.flush(
            true,
            "h".into(),
            "e".into(),
            "v".into(),
            "s".into(),
            "r".into(),
        );
        assert!(bytes.is_some());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_open_from_worker() {
        let path = test_path();
        let creator = ShmSpanConcentrator::create(
            path.clone(),
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        let worker = ShmSpanConcentrator::open(path.as_c_str()).unwrap();
        worker.add_span(&span("svc2", "res2", 5_000_000));
        let bytes = creator.flush(
            true,
            "h".into(),
            "".into(),
            "".into(),
            "".into(),
            "r".into(),
        );
        assert!(bytes.is_some());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_needs_reload() {
        let path = test_path();
        let creator = ShmSpanConcentrator::create(
            path.clone(),
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        let worker = ShmSpanConcentrator::open(path.as_c_str()).unwrap();
        assert!(!worker.needs_reload());
        creator.signal_reload();
        assert!(worker.needs_reload());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn workers_follow_replacements_while_owners_keep_the_old_mapping() {
        let path = test_path();
        let old = default_concentrator(path.clone());
        let old_flusher = old.clone();
        let worker = ShmSpanConcentrator::open(path.as_c_str()).unwrap();
        worker.add_span(&span("svc", "old", 1_000));

        let new = ShmSpanConcentrator::create(path.clone(), 20_000_000_000, 8, 4096).unwrap();
        assert!(
            worker.needs_reload(),
            "the old concentrator's workers must be told to reopen"
        );
        assert!(!new.needs_reload());

        assert!(
            new.flush(
                true,
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new()
            )
            .is_none(),
            "the new concentrator must not have inherited the old one's buckets"
        );
        worker.add_span(&span("svc", "new", 1_000));
        let flushed = flush_one(&new);
        assert_eq!(
            flushed.stats.iter().map(|b| b.stats.len()).sum::<usize>(),
            1
        );
        assert_eq!(flushed.stats[0].stats[0].resource, "new");
        assert_eq!(flushed.stats[0].duration, 20_000_000_000);
        assert_eq!(worker.slot_usage().1, 8);

        let flushed = flush_one(&old_flusher);
        assert_eq!(flushed.stats[0].stats.len(), 1);
        assert_eq!(flushed.stats[0].stats[0].resource, "old");
        assert_eq!(flushed.stats[0].duration, 10_000_000_000);

        // The retired owner cleaning up late must not take the name from its successor.
        old.unlink();
        drop(old);
        assert!(ShmSpanConcentrator::open(path.as_c_str()).is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_retries_missing_and_retired_mappings() {
        let path = test_path();
        let worker = ShmSpanConcentrator::new_reader(path.clone());
        let input = span("svc", "res", 1_000);
        assert!(!worker.add_span(&input));

        let old = default_concentrator(path.clone());
        assert!(worker.add_span(&input));
        assert_eq!(flush_one(&old).stats[0].stats[0].hits, 1);

        old.signal_reload();
        assert!(!worker.add_span(&input));
        old.unlink();
        assert!(!worker.add_span(&input));

        let new = default_concentrator(path);
        assert!(worker.add_span(&input));
        assert_eq!(flush_one(&new).stats[0].stats[0].hits, 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn concurrent_workers_follow_a_replacement() {
        let path = test_path();
        let _old = default_concentrator(path.clone());
        let worker = ShmSpanConcentrator::open(&path).unwrap();
        let new = ShmSpanConcentrator::create(path, 20_000_000_000, 8, 4096).unwrap();
        let start = std::sync::Barrier::new(4);

        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    start.wait();
                    for _ in 0..100 {
                        assert!(worker.add_span(&span("svc", "res", 1_000)));
                    }
                });
            }
        });
        let flushed = flush_one(&new);
        assert_eq!(flushed.stats[0].stats.len(), 1);
        assert_eq!(flushed.stats[0].stats[0].hits, 400);
    }

    #[test]
    fn test_histogram_bins() {
        assert_eq!(bin_for_duration(0), 0);
        assert_eq!(bin_for_duration(-1), 0);
        assert!(bin_for_duration(1) >= 1);
        assert_eq!(bin_for_duration(MAX_DURATION_NS as i64), N_BINS - 1);
        assert_eq!(bin_for_duration(i64::MAX), N_BINS - 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_empty() {
        let c = ShmSpanConcentrator::create(
            test_path(),
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        assert!(
            c.flush(
                false,
                "h".into(),
                "e".into(),
                "v".into(),
                "s".into(),
                "r".into()
            )
            .is_none()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_slot_usage() {
        let c = ShmSpanConcentrator::create(
            test_path(),
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        let (used, total) = c.slot_usage();
        assert_eq!(used, 0);
        assert_eq!(total, DEFAULT_SLOT_COUNT);

        c.add_span(&span("svc", "res1", 1_000));
        c.add_span(&span("svc", "res2", 2_000));
        let (used2, _) = c.slot_usage();
        assert_eq!(used2, 2);
    }
    /// The slots that hold a written key, in slot order.
    fn occupied<'a>(bucket: &'a Bucket<'a>) -> Vec<&'a ShmEntry> {
        bucket
            .entries
            .iter()
            .filter(|entry| {
                let h = entry.key_hash.load(Relaxed);
                h != SLOT_EMPTY && h != SLOT_INIT
            })
            .collect()
    }

    fn default_concentrator(path: CString) -> ShmSpanConcentrator {
        ShmSpanConcentrator::create(
            path,
            10_000_000_000,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap()
    }

    fn flush_one(c: &ShmSpanConcentrator) -> pb::ClientStatsPayload {
        c.flush(
            true,
            "h".into(),
            "e".into(),
            "v".into(),
            "s".into(),
            "r".into(),
        )
        .expect("a recorded span should still flush")
    }

    /// A layout describes a segment or it does not; there is no partial acceptance, because
    /// every offset and loop bound in the module is derived from one.
    #[test]
    fn a_layout_is_accepted_only_if_it_fits_the_mapping() {
        let page = page_size::get();
        let entry = size_of::<ShmEntry>();
        let region = (bucket_hdr_size() + 4 * entry + 64).div_ceil(page) * page;
        let total = page + 2 * region;

        assert!(Layout::new(10, 4, 64, region as u32, total).is_some());
        // One byte short of holding both bucket regions.
        assert!(Layout::new(10, 4, 64, region as u32, total - 1).is_none());
        // More slots than the region it claims to live in can hold.
        let overfull = 4 + (region / entry) as u32 + 1;
        assert!(Layout::new(10, overfull, 64, region as u32, total).is_none());
        // A zero bucket width is a division by zero at flush; a zero slot count is one in
        // add_span's probe.
        assert!(Layout::new(0, 4, 64, region as u32, total).is_none());
        assert!(Layout::new(10, 0, 64, region as u32, total).is_none());
        // A region size that would leave bucket 1's slots misaligned.
        assert!(Layout::new(10, 4, 64, region as u32 + 1, total + 2).is_none());
        // Dimensions whose arithmetic overflows are rejected, not wrapped.
        assert!(Layout::new(10, u32::MAX, u32::MAX, u32::MAX, usize::MAX).is_none());
    }

    /// An opener has nothing but the header to learn the layout from, so that is where it has
    /// to be checked rather than trusted.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_header_that_does_not_describe_the_mapping_is_refused() {
        let path = test_path();
        let c = default_concentrator(path.clone());

        mapping(&c).header().slot_count.store(1 << 20, Relaxed);
        assert!(ShmSpanConcentrator::open(path.as_c_str()).is_err());

        mapping(&c)
            .header()
            .slot_count
            .store(DEFAULT_SLOT_COUNT as u32, Relaxed);
        assert!(ShmSpanConcentrator::open(path.as_c_str()).is_ok());

        mapping(&c).header().bucket_size_nanos.store(0, Relaxed);
        assert!(ShmSpanConcentrator::open(path.as_c_str()).is_err());
    }

    /// The dimensions in the header are peer-writable, and the flush loop's bound, its slot
    /// addresses and its divisor would all follow from them. A handle that has already
    /// validated its layout keeps using it, so rewriting the header moves nothing.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_rewritten_header_does_not_move_an_open_handle() {
        let path = test_path();
        let c = default_concentrator(path.clone());
        let worker = ShmSpanConcentrator::open(path.as_c_str()).unwrap();
        worker.add_span(&span("svc", "res", 1_000_000));

        let mem = mapping(&c);
        let hdr = mem.header();
        hdr.slot_count.store(1 << 20, Relaxed);
        hdr.bucket_region_size.store(!7, Relaxed);
        hdr.string_pool_size.store(u32::MAX, Relaxed);
        hdr.bucket_size_nanos.store(0, Relaxed);
        hdr.active_idx.store(200, Relaxed);

        assert_eq!(
            c.slot_usage().1,
            DEFAULT_SLOT_COUNT,
            "the slot count was taken from the segment"
        );

        let payload = flush_one(&c);
        assert_eq!(payload.stats.len(), 1);
        assert_eq!(payload.stats[0].stats.len(), 1);
        assert_eq!(
            payload.stats[0].duration, 10_000_000_000,
            "the bucket width was taken from the segment"
        );
    }

    /// `bool` and `Trilean` have more bit patterns than they have values, and the key holding
    /// them is peer-writable: loading one as its Rust type would be undefined behaviour before
    /// anything got to inspect it. They are decoded instead, and an entry that fails to decode
    /// costs only itself.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn malformed_scalars_drop_only_their_own_entry() {
        let c = default_concentrator(test_path());
        for resource in ["a", "b", "c", "d", "e", "f", "g"] {
            c.add_span(&span("svc", resource, 1_000_000));
        }

        let mem = mapping(&c);
        let bucket = mem.bucket(0);
        let entries = occupied(&bucket);
        assert_eq!(entries.len(), 7);
        entries[0].key.is_trace_root.store(7, Relaxed);
        entries[1].key.is_synthetics_request.store(9, Relaxed);
        entries[2].key.peer_tag_count.store(200, Relaxed);
        entries[3].key.grpc_status_code.store(300, Relaxed);
        entries[4]
            .key
            .additional_tag_count
            .store(MAX_ADDITIONAL_TAGS as u8 + 1, Relaxed);
        // Each count is fine on its own, but together they overrun the shared arrays.
        entries[5]
            .key
            .peer_tag_count
            .store((MAX_PEER_TAGS - MAX_ADDITIONAL_TAGS + 1) as u8, Relaxed);
        entries[5]
            .key
            .additional_tag_count
            .store(MAX_ADDITIONAL_TAGS as u8, Relaxed);

        let payload = flush_one(&c);
        assert_eq!(
            payload.stats[0].stats.len(),
            1,
            "only the entry that still decodes should be exported"
        );
    }

    /// A string reference's offset and length are a peer's to write, and the flush copies the
    /// bytes they name into an owned string bound for the agent. Their bounds are the pool's -
    /// not the mapping's, since a reference landing elsewhere in the segment would still be
    /// reading another bucket's slots - and a reference that misses yields nothing.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn string_references_outside_the_pool_export_nothing() {
        let c = default_concentrator(test_path());
        c.add_span(&span("svc", "res", 1_000_000));

        {
            let mem = mapping(&c);
            let bucket = mem.bucket(0);
            let entries = occupied(&bucket);
            // strs[0] is resource_name and strs[1] service_name; see fixed_strs.
            entries[0].key.strs[0]
                .offset
                .store(DEFAULT_STRING_POOL_BYTES as u32 - 2, Relaxed);
            entries[0].key.strs[0].len.store(1 << 20, Relaxed);
            entries[0].key.strs[1].offset.store(0, Relaxed);
            entries[0].key.strs[1].len.store(u32::MAX, Relaxed);
        }

        let payload = flush_one(&c);
        let group = &payload.stats[0].stats[0];
        assert_eq!(group.resource, "");
        assert_eq!(group.service, "");
        assert_eq!(group.name, "op", "an untouched field still exports");
        assert_eq!(group.hits, 1);
    }

    #[test]
    fn pool_copies_stay_in_bounds_and_export_owned_strings() {
        let storage = [const { AtomicU8::new(b'?') }; 16];
        let pool = &storage[4..12];
        let cursor = AtomicU32::new(5);
        let sr = alloc_str(pool, &cursor, "€");
        assert_eq!((sr.offset, sr.len), (5, 3));

        let mut budget = 3;
        let owned = pool_string(pool, sr, &mut budget);
        assert_eq!(owned, "€");
        assert_eq!(budget, 0);

        pool[5].store(0xff, Relaxed);
        pool[6].store(b'x', Relaxed);
        pool[7].store(b'y', Relaxed);
        let mut budget = 3;
        assert_eq!(pool_string(pool, sr, &mut budget), "\u{fffd}xy");
        assert_eq!(budget, 0);
        assert_eq!(owned, "€", "exported strings must own their bytes");

        let mut budget = 2;
        assert!(pool_string(pool, sr, &mut budget).is_empty());
        assert_eq!(budget, 2);

        assert_eq!(alloc_str(pool, &cursor, "!").len, 0);
        cursor.store(u32::MAX, Relaxed);
        assert_eq!(alloc_str(pool, &cursor, "!").len, 0);
        assert!(
            storage[..9]
                .iter()
                .chain(&storage[12..])
                .all(|b| b.load(Relaxed) == b'?')
        );
    }

    /// Bounding each reference to the pool still leaves a peer free to repeat one large
    /// reference across every field of every slot. The pool's capacity is the most real data a
    /// bucket can hold, so it is also the budget for what one flush copies out of it.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn one_large_reference_repeated_across_a_key_is_bounded() {
        const POOL: usize = 4096;
        let c = ShmSpanConcentrator::create(test_path(), 10_000_000_000, 8, POOL).unwrap();
        c.add_span(&span("svc", "res", 1_000_000));

        {
            let mem = mapping(&c);
            let bucket = mem.bucket(0);
            let entries = occupied(&bucket);
            let key = &entries[0].key;
            for wire in key.strs.iter().chain(&key.tag_keys).chain(&key.tag_values) {
                wire.offset.store(0, Relaxed);
                wire.len.store(POOL as u32, Relaxed);
            }
            key.peer_tag_count.store(MAX_PEER_TAGS as u8, Relaxed);
        }

        let payload = flush_one(&c);
        let group = &payload.stats[0].stats[0];
        let exported: usize = [
            &group.service,
            &group.name,
            &group.resource,
            &group.r#type,
            &group.span_kind,
            &group.http_method,
            &group.http_endpoint,
            &group.service_source,
        ]
        .iter()
        .map(|s| s.len())
        .sum::<usize>()
            + group.peer_tags.iter().map(|t| t.len()).sum::<usize>();

        // Forty references of POOL bytes each would be 160 KiB. One pool's worth may come out,
        // times the three bytes an invalid byte expands to as a replacement character.
        assert!(
            exported <= 3 * POOL,
            "exported {exported} bytes from a {POOL}-byte pool"
        );
    }
    /// A slot's marker is peer-writable, and `SLOT_INIT` means "a writer is part-way through
    /// this slot". Nothing ever clears a stale one - the flush skips it - so a marker left set
    /// must not be able to hold `add_span` in that slot forever: in the sidecar `add_span` runs
    /// on the IPC handler's own thread.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_permanent_slot_marker_does_not_trap_add_span() {
        let c = default_concentrator(test_path());
        for entry in mapping(&c).bucket(0).entries {
            entry.key_hash.store(SLOT_INIT, Relaxed);
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let caller = c.clone();
        std::thread::spawn(move || {
            caller.add_span(&span("svc", "res", 1_000_000));
            let _ = tx.send(());
        });

        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok(),
            "add_span never returned from slots whose markers stay set"
        );
    }
    /// The order of the fixed string fields is written down twice - in `fixed_strs` and in
    /// `drain_entry`'s destructuring - so give all eight distinct values and check each comes
    /// back where it went in. Adding the same span twice also exercises the matching path,
    /// which compares those fields in that same order.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_key_round_trips_every_fixed_string_field() {
        let c = default_concentrator(test_path());
        let input = ShmSpanInput {
            fixed: FixedAggregationKey {
                resource_name: "the-resource",
                service_name: "the-service",
                operation_name: "the-operation",
                span_type: "the-type",
                span_kind: "the-kind",
                http_method: "the-method",
                http_endpoint: "the-endpoint",
                service_source: "the-source",
                http_status_code: 418,
                is_synthetics_request: true,
                is_trace_root: pb::Trilean::False,
                grpc_status_code: Some(7),
            },
            peer_tags: &[("peer.service", "upstream")],
            additional_tags: &[],
            additional_tags_cardinality_limit: 100,
            duration_ns: 1_000_000,
            is_error: true,
            is_top_level: true,
        };
        c.add_span(&input);
        c.add_span(&input);

        let payload = flush_one(&c);
        assert_eq!(
            payload.stats[0].stats.len(),
            1,
            "the second span did not match the first one's slot"
        );
        let group = &payload.stats[0].stats[0];
        assert_eq!(group.resource, "the-resource");
        assert_eq!(group.service, "the-service");
        assert_eq!(group.name, "the-operation");
        assert_eq!(group.r#type, "the-type");
        assert_eq!(group.span_kind, "the-kind");
        assert_eq!(group.http_method, "the-method");
        assert_eq!(group.http_endpoint, "the-endpoint");
        assert_eq!(group.service_source, "the-source");
        assert_eq!(group.http_status_code, 418);
        assert_eq!(group.grpc_status_code, "7");
        assert!(group.synthetics);
        assert_eq!(group.is_trace_root, pb::Trilean::False as i32);
        assert_eq!(group.peer_tags, vec!["peer.service:upstream"]);
        assert_eq!(group.hits, 2);
        assert_eq!(group.errors, 2);
    }

    fn tagged<'a>(
        resource: &'a str,
        additional_tags: &'a [(&'a str, &'a str)],
        additional_tags_cardinality_limit: u32,
    ) -> ShmSpanInput<'a> {
        ShmSpanInput {
            additional_tags,
            additional_tags_cardinality_limit,
            ..span("svc", resource, 1_000_000)
        }
    }

    /// Spans blocked as `(collapsed, oversized)`.
    fn blocked(drained: &Drained) -> (usize, usize) {
        let blocked = &drained.blocked_additional_tags;
        (blocked.collapsed, blocked.oversized)
    }

    /// Every group as "[its additional metric tags] x<hits>", sorted.
    fn additional_tags_of(drained: &Drained) -> Vec<String> {
        let mut groups: Vec<String> = drained
            .buckets
            .iter()
            .flat_map(|b| &b.stats)
            .map(|g| format!("[{}] x{}", g.additional_metric_tags.join(", "), g.hits))
            .collect();
        groups.sort();
        groups
    }

    /// The memory budget for additional metric tags is nil: they share the peer tag arrays, and
    /// their count sits in what used to be the key's tail padding.
    #[test]
    fn additional_tags_do_not_grow_a_slot() {
        assert_eq!(size_of::<ShmKeyHeader>(), 332);
        assert_eq!(size_of::<ShmEntry>(), 4472);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn additional_tags_are_part_of_the_key() {
        let c = default_concentrator(test_path());
        let tags = [("region", "eu"), ("tenant", "acme")];
        c.add_span(&tagged("res", &tags, 100));
        c.add_span(&tagged("res", &tags, 100));
        c.add_span(&tagged("res", &[("region", "us")], 100));
        c.add_span(&tagged("res", &[], 100));

        let drained = c.drain(true);
        assert_eq!(
            additional_tags_of(&drained),
            ["[] x1", "[region:eu, tenant:acme] x2", "[region:us] x1"]
        );
        assert_eq!(blocked(&drained), (0, 0));
    }

    /// Peer tags get whatever room the additional metric tags leave in the shared arrays, and
    /// each kind drains back into its own field.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn peer_tags_and_additional_tags_share_the_slot_arrays() {
        let c = default_concentrator(test_path());
        let peer_tags: Vec<String> = (0..MAX_PEER_TAGS).map(|i| format!("peer.{i:02}")).collect();
        let peer_tags: Vec<(&str, &str)> = peer_tags.iter().map(|k| (k.as_str(), "v")).collect();
        let input = ShmSpanInput {
            peer_tags: &peer_tags,
            additional_tags: &[("region", "eu"), ("tenant", "acme")],
            ..span("svc", "res", 1_000_000)
        };
        c.add_span(&input);
        c.add_span(&input);

        let payload = flush_one(&c);
        assert_eq!(payload.stats[0].stats.len(), 1);
        let group = &payload.stats[0].stats[0];
        assert_eq!(group.hits, 2);
        assert_eq!(group.peer_tags.len(), MAX_PEER_TAGS - 2);
        assert_eq!(group.peer_tags[0], "peer.00:v");
        assert_eq!(group.additional_metric_tags, ["region:eu", "tenant:acme"]);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn oversized_additional_tag_values_are_blocked() {
        let c = default_concentrator(test_path());
        // The limit counts characters, not bytes.
        let longest = "é".repeat(ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN);
        let too_long = "x".repeat(ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN + 1);
        c.add_span(&tagged("a", &[("tenant", &longest)], 100));
        c.add_span(&tagged(
            "b",
            &[("region", "eu"), ("tenant", &too_long)],
            100,
        ));

        let drained = c.drain(true);
        assert_eq!(
            additional_tags_of(&drained),
            [
                format!("[region:eu, tenant:{TRACER_BLOCKED_VALUE}] x1"),
                format!("[tenant:{longest}] x1"),
            ]
        );
        assert_eq!(blocked(&drained), (0, 1));
    }

    /// Past the limit a new key keeps its additional metric tag keys with masked values, while
    /// keys admitted earlier in the bucket keep counting as themselves.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn the_cardinality_limit_masks_only_new_keys() {
        let c = default_concentrator(test_path());
        for tenant in ["a", "b", "c", "a", "d"] {
            c.add_span(&tagged("res", &[("tenant", tenant)], 2));
        }
        // Another set of keys over the limit is masked into an entry of its own.
        c.add_span(&tagged("res", &[("region", "eu"), ("tenant", "e")], 2));
        // Keys without additional metric tags are not subject to the limit.
        c.add_span(&tagged("res", &[], 2));
        c.add_span(&tagged("other", &[], 2));

        let drained = c.drain(true);
        assert_eq!(
            additional_tags_of(&drained),
            [
                "[] x1".to_owned(),
                "[] x1".to_owned(),
                format!("[region:{TRACER_BLOCKED_VALUE}, tenant:{TRACER_BLOCKED_VALUE}] x1"),
                "[tenant:a] x2".to_owned(),
                "[tenant:b] x1".to_owned(),
                format!("[tenant:{TRACER_BLOCKED_VALUE}] x2"),
            ]
        );
        assert_eq!(blocked(&drained), (3, 0));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn the_cardinality_limit_resets_with_each_bucket() {
        let c = default_concentrator(test_path());
        c.add_span(&tagged("res", &[("tenant", "a")], 1));
        c.add_span(&tagged("res", &[("tenant", "b")], 1));
        let first = c.drain(false);
        assert_eq!(
            additional_tags_of(&first),
            [
                "[tenant:a] x1".to_owned(),
                format!("[tenant:{TRACER_BLOCKED_VALUE}] x1"),
            ]
        );
        assert_eq!(blocked(&first), (1, 0));

        // The other bucket, then the drained one again: both start with the whole limit.
        c.add_span(&tagged("res", &[("tenant", "b")], 1));
        let second = c.drain(false);
        assert_eq!(additional_tags_of(&second), ["[tenant:b] x1"]);
        assert_eq!(blocked(&second), (0, 0));

        c.add_span(&tagged("res", &[("tenant", "c")], 1));
        let third = c.drain(true);
        assert_eq!(additional_tags_of(&third), ["[tenant:c] x1"]);
        assert_eq!(blocked(&third), (0, 0));
    }

    /// The telemetry for blocked values travels with the flushed buckets: values over the limit
    /// as collapsed, and values too long as oversized, which is no cardinality limit.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn blocked_additional_tags_are_reported_with_the_flush() {
        let mut c = default_concentrator(test_path());
        let too_long = "x".repeat(ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN + 1);
        c.add_span(&tagged("res", &[("tenant", "a")], 1));
        c.add_span(&tagged("res", &[("tenant", "b")], 1));
        c.add_span(&tagged("res", &[("tenant", &too_long)], 1));
        c.add_span(&tagged("res", &[("tenant", &too_long)], 1));
        let flushed = c.flush_buckets(true);
        assert_eq!(flushed.unobfuscated_buckets[0].stats.len(), 2);
        let mut expected = CollapsedFieldsMetrics::zero();
        let mut additional_tags = CollapsedFieldSet::empty();
        additional_tags.add(CollapsedFieldSet::ADDITIONAL_TAGS);
        expected.add(additional_tags, 1);
        expected.add_oversized_additional_tags(2);
        assert_eq!(
            format!("{:?}", flushed.collapsed_fields_metrics),
            format!("{expected:?}")
        );
    }

    /// A key no free slot is left for is masked like one over the limit, so it still counts if
    /// its masked entry exists.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_full_table_masks_new_keys_rather_than_dropping_them() {
        let c = ShmSpanConcentrator::create(test_path(), 10_000_000_000, 4, 4096).unwrap();
        for tenant in ["a", "b", "c"] {
            c.add_span(&tagged("res", &[("tenant", tenant)], 2));
        }
        c.add_span(&tagged("other", &[], 2));
        // All four slots are taken, one of them by the masked entry.
        c.add_span(&tagged("res", &[("tenant", "d")], 2));

        let drained = c.drain(true);
        assert_eq!(
            additional_tags_of(&drained),
            [
                "[] x1".to_owned(),
                "[tenant:a] x1".to_owned(),
                "[tenant:b] x1".to_owned(),
                format!("[tenant:{TRACER_BLOCKED_VALUE}] x2"),
            ]
        );
        assert_eq!(blocked(&drained), (2, 0));
    }

    /// A span the full table cannot take even masked was dropped, so it blocked no value.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_span_a_full_table_drops_is_not_counted_as_blocked() {
        let c = ShmSpanConcentrator::create(test_path(), 10_000_000_000, 4, 4096).unwrap();
        for resource in ["a", "b", "c", "d"] {
            c.add_span(&tagged(resource, &[], 100));
        }
        let too_long = "x".repeat(ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN + 1);
        c.add_span(&tagged("res", &[("tenant", "a")], 100));
        c.add_span(&tagged("res", &[("tenant", &too_long)], 100));

        let drained = c.drain(true);
        assert_eq!(additional_tags_of(&drained), ["[] x1"; 4]);
        assert_eq!(blocked(&drained), (0, 0));
    }

    /// Writers racing to add the same new key must all count into it: room another writer has
    /// taken under the limit, but not used yet, is not the limit being reached.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn concurrent_writers_of_one_new_key_are_not_masked() {
        const WRITERS: usize = 8;
        let c = default_concentrator(test_path());
        for _ in 0..1000 {
            let start = std::sync::Barrier::new(WRITERS);
            std::thread::scope(|scope| {
                for _ in 0..WRITERS {
                    scope.spawn(|| {
                        start.wait();
                        c.add_span(&tagged("res", &[("tenant", "a")], 1));
                    });
                }
            });
            assert_eq!(additional_tags_of(&c.drain(false)), ["[tenant:a] x8"]);
        }
    }

    #[test]
    fn an_owned_input_lends_out_both_kinds_of_tags() {
        let owned = OwnedShmSpanInput {
            fixed: Default::default(),
            peer_tags: vec![("peer.service".into(), "upstream".into())],
            additional_tags: vec![("tenant".into(), "acme".into())],
            additional_tags_cardinality_limit: 7,
            duration_ns: 1,
            is_error: false,
            is_top_level: true,
        };
        let mut tag_buf = Vec::new();
        let input = owned.as_shm_input(&mut tag_buf);
        assert_eq!(input.peer_tags, [("peer.service", "upstream")]);
        assert_eq!(input.additional_tags, [("tenant", "acme")]);
        assert_eq!(input.additional_tags_cardinality_limit, 7);
    }
}
