// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free shared-memory span stats concentrator.
//!
//! All PHP worker processes open the same SHM file and call
//! `ShmSpanConcentrator::add_span`.  The sidecar is the creator
//! (`ShmSpanConcentrator::create`) and periodically calls
//! `ShmSpanConcentrator::flush` to drain the inactive bucket.
//!
//! The table itself - layout, slot lifecycle, double-buffering, and what may and may not be
//! trusted in the mapping - is the shared table's (`shm_table`); this module is the [`SpanStats`]
//! schema that it lays out: the aggregation key of a span, and the counters and duration histograms
//! kept for each.
//!
//! ## Additional metric tags
//! They share the peer tag arrays of a slot. A bucket admits at most
//! `additional_tags_cardinality_limit` entries with unmasked additional metric tags, under the
//! table's limit on new keys. A key over the limit, or one no free slot is left for, has its
//! values replaced with `TRACER_BLOCKED_VALUE` and is added again. Masked keys are always
//! admitted.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering::*};
use tracing::warn;
use zwohash::ZwoHasher;

use libdd_ddsketch::DDSketch;
use libdd_trace_protobuf::pb;
use libdd_trace_stats::span_concentrator::{
    ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN, ADDITIONAL_METRIC_TAGS_MAX_KEYS, FixedAggregationKey,
    FlushResult, FlushableConcentrator, TRACER_BLOCKED_VALUE, additional_metric_tag_value_too_long,
    cardinality_limit_telemetry::{CollapsedFieldSet, CollapsedFieldsMetrics},
};

use crate::shm_table::{
    self, Insert, Pool, Schema, ShmSafe, ShmTable, ShmTableMapping, WireStringRef, alloc_str,
    pool_eq, pool_string,
};
pub use crate::shm_table::{RELOAD_FILL_RATIO, StringRef};

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

/// Default aggregation slots per bucket.
pub const DEFAULT_SLOT_COUNT: usize = 256;
/// Default per-bucket string pool size.
pub const DEFAULT_STRING_POOL_BYTES: usize = 512 * 1024;

/// Shared-memory span stats concentrator.
pub type ShmSpanConcentrator = ShmTable<SpanStats>;

type ShmEntry = shm_table::ShmEntry<SpanStats>;

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
/// travel as raw integers and become Rust types in `ShmKeyHeader::scalars`, after checking.
#[repr(C)]
pub struct ShmKeyHeader {
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

// SAFETY: `#[repr(C)]` and made only of integer atomics and `ShmSafe` arrays of them.
unsafe impl ShmSafe for ShmKeyHeader {}

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
pub struct ShmStats {
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

// SAFETY: `#[repr(C)]` and made only of integer atomics and arrays of them.
unsafe impl ShmSafe for ShmStats {}

/// What a bucket keeps of the spans whose additional metric tag values it blocked.
#[repr(C)]
pub struct ShmStatsBucketState {
    /// Spans whose additional metric tag values were replaced by [`TRACER_BLOCKED_VALUE`] for
    /// lack of room under the cardinality limit, or in the table.
    additional_tag_collapsed: AtomicU32,
    /// Spans with additional metric tag values replaced for being too long.
    additional_tag_oversized: AtomicU32,
    /// Set by the first writer blocking additional metric tag values, which alone logs it.
    additional_tag_warned: AtomicU8,
}

// SAFETY: `#[repr(C)]` and made only of integer atomics.
unsafe impl ShmSafe for ShmStatsBucketState {}

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

/// A span as the table takes it: its key as a slot stores it, and the span for its stats.
pub struct SlotInput<'a> {
    key: SlotKey<'a>,
    span: &'a ShmSpanInput<'a>,
}

/// Schema of the APM client stats table: one slot per span aggregation key.
pub struct SpanStats;

impl Schema for SpanStats {
    const ID: u32 = 1;
    const VERSION: u32 = 2;

    type Key = ShmKeyHeader;
    type Value = ShmStats;
    type BucketState = ShmStatsBucketState;
    type Input<'a> = SlotInput<'a>;

    fn hash(input: &SlotInput<'_>) -> u64 {
        let key = &input.key;
        let mut h = ZwoHasher::default();
        key.fixed.hash(&mut h);
        key.peer_tags.len().hash(&mut h);
        for &(k, v) in key.tags() {
            k.hash(&mut h);
            v.hash(&mut h);
        }
        h.finish()
    }

    /// Write `input`'s key into a slot this thread has just claimed.
    fn write_key(k: &ShmKeyHeader, pool: &Pool, cursor: &AtomicU32, input: &SlotInput<'_>) {
        let key = &input.key;
        for (wire, s) in k.strs.iter().zip(fixed_strs(key.fixed)) {
            wire.store(alloc_str(pool, cursor, s));
        }
        // `add_span` keeps both tag counts within the arrays; zipped rather than indexed all
        // the same, so the arrays themselves are the limit.
        for (&(tk, tv), (wire_k, wire_v)) in key.tags().zip(k.tag_keys.iter().zip(&k.tag_values)) {
            wire_k.store(alloc_str(pool, cursor, tk));
            wire_v.store(alloc_str(pool, cursor, tv));
        }
        k.store_scalars(key.fixed, key.peer_tags.len(), key.additional_tags.len());
    }

    /// Does this slot hold `input`'s key?
    ///
    /// A slot whose scalars do not decode, or whose strings do not resolve inside the pool,
    /// does not match: `add_span` then probes on rather than folding its span into an entry
    /// whose key it could not read.
    fn key_matches(k: &ShmKeyHeader, pool: &Pool, input: &SlotInput<'_>) -> bool {
        let Some(scalars) = k.scalars() else {
            return false;
        };
        let key = &input.key;
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
                .all(|(wire, s)| pool_eq(pool, wire.snapshot(), s))
            && key.tags().zip(k.tag_keys.iter().zip(&k.tag_values)).all(
                |(&(ik, iv), (wire_k, wire_v))| {
                    pool_eq(pool, wire_k.snapshot(), ik) && pool_eq(pool, wire_v.snapshot(), iv)
                },
            )
    }

    fn record(s: &ShmStats, input: &SlotInput<'_>) {
        let input = input.span;
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
}

/// Pre-extracted span stats for one span, ready to be fed into `ShmSpanConcentrator::add_span`.
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
    /// Borrow as a [`ShmSpanInput`] for passing to `ShmSpanConcentrator::add_span`.
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

impl ShmTable<SpanStats> {
    /// Submit a span to shared memory. Returns false if no current mapping is available.
    pub fn add_span(&self, input: &ShmSpanInput<'_>) -> bool {
        self.with_current(|mem| mem.add_span(input)).is_some()
    }

    /// Drain the inactive bucket, or both buckets if `force` is true.
    pub fn drain_buckets(&self, force: bool) -> Vec<pb::ClientStatsBucket> {
        self.drain(force).buckets
    }

    fn drain(&self, force: bool) -> Drained {
        self.with_current(|mem| mem.drain_stats(force))
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

impl ShmTableMapping<SpanStats> {
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

        let (inserted, log_blocked) = self.with_active_bucket(|bucket| {
            // One budget of waits on slots being written for the whole span, masked retry
            // included; see `ShmTableMapping::insert`.
            let mut waits = 0u32;
            let key = SlotKey {
                fixed: &input.fixed,
                peer_tags,
                additional_tags: &additional_tags[..additional_tag_count],
            };
            let limited = key.is_cardinality_limited();
            let limit = limited.then_some(input.additional_tags_cardinality_limit);
            let inserted = Self::insert(bucket, &SlotInput { key, span: input }, limit, &mut waits);
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
                    Self::insert(bucket, &SlotInput { key, span: input }, None, &mut waits),
                    Insert::Done
                )
            } else {
                matches!(inserted, Insert::Done)
            };

            // A span the table could not take at all had no value substituted either.
            let mut log_blocked = false;
            if recorded && (masked || oversized_key.is_some()) {
                let state = &bucket.hdr.schema;
                if masked {
                    state.additional_tag_collapsed.fetch_add(1, Relaxed);
                }
                if oversized_key.is_some() {
                    state.additional_tag_oversized.fetch_add(1, Relaxed);
                }
                log_blocked = state.additional_tag_warned.swap(1, Relaxed) == 0;
            }
            (inserted, log_blocked)
        });

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

    fn drain_stats(&self, force: bool) -> Drained {
        let mut blocked = BlockedAdditionalTags::default();
        let buckets = self
            .drain_buckets_with_state(
                force,
                |entry, pool, budget| {
                    Self::drain_entry(entry, pool, budget).filter(|gs| gs.hits > 0)
                },
                |state| {
                    // The warning applies per bucket: the next interval logs afresh.
                    state.additional_tag_warned.store(0, Relaxed);
                    let take = |counter: &AtomicU32| {
                        usize::try_from(counter.swap(0, Relaxed)).unwrap_or(usize::MAX)
                    };
                    blocked.collapsed += take(&state.additional_tag_collapsed);
                    blocked.oversized += take(&state.additional_tag_oversized);
                },
            )
            .into_iter()
            .map(|b| pb::ClientStatsBucket {
                start: b.start,
                duration: b.duration,
                stats: b.groups,
                agent_time_shift: 0,
            })
            .collect();
        Drained {
            buckets,
            blocked_additional_tags: blocked,
        }
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
        let stat_buckets = self.drain_stats(force).buckets;
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
        let s = &entry.value;
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
            Some(resource),
            Some(service),
            Some(name),
            Some(span_type),
            Some(span_kind),
            Some(http_method),
            Some(http_endpoint),
            Some(source),
        ] = std::array::from_fn::<_, FIXED_STRS, _>(|i| {
            pool_string(pool, k.strs[i].snapshot(), budget)
        })
        else {
            return None;
        };

        let mut tags = k
            .tag_keys
            .iter()
            .zip(&k.tag_values)
            .map(|(wire_k, wire_v)| {
                let tag_key = pool_string(pool, wire_k.snapshot(), budget)?;
                let tag_value = pool_string(pool, wire_v.snapshot(), budget)?;
                Some(format!("{tag_key}:{tag_value}"))
            });
        let peer_tags: Vec<String> = tags
            .by_ref()
            .take(scalars.peer_tag_count)
            .collect::<Option<_>>()?;
        let additional_metric_tags: Vec<String> = tags
            .take(scalars.additional_tag_count)
            .collect::<Option<_>>()?;

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
    use crate::shm_table::{SLOT_EMPTY, SLOT_INIT, bucket_hdr_size};
    use std::ffi::CString;
    use std::sync::Arc;

    type ShmStatsMapping = ShmTableMapping<SpanStats>;
    type Layout = shm_table::Layout<SpanStats>;
    type Bucket<'a> = shm_table::Bucket<'a, SpanStats>;
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
        let region = (bucket_hdr_size::<SpanStats>() + 4 * entry + 64).div_ceil(page) * page;
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
    /// reading another bucket's slots - and an entry with a reference that misses is dropped
    /// rather than exported with that field blank.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn string_references_outside_the_pool_drop_their_entry() {
        let c = default_concentrator(test_path());
        for resource in ["a", "b", "c"] {
            c.add_span(&span("svc", resource, 1_000_000));
        }

        {
            let mem = mapping(&c);
            let bucket = mem.bucket(0);
            let entries = occupied(&bucket);
            assert_eq!(entries.len(), 3);
            // strs[0] is resource_name and strs[1] service_name; see fixed_strs.
            entries[0].key.strs[0]
                .offset
                .store(DEFAULT_STRING_POOL_BYTES as u32 - 2, Relaxed);
            entries[0].key.strs[0].len.store(1 << 20, Relaxed);
            entries[1].key.strs[1].offset.store(0, Relaxed);
            entries[1].key.strs[1].len.store(u32::MAX, Relaxed);
        }

        let payload = flush_one(&c);
        let groups = &payload.stats[0].stats;
        assert_eq!(
            groups.len(),
            1,
            "only the untouched entry should be exported"
        );
        assert_eq!(groups[0].service, "svc");
        assert_eq!(groups[0].hits, 1);
    }

    #[test]
    fn pool_copies_stay_in_bounds_and_export_owned_strings() {
        let storage = [const { AtomicU8::new(b'?') }; 16];
        let pool = &storage[4..12];
        let cursor = AtomicU32::new(5);
        let sr = alloc_str(pool, &cursor, "€");
        assert_eq!((sr.offset, sr.len), (5, 3));

        let mut budget = 3;
        let owned = pool_string(pool, sr, &mut budget).unwrap();
        assert_eq!(owned, "€");
        assert_eq!(budget, 0);

        pool[5].store(0xff, Relaxed);
        pool[6].store(b'x', Relaxed);
        pool[7].store(b'y', Relaxed);
        let mut budget = 3;
        assert_eq!(pool_string(pool, sr, &mut budget).unwrap(), "\u{fffd}xy");
        assert_eq!(budget, 0);
        assert_eq!(owned, "€", "exported strings must own their bytes");

        let mut budget = 2;
        assert_eq!(pool_string(pool, sr, &mut budget), None);
        assert_eq!(budget, 2);

        let outside = StringRef { offset: 6, len: 3 };
        let mut budget = 8;
        assert_eq!(pool_string(pool, outside, &mut budget), None);
        assert_eq!(budget, 8);

        let mut budget = 0;
        assert_eq!(
            pool_string(pool, StringRef::default(), &mut budget).unwrap(),
            ""
        );

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
    /// reference across every slot. The pool's capacity is the most real data a bucket can
    /// hold, so it is also the budget for what one flush copies out of it; entries past it are
    /// dropped.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn one_large_reference_repeated_across_slots_is_bounded() {
        const POOL: usize = 4096;
        let c = ShmSpanConcentrator::create(test_path(), 10_000_000_000, 8, POOL).unwrap();
        for resource in ["a", "b", "c", "d"] {
            c.add_span(&span("svc", resource, 1_000_000));
        }

        {
            let mem = mapping(&c);
            let bucket = mem.bucket(0);
            let entries = occupied(&bucket);
            assert_eq!(entries.len(), 4);
            for entry in entries {
                // strs[0] is resource_name; see fixed_strs.
                entry.key.strs[0].offset.store(0, Relaxed);
                entry.key.strs[0].len.store(POOL as u32 / 2, Relaxed);
            }
        }

        let payload = flush_one(&c);
        let groups = &payload.stats[0].stats;
        let exported: usize = groups
            .iter()
            .flat_map(|g| {
                [
                    &g.service,
                    &g.name,
                    &g.resource,
                    &g.r#type,
                    &g.span_kind,
                    &g.http_method,
                    &g.http_endpoint,
                    &g.service_source,
                ]
                .map(String::len)
                .into_iter()
                .chain(g.peer_tags.iter().map(String::len))
            })
            .sum();

        // Four references of POOL / 2 bytes each would be two pools' worth. One pool's worth
        // may come out, times the three bytes an invalid byte expands to as a replacement
        // character.
        assert_eq!(groups.len(), 1, "entries past the budget should be dropped");
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
