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
//! 2. Sets `ShmHeader::please_reload = 1` on the **old** mapping so workers know to re-open the
//!    path on their next `add_span` call.
//! 3. Holds onto the old concentrator for ≥ 1 s, flushing it periodically, to absorb any spans that
//!    arrived before workers noticed the reload flag.
//! 4. Drops the old concentrator after that grace period.

use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::hash::{Hash, Hasher};
use std::hint;
use std::io;
use std::sync::atomic::{fence, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering::*};
use std::sync::Arc;
use std::thread;
use zwohash::ZwoHasher;

use libdd_ddsketch::DDSketch;
use libdd_trace_protobuf::pb;
use libdd_trace_stats::span_concentrator::{FixedAggregationKey, FlushableConcentrator};

use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle};

const SHM_VERSION: u32 = 1;

/// Maximum peer-tag (key, value) pairs per aggregation slot.
pub const MAX_PEER_TAGS: usize = 16;

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
/// Used by [`FixedAggregationKey<StringRef>`] when the key is stored in shared memory.
/// Both `offset == 0 && len == 0` and `offset != 0` are valid; a zero-length slice
/// represents an absent / empty string.
#[repr(C)]
#[derive(Clone, Copy, Default, Hash, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct StringRef {
    pub offset: u32,
    pub len: u32,
}

/// Aggregation key – position-independent, no raw pointers.
///
/// The fixed string fields and scalar fields are grouped in `FixedAggregationKey<StringRef>`.
/// Peer-tag arrays follow separately because they are variable-count and not part of the
/// generic key struct.
#[repr(C)]
struct ShmKeyHeader {
    fixed: FixedAggregationKey<StringRef>,
    peer_tag_keys: [StringRef; MAX_PEER_TAGS],
    peer_tag_values: [StringRef; MAX_PEER_TAGS],
    peer_tag_count: u8,
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
    key: UnsafeCell<ShmKeyHeader>,
    stats: ShmStats,
}

// SAFETY: ShmEntry lives entirely in shared-memory; all mutations go through
// the atomic protocol described in the module doc.
unsafe impl Sync for ShmEntry {}

/// Per-bucket control header.
#[repr(C)]
struct ShmBucketHeader {
    start_nanos: AtomicU64,
    in_flight: AtomicI64,
    string_cursor: AtomicU32,
}

/// Global SHM header (first page of the mapping).
#[repr(C)]
struct ShmHeader {
    /// Layout version; checked by [`ShmSpanConcentrator::open`].  Mismatch returns an error.
    version: u32,
    /// Width of each time bucket in nanoseconds (e.g. 10 s = 10_000_000_000).
    bucket_size_nanos: u64,
    /// Number of aggregation slots per bucket (hash-table capacity).
    slot_count: u32,
    /// Byte size of one full bucket region (header + slots + string pool), page-aligned.
    bucket_region_size: u32,
    /// Byte capacity of the per-bucket string pool.
    string_pool_size: u32,
    /// Index (0 or 1) of the bucket currently being written to by PHP workers.
    active_idx: AtomicU8,
    /// Set to 1 by the sidecar when workers should re-open the SHM at the
    /// same path (a new, larger mapping has been created there).
    please_reload: AtomicU8,
    /// Monotonic counter incremented on every successful flush, used as the stats sequence number.
    flush_seq: AtomicU64,
}

fn bucket_hdr_size() -> usize {
    // Align to 8 bytes (AtomicU64 alignment).
    let s = size_of::<ShmBucketHeader>();
    (s + 7) & !7
}

fn pool_start_within_bucket(slot_count: u32) -> usize {
    bucket_hdr_size() + (slot_count as usize) * size_of::<ShmEntry>()
}

fn aligned_bucket_region(slot_count: u32, string_pool_size: u32) -> usize {
    let raw = pool_start_within_bucket(slot_count) + string_pool_size as usize;
    let page = page_size::get();
    raw.div_ceil(page) * page
}

fn total_shm_size(slot_count: u32, string_pool_size: u32) -> usize {
    page_size::get() + 2 * aligned_bucket_region(slot_count, string_pool_size)
}

fn bucket_start(bucket_idx: u8, bucket_region_size: u32) -> usize {
    page_size::get() + bucket_idx as usize * bucket_region_size as usize
}

unsafe fn shm_header(base: *const u8) -> &'static ShmHeader {
    &*(base as *const ShmHeader)
}

unsafe fn bucket_header(base: *const u8, bkt_start: usize) -> &'static ShmBucketHeader {
    &*(base.add(bkt_start) as *const ShmBucketHeader)
}

unsafe fn entry_ref(base: *const u8, bkt_start: usize, slot: usize) -> &'static ShmEntry {
    let p = base.add(bkt_start + bucket_hdr_size() + slot * size_of::<ShmEntry>());
    &*(p as *const ShmEntry)
}

unsafe fn pool_base(base: *const u8, bkt_start: usize, slot_count: u32) -> *const u8 {
    base.add(bkt_start + pool_start_within_bucket(slot_count))
}

unsafe fn sref_str<'a>(pool: *const u8, sr: StringRef) -> &'a str {
    if sr.len == 0 {
        return "";
    }
    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
        pool.add(sr.offset as usize),
        sr.len as usize,
    ))
}

fn hash_key(input: &ShmSpanInput<'_>) -> u64 {
    let mut h = ZwoHasher::default();
    input.fixed.hash(&mut h);
    for &(k, v) in input.peer_tags {
        k.hash(&mut h);
        v.hash(&mut h);
    }
    match h.finish() {
        SLOT_EMPTY => 1,
        SLOT_INIT => SLOT_INIT - 1,
        v => v,
    }
}

unsafe fn key_matches(entry: &ShmEntry, input: &ShmSpanInput<'_>, pool: *const u8) -> bool {
    let k = &*entry.key.get();
    k.fixed.convert(|sr| unsafe { sref_str(pool, *sr) }) == input.fixed
        && (k.peer_tag_count as usize) == input.peer_tags.len()
        && input.peer_tags.iter().enumerate().all(|(i, &(ik, iv))| {
            sref_str(pool, k.peer_tag_keys[i]) == ik && sref_str(pool, k.peer_tag_values[i]) == iv
        })
}

unsafe fn alloc_str(pool: *mut u8, cursor: &AtomicU32, pool_size: u32, s: &str) -> StringRef {
    let len = s.len() as u32;
    if len == 0 {
        return StringRef::default();
    }
    let mut spins = 0u32;
    loop {
        let old = cursor.load(Relaxed);
        let new = old.saturating_add(len);
        if new > pool_size {
            return StringRef::default();
        }
        if cursor
            .compare_exchange_weak(old, new, Relaxed, Relaxed)
            .is_ok()
        {
            std::ptr::copy_nonoverlapping(s.as_ptr(), pool.add(old as usize), len as usize);
            return StringRef { offset: old, len };
        }
        spins += 1;
        if spins % YIELD_AFTER_SPINS == 0 {
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
    /// (key, value) peer-tag pairs (capped at `MAX_PEER_TAGS` by the caller).
    pub peer_tags: &'a [(&'a str, &'a str)],
    // stats
    pub duration_ns: i64,
    pub is_error: bool,
    pub is_top_level: bool,
}

/// Owned (serializable) version of [`ShmSpanInput`].
///
/// Used as the IPC fallback payload when the PHP side cannot open the SHM concentrator yet
/// (e.g. on the very first request, before the sidecar has processed
/// `set_universal_service_tags` and created the SHM file).  The sidecar handler receives
/// this struct, writes to the now-existing SHM concentrator, and the span is counted.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct OwnedShmSpanInput {
    pub fixed: FixedAggregationKey<String>,
    pub peer_tags: Vec<(String, String)>,
    pub duration_ns: i64,
    pub is_error: bool,
    pub is_top_level: bool,
}

impl OwnedShmSpanInput {
    /// Borrow as a [`ShmSpanInput`] for passing to [`ShmSpanConcentrator::add_span`].
    ///
    /// `peer_tag_buf` is a caller-supplied scratch buffer; it must outlive the returned value.
    pub fn as_shm_input<'a>(
        &'a self,
        peer_tag_buf: &'a mut Vec<(&'a str, &'a str)>,
    ) -> ShmSpanInput<'a> {
        peer_tag_buf.clear();
        for (k, v) in &self.peer_tags {
            peer_tag_buf.push((k.as_str(), v.as_str()));
        }
        ShmSpanInput {
            fixed: self.fixed.convert(|s: &str| s),
            peer_tags: peer_tag_buf.as_slice(),
            duration_ns: self.duration_ns,
            is_error: self.is_error,
            is_top_level: self.is_top_level,
        }
    }
}

/// Shared-memory span stats concentrator.
///
/// Created once by the sidecar; opened (read-write) by each PHP worker.
#[derive(Clone)]
pub struct ShmSpanConcentrator {
    mem: Arc<MappedMem<NamedShmHandle>>,
}

unsafe impl Send for ShmSpanConcentrator {}
unsafe impl Sync for ShmSpanConcentrator {}

impl ShmSpanConcentrator {
    /// Create a new SHM concentrator (sidecar side).
    ///
    /// Unlinks any pre-existing SHM file at `path` before creating the new one.
    pub fn create(
        path: CString,
        bucket_size_nanos: u64,
        slot_count: usize,
        string_pool_bytes: usize,
    ) -> io::Result<Self> {
        let slot_count = slot_count.max(1) as u32;
        let string_pool_size = string_pool_bytes as u32;
        let total = total_shm_size(slot_count, string_pool_size);

        // Remove any stale mapping at this path (ignore errors).
        #[cfg(unix)]
        unsafe {
            libc::shm_unlink(path.as_ptr());
        }

        let handle = NamedShmHandle::create(path, total)?;
        let mut mem = handle.map()?;

        let base = mem.as_slice_mut().as_mut_ptr();
        unsafe {
            // fresh mmap. Initialized to zero.
            let hdr = &mut *(base as *mut ShmHeader);
            hdr.version = SHM_VERSION;
            hdr.bucket_size_nanos = bucket_size_nanos;
            hdr.slot_count = slot_count;
            hdr.bucket_region_size = aligned_bucket_region(slot_count, string_pool_size) as u32;
            hdr.string_pool_size = string_pool_size;
        }

        Ok(ShmSpanConcentrator { mem: Arc::new(mem) })
    }

    /// Open an existing SHM concentrator (PHP worker side).
    pub fn open(path: &CStr) -> io::Result<Self> {
        let handle = NamedShmHandle::open(path)?;
        let mem = handle.map()?;

        let base = mem.as_slice().as_ptr();
        unsafe {
            let hdr = shm_header(base);
            if hdr.version != SHM_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SHM span concentrator: incompatible version",
                ));
            }
        }

        Ok(ShmSpanConcentrator { mem: Arc::new(mem) })
    }

    fn base_ptr(&self) -> *const u8 {
        self.mem.as_slice().as_ptr()
    }

    fn header(&self) -> &ShmHeader {
        unsafe { shm_header(self.base_ptr()) }
    }

    /// Returns `true` when the sidecar has signalled that workers should
    /// re-open the SHM at the same path (a larger mapping has been created).
    ///
    /// Workers should call this before every `add_span`; when it returns `true`
    /// they should drop this handle, call `open(path)`, and retry.
    pub fn needs_reload(&self) -> bool {
        self.header().please_reload.load(Acquire) != 0
    }

    /// Add a span to the currently-active bucket.  Thread-safe.
    pub fn add_span(&self, input: &ShmSpanInput<'_>) {
        let hdr = self.header();
        let slot_count = hdr.slot_count;
        let brs = hdr.bucket_region_size;
        let pool_size = hdr.string_pool_size;
        let base = self.base_ptr();

        // Claim in-flight on the active bucket, with double-check against swap.
        let active = hdr.active_idx.load(Acquire);
        let bkt_start = bucket_start(active, brs);
        let bh = unsafe { bucket_header(base, bkt_start) };
        bh.in_flight.fetch_add(1, Acquire);

        let (bkt_start, bh) = {
            let active2 = hdr.active_idx.load(Acquire);
            if active2 != active {
                bh.in_flight.fetch_sub(1, Release);
                let s2 = bucket_start(active2, brs);
                let h2 = unsafe { bucket_header(base, s2) };
                h2.in_flight.fetch_add(1, Acquire);
                (s2, h2)
            } else {
                (bkt_start, bh)
            }
        };

        let hash = hash_key(input);
        let pool = unsafe { pool_base(base, bkt_start, slot_count) as *mut u8 };

        let mut slot = (hash as usize) % slot_count as usize;
        let mut done = false;
        for _ in 0..slot_count {
            let entry = unsafe { entry_ref(base, bkt_start, slot) };

            let mut spins = 0u32;
            loop {
                match entry.key_hash.load(Acquire) {
                    SLOT_EMPTY => {
                        if entry
                            .key_hash
                            .compare_exchange(SLOT_EMPTY, SLOT_INIT, Acquire, Relaxed)
                            .is_ok()
                        {
                            unsafe {
                                Self::write_key(entry, input, pool, &bh.string_cursor, pool_size);
                            }
                            // Release on the store synchronises the key write with any
                            // subsequent Acquire load of the hash — no separate fence needed.
                            entry.key_hash.store(hash, Release);
                            Self::update_stats(entry, input);
                            done = true;
                            break;
                        }
                        spins += 1;
                        if spins % YIELD_AFTER_SPINS == 0 {
                            thread::yield_now();
                        } else {
                            hint::spin_loop();
                        }
                    }
                    SLOT_INIT => {
                        spins += 1;
                        if spins % YIELD_AFTER_SPINS == 0 {
                            thread::yield_now();
                        } else {
                            hint::spin_loop();
                        }
                    }
                    h if h == hash => {
                        if unsafe { key_matches(entry, input, pool) } {
                            Self::update_stats(entry, input);
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
            slot = (slot + 1) % slot_count as usize;
        }

        bh.in_flight.fetch_sub(1, Release);
    }

    unsafe fn write_key(
        entry: &ShmEntry,
        input: &ShmSpanInput<'_>,
        pool: *mut u8,
        cursor: &AtomicU32,
        pool_size: u32,
    ) {
        let k = &mut *entry.key.get();
        let fi = &input.fixed;
        k.fixed = fi.convert(|s| unsafe { alloc_str(pool, cursor, pool_size, s) });
        let n = input.peer_tags.len().min(MAX_PEER_TAGS);
        k.peer_tag_count = n as u8;
        for (i, &(tk, tv)) in input.peer_tags[..n].iter().enumerate() {
            k.peer_tag_keys[i] = alloc_str(pool, cursor, pool_size, tk);
            k.peer_tag_values[i] = alloc_str(pool, cursor, pool_size, tv);
        }
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

    /// Returns `(used_slots, total_slots)` for the currently-active bucket.
    ///
    /// The sidecar uses this to decide when to recreate with more slots.
    pub fn slot_usage(&self) -> (usize, usize) {
        let hdr = self.header();
        let active = hdr.active_idx.load(Acquire);
        let bkt_start = bucket_start(active, hdr.bucket_region_size);
        let base = self.base_ptr();
        let slot_count = hdr.slot_count as usize;

        let used = (0..slot_count)
            .filter(|&s| {
                let h = unsafe { entry_ref(base, bkt_start, s) }
                    .key_hash
                    .load(Relaxed);
                h != SLOT_EMPTY && h != SLOT_INIT
            })
            .count();

        (used, slot_count)
    }

    /// Signal workers to re-open the SHM (call before creating a new, larger one).
    pub fn signal_reload(&self) {
        self.header().please_reload.store(1, Release);
    }

    /// Drain the inactive (or both, if `force`) bucket(s) and return raw stat buckets.
    ///
    /// This is the low-level building block used by both [`flush`] and the
    /// [`FlushableConcentrator`] impl.
    pub fn drain_buckets(&self, force: bool) -> Vec<pb::ClientStatsBucket> {
        let hdr = self.header();
        let slot_count = hdr.slot_count;
        let brs = hdr.bucket_region_size;
        let pool_size = hdr.string_pool_size;
        let bucket_nanos = hdr.bucket_size_nanos;

        let mut stat_buckets: Vec<pb::ClientStatsBucket> = Vec::new();

        if force {
            for idx in 0u8..2 {
                if let Some(b) = self.drain_bucket(idx, slot_count, brs, pool_size, bucket_nanos) {
                    stat_buckets.push(b);
                }
            }
        } else {
            let old_active = hdr.active_idx.fetch_xor(1, AcqRel);
            if let Some(b) = self.drain_bucket(old_active, slot_count, brs, pool_size, bucket_nanos)
            {
                stat_buckets.push(b);
            }
        }

        stat_buckets
    }

    /// Flush and return a serialised `ClientStatsPayload`, or `None` if empty.
    ///
    /// * `force = false` – swap the active bucket, drain the previously-active one.
    /// * `force = true`  – drain both buckets without swapping (shutdown).
    #[allow(clippy::too_many_arguments)]
    pub fn flush(
        &self,
        force: bool,
        hostname: String,
        env: String,
        version: String,
        service: String,
        runtime_id: String,
    ) -> Option<pb::ClientStatsPayload> {
        let stat_buckets = self.drain_buckets(force);
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

    fn drain_bucket(
        &self,
        bucket_idx: u8,
        slot_count: u32,
        bucket_region_size: u32,
        _pool_size: u32,
        bucket_size_nanos: u64,
    ) -> Option<pb::ClientStatsBucket> {
        let base = self.base_ptr();
        let bkt_start = bucket_start(bucket_idx, bucket_region_size);
        let bh = unsafe { bucket_header(base, bkt_start) };

        // Wait for in-flight writers (bounded to tolerate dead workers).
        // The intermediate loads only need Relaxed; a single fence(Acquire) after
        // the loop synchronizes with the Release in each writer's in_flight.fetch_sub,
        // and covers all subsequent SHM reads in this function and callees.
        let mut spins = 0u32;
        while bh.in_flight.load(Relaxed) != 0 && spins < MAX_FLUSH_WAIT_ITERS {
            spins += 1;
            if spins % YIELD_AFTER_SPINS == 0 {
                thread::yield_now();
            } else {
                hint::spin_loop();
            }
        }
        fence(Acquire);

        let bucket_start_ts = bh.start_nanos.load(Relaxed);
        let pool = unsafe { pool_base(base, bkt_start, slot_count) };

        let mut grouped: Vec<pb::ClientGroupedStats> = Vec::new();

        for slot in 0..slot_count as usize {
            let entry = unsafe { entry_ref(base, bkt_start, slot) };
            let h = entry.key_hash.load(Relaxed);
            if h == SLOT_EMPTY || h == SLOT_INIT {
                continue;
            }

            let gs = unsafe { Self::read_entry(entry, pool) };
            if gs.hits > 0 {
                grouped.push(gs);
            }

            unsafe {
                std::ptr::write_bytes(std::ptr::addr_of!(entry.stats) as *mut ShmStats, 0, 1);
            }
            entry.key_hash.store(SLOT_EMPTY, Release);
        }

        bh.string_cursor.store(0, Release);

        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        bh.start_nanos
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

    unsafe fn read_entry(entry: &ShmEntry, pool: *const u8) -> pb::ClientGroupedStats {
        let k = &*entry.key.get();
        let f = &k.fixed;
        let s = &entry.stats;

        macro_rules! read_str {
            ($sref:expr) => {{
                let r: StringRef = $sref;
                if r.len == 0 {
                    String::new()
                } else {
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        pool.add(r.offset as usize),
                        r.len as usize,
                    ))
                    .into_owned()
                }
            }};
        }

        let peer_tags: Vec<String> = (0..k.peer_tag_count as usize)
            .map(|i| {
                format!(
                    "{}:{}",
                    read_str!(k.peer_tag_keys[i]),
                    read_str!(k.peer_tag_values[i])
                )
            })
            .collect();

        // fence(Acquire) in drain_bucket's spin-wait loop already synchronises these reads.
        let hits = s.hits.load(Relaxed);
        let errors = s.errors.load(Relaxed);
        let duration_sum = s.duration_sum.load(Relaxed);
        let top_level_hits = s.top_level_hits.load(Relaxed);

        let mut ok_sketch = DDSketch::default();
        let mut err_sketch = DDSketch::default();
        for bin in 0..N_BINS {
            let ok_count = s.ok_bins[bin].load(Relaxed);
            let err_count = s.error_bins[bin].load(Relaxed);
            let rep = bin_representative(bin);
            if ok_count > 0 {
                let _ = ok_sketch.add_with_count(rep, ok_count as f64);
            }
            if err_count > 0 {
                let _ = err_sketch.add_with_count(rep, err_count as f64);
            }
        }

        pb::ClientGroupedStats {
            service: read_str!(f.service_name),
            name: read_str!(f.operation_name),
            resource: read_str!(f.resource_name),
            http_status_code: f.http_status_code,
            r#type: read_str!(f.span_type),
            db_type: String::new(),
            hits,
            errors,
            duration: duration_sum,
            ok_summary: ok_sketch.encode_to_vec(),
            error_summary: err_sketch.encode_to_vec(),
            synthetics: f.is_synthetics_request,
            top_level_hits,
            span_kind: read_str!(f.span_kind),
            peer_tags,
            is_trace_root: if f.is_trace_root {
                pb::Trilean::True.into()
            } else {
                pb::Trilean::False.into()
            },
            http_method: read_str!(f.http_method),
            http_endpoint: read_str!(f.http_endpoint),
            grpc_status_code: f
                .grpc_status_code
                .map(|c| c.to_string())
                .unwrap_or_default(),
            service_source: read_str!(f.service_source),
            span_derived_primary_tags: vec![],
        }
    }
}

impl FlushableConcentrator for ShmSpanConcentrator {
    fn flush_buckets(&mut self, force: bool) -> Vec<pb::ClientStatsBucket> {
        self.drain_buckets(force)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

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
                is_trace_root: true,
                grpc_status_code: None,
            },
            peer_tags: &[],
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
        assert!(c
            .flush(
                false,
                "h".into(),
                "e".into(),
                "v".into(),
                "s".into(),
                "r".into()
            )
            .is_none());
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
}
