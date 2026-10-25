// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free shared-memory Data Streams Monitoring (DSM) checkpoint table.
//!
//! PHP workers record each checkpoint with `ShmDsmCheckpointTable::add_checkpoint`; the sidecar
//! creates the table and periodically drains it with `ShmDsmCheckpointTable::drain_buckets`. The
//! table itself is the shared one (`shm_table`); this module is the [`DataStreamsCheckpoints`]
//! schema it lays out.
//!
//! ## What a slot holds
//! One slot per checkpoint, keyed by its pathway hash, which already folds in the parent's. As
//! in the Java and Go tracers, the parent hash and edge tags reported for a checkpoint are those
//! of the first input that created it; later inputs with the same pathway hash only add to its
//! histograms.
//!
//! Each checkpoint has three histograms - pathway latency and edge latency (in seconds) and payload
//! size (in bytes) - whose bins are the backend's own DDSketch bins (`BACKEND_GAMMA`,
//! `BACKEND_INDEX_OFFSET`), so turning them into sketches at flush time is exact. The bins cover
//! a fixed range per histogram; values outside it are counted into the nearest end bin.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering::*};

use libdd_ddsketch::DDSketch;

use crate::shm_table::{
    self, Pool, Schema, ShmSafe, ShmTable, ShmTableMapping, WireStringRef, alloc_str, pool_string,
};

/// Gamma of the backend's DDSketch mapping, which the Java and Go tracers also use for DSM.
pub const BACKEND_GAMMA: f64 = 1.015625;
/// Index offset of the backend's DDSketch mapping, which the Java and Go tracers also use.
pub const BACKEND_INDEX_OFFSET: f64 = 1.8761281912861705;

/// Maximum edge tags kept per checkpoint. The Java tracer has 14 tag slots.
pub const MAX_EDGE_TAGS: usize = 16;

/// Width of a DSM time bucket: 10 s, as in the Java and Go tracers.
pub const DSM_BUCKET_NANOS: u64 = 10_000_000_000;
/// Default checkpoints per bucket.
pub const DEFAULT_DSM_CHECKPOINT_SLOT_COUNT: usize = 256;
/// Default per-bucket string pool size, for edge tags.
pub const DEFAULT_DSM_CHECKPOINT_STRING_POOL_BYTES: usize = 256 * 1024;

/// Backend bin of 1 µs, the lowest latency binned on its own.
const LATENCY_MIN_INDEX: i32 = -890;
/// Bins from 1 µs through the one holding 10⁵ s.
const LATENCY_BINS: usize = 1635;
/// Backend bin of 1 byte, the lowest payload size binned on its own.
const PAYLOAD_MIN_INDEX: i32 = 1;
/// Bins from 1 byte through the one holding 10⁹ bytes.
const PAYLOAD_BINS: usize = 1338;

/// Shared-memory DSM checkpoint table.
pub type ShmDsmCheckpointTable = ShmTable<DataStreamsCheckpoints>;

type ShmDsmCheckpointMapping = ShmTableMapping<DataStreamsCheckpoints>;
type ShmEntry = shm_table::ShmEntry<DataStreamsCheckpoints>;

/// The backend bin `value` falls into; `value` must be positive and finite.
///
/// The same arithmetic as `libdd_ddsketch`'s logarithmic mapping, so that a bin numbered here is
/// the bin a sketch with the backend mapping would have put the value in.
fn backend_index(value: f64) -> i32 {
    // Saturating: a value small or large enough to leave the i32 range is clamped anyway.
    (value.ln() * (1.0 / BACKEND_GAMMA.ln()) + BACKEND_INDEX_OFFSET).floor() as i32
}

/// The sketch the backend expects, empty.
fn backend_sketch() -> Option<DDSketch> {
    DDSketch::with_log_mapping(BACKEND_GAMMA, BACKEND_INDEX_OFFSET)
}

/// One histogram's bins as they are stored in the mapping.
///
/// `bins[i]` counts the values in backend bin `MIN + i`; `zero` counts values at or below zero,
/// which a sketch keeps apart from its bins.
#[repr(C)]
pub struct ShmHistogram<const MIN: i32, const BINS: usize> {
    zero: AtomicU32,
    bins: [AtomicU32; BINS],
}

impl<const MIN: i32, const BINS: usize> ShmHistogram<MIN, BINS> {
    fn record(&self, value: f64) {
        if value <= 0.0 || value.is_nan() {
            self.zero.fetch_add(1, Relaxed);
            return;
        }
        let max = MIN + (BINS as i32 - 1);
        let index = if value.is_infinite() {
            max
        } else {
            backend_index(value).clamp(MIN, max)
        };
        // In range by the clamp; `get` only so that no arithmetic slip can become a panic.
        if let Some(bin) = self.bins.get((index - MIN) as usize) {
            bin.fetch_add(1, Relaxed);
        }
    }

    /// Take the counts and clear them, as a sketch with the backend mapping and the number of
    /// values it holds.
    ///
    /// The counts are cleared even when no sketch can be built, so that none are left behind.
    fn drain(&self) -> Option<(DDSketch, u64)> {
        let mut sketch = backend_sketch();
        let zero = self.zero.swap(0, Relaxed);
        let mut total = u64::from(zero);
        if let Some(sketch) = sketch.as_mut().filter(|_| zero > 0) {
            let _ = sketch.add_with_count(0.0, f64::from(zero));
        }
        for (i, bin) in self.bins.iter().enumerate() {
            let count = bin.swap(0, Relaxed);
            if count > 0 {
                total += u64::from(count);
                if let Some(sketch) = sketch.as_mut() {
                    let _ = sketch.add_to_bin(MIN + i as i32, f64::from(count));
                }
            }
        }
        Some((sketch?, total))
    }
}

type LatencyHistogram = ShmHistogram<LATENCY_MIN_INDEX, LATENCY_BINS>;
type PayloadHistogram = ShmHistogram<PAYLOAD_MIN_INDEX, PAYLOAD_BINS>;

/// Per-checkpoint histograms, all integer atomics so that zeroed mmap memory is valid.
#[repr(C, align(8))]
pub struct ShmCheckpointValue {
    /// Seconds since the pathway started.
    pathway_latency: LatencyHistogram,
    /// Seconds since the previous checkpoint.
    edge_latency: LatencyHistogram,
    /// Bytes; inputs without a known size are not counted.
    payload_size: PayloadHistogram,
}

// SAFETY: `#[repr(C)]` and made only of integer atomics and arrays of them.
unsafe impl ShmSafe for ShmCheckpointValue {}

/// Checkpoint key as stored in the mapping.
#[repr(C)]
pub struct ShmCheckpointKey {
    /// What checkpoints are keyed on: the pathway hash.
    hash: AtomicU64,
    parent_hash: AtomicU64,
    edge_tags: [WireStringRef; MAX_EDGE_TAGS],
    /// At most [`MAX_EDGE_TAGS`].
    edge_tag_count: AtomicU8,
}

// SAFETY: `#[repr(C)]` and made only of integer atomics and `ShmSafe` arrays of them.
unsafe impl ShmSafe for ShmCheckpointKey {}

/// Schema of the DSM checkpoint table: one slot per checkpoint.
pub struct DataStreamsCheckpoints;

impl Schema for DataStreamsCheckpoints {
    const ID: u32 = 2;
    const VERSION: u32 = 2;

    type Key = ShmCheckpointKey;
    type Value = ShmCheckpointValue;
    type Input<'a> = DsmCheckpointInput<'a>;

    fn hash(input: &DsmCheckpointInput<'_>) -> u64 {
        input.hash
    }

    fn write_key(
        k: &ShmCheckpointKey,
        pool: &Pool,
        cursor: &AtomicU32,
        input: &DsmCheckpointInput<'_>,
    ) {
        k.hash.store(input.hash, Relaxed);
        k.parent_hash.store(input.parent_hash, Relaxed);
        // Zipped, so a caller passing more tags than there is room for loses the surplus.
        let mut stored = 0u8;
        for (wire, tag) in k.edge_tags.iter().zip(input.edge_tags) {
            wire.store(alloc_str(pool, cursor, tag));
            stored += 1;
        }
        k.edge_tag_count.store(stored, Relaxed);
    }

    /// Only the pathway hash is compared: a checkpoint is its pathway hash, which already folds in
    /// the parent's, and its other key fields are whatever the first input brought.
    fn key_matches(k: &ShmCheckpointKey, _pool: &Pool, input: &DsmCheckpointInput<'_>) -> bool {
        k.hash.load(Relaxed) == input.hash
    }

    fn record(s: &ShmCheckpointValue, input: &DsmCheckpointInput<'_>) {
        s.pathway_latency
            .record(nanos_to_seconds(input.pathway_latency_ns));
        s.edge_latency
            .record(nanos_to_seconds(input.edge_latency_ns));
        // Zero means the size is unknown; an empty histogram says so better than a zero would.
        if input.payload_size_bytes > 0 {
            s.payload_size.record(input.payload_size_bytes as f64);
        }
    }
}

fn nanos_to_seconds(nanos: i64) -> f64 {
    nanos as f64 / 1e9
}

/// One DSM checkpoint, ready to be fed into `ShmDsmCheckpointTable::add_checkpoint`.
pub struct DsmCheckpointInput<'a> {
    /// Pathway hash of the checkpoint; checkpoints are aggregated on it.
    pub hash: u64,
    /// Pathway hash of the previous checkpoint, or 0 for the first one.
    pub parent_hash: u64,
    /// `key:value` edge tags; at most [`MAX_EDGE_TAGS`] are kept.
    pub edge_tags: &'a [&'a str],
    pub pathway_latency_ns: i64,
    pub edge_latency_ns: i64,
    /// Zero when unknown.
    pub payload_size_bytes: i64,
}

/// Owned (serializable) version of [`DsmCheckpointInput`].
///
/// The IPC fallback sends this to the sidecar, which creates the table if needed and adds the
/// checkpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedDsmCheckpoint {
    pub hash: u64,
    pub parent_hash: u64,
    pub edge_tags: Vec<String>,
    pub pathway_latency_ns: i64,
    pub edge_latency_ns: i64,
    pub payload_size_bytes: i64,
}

impl OwnedDsmCheckpoint {
    /// Borrow as a [`DsmCheckpointInput`] for passing to `ShmDsmCheckpointTable::add_checkpoint`.
    ///
    /// `edge_tag_buf` is a caller-supplied scratch buffer; it must outlive the returned value.
    pub fn as_shm_input<'a>(
        &'a self,
        edge_tag_buf: &'a mut Vec<&'a str>,
    ) -> DsmCheckpointInput<'a> {
        edge_tag_buf.clear();
        edge_tag_buf.extend(self.edge_tags.iter().map(String::as_str));
        DsmCheckpointInput {
            hash: self.hash,
            parent_hash: self.parent_hash,
            edge_tags: edge_tag_buf.as_slice(),
            pathway_latency_ns: self.pathway_latency_ns,
            edge_latency_ns: self.edge_latency_ns,
            payload_size_bytes: self.payload_size_bytes,
        }
    }
}

/// One checkpoint drained out of the table.
#[derive(Debug, Clone, PartialEq)]
pub struct DsmCheckpoint {
    pub hash: u64,
    pub parent_hash: u64,
    pub edge_tags: Vec<String>,
    /// Serialized DDSketch protobufs, with the backend mapping.
    pub pathway_latency: Vec<u8>,
    pub edge_latency: Vec<u8>,
    pub payload_size: Vec<u8>,
}

/// The checkpoints of one time bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct DsmCheckpointBucket {
    /// Start of the bucket, in nanoseconds since the Unix epoch, aligned to its duration.
    pub start: u64,
    pub duration: u64,
    pub checkpoints: Vec<DsmCheckpoint>,
}

impl ShmTable<DataStreamsCheckpoints> {
    /// Submit a checkpoint to shared memory. Returns false if no current mapping is available.
    pub fn add_checkpoint(&self, input: &DsmCheckpointInput<'_>) -> bool {
        self.add(input)
    }

    /// Drain the inactive bucket, or both buckets if `force` is true.
    ///
    /// Buckets are dated by when they are drained rather than by the start the table recorded:
    /// a drain that is not forced closes the interval that has just ended, and a forced one also
    /// takes the interval still under way. Draining every [`DSM_BUCKET_NANOS`], just after the
    /// interval boundaries, is what makes those the intervals the checkpoints were recorded in.
    pub fn drain_buckets(&self, force: bool) -> Vec<DsmCheckpointBucket> {
        self.with_current(|mem| mem.drain_checkpoint_buckets(force, now_nanos()))
            .unwrap_or_default()
    }
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

impl ShmDsmCheckpointMapping {
    fn drain_checkpoint_buckets(&self, force: bool, now: u64) -> Vec<DsmCheckpointBucket> {
        self.drain_buckets(force, Self::drain_entry)
            .into_iter()
            .map(|b| {
                // The private layout guarantees a nonzero duration.
                let current = now - now % b.duration;
                DsmCheckpointBucket {
                    start: if b.current {
                        current
                    } else {
                        current.saturating_sub(b.duration)
                    },
                    duration: b.duration,
                    checkpoints: b.groups,
                }
            })
            .collect()
    }

    /// Take one slot's histograms and clear them, building the checkpoint to export.
    ///
    /// `None` when the slot holds no recorded inputs, or its key is not one this writer could have
    /// produced; the histograms are drained either way, so that nothing is left behind for
    /// whichever key next occupies the slot.
    fn drain_entry(entry: &ShmEntry, pool: &Pool, budget: &mut usize) -> Option<DsmCheckpoint> {
        let s = &entry.value;
        // fence(Acquire) in drain_bucket's spin-wait loop already synchronises these reads.
        let (pathway_latency, edge_latency, payload_size) = (
            s.pathway_latency.drain(),
            s.edge_latency.drain(),
            s.payload_size.drain(),
        );
        let ((pathway_latency, recorded), (edge_latency, _), (payload_size, _)) =
            (pathway_latency?, edge_latency?, payload_size?);
        if recorded == 0 {
            return None;
        }

        let k = &entry.key;
        let edge_tag_count = match k.edge_tag_count.load(Relaxed) as usize {
            n if n <= MAX_EDGE_TAGS => n,
            _ => return None,
        };
        let edge_tags = k
            .edge_tags
            .iter()
            .take(edge_tag_count)
            .map(|wire| pool_string(pool, wire.snapshot(), budget))
            .collect();

        Some(DsmCheckpoint {
            hash: k.hash.load(Relaxed),
            parent_hash: k.parent_hash.load(Relaxed),
            edge_tags,
            pathway_latency: pathway_latency.encode_to_vec(),
            edge_latency: edge_latency.encode_to_vec(),
            payload_size: payload_size.encode_to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm_stats::{DEFAULT_SLOT_COUNT, DEFAULT_STRING_POOL_BYTES, ShmSpanConcentrator};
    use crate::shm_table::{SLOT_EMPTY, SLOT_INIT};
    use std::ffi::CString;
    use std::sync::Arc;
    use std::sync::atomic::Ordering::SeqCst;

    fn test_path() -> CString {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        CString::new(format!(
            "/dddsm-shm-t-{}-{}",
            unsafe { libc::getpid() },
            COUNTER.fetch_add(1, SeqCst)
        ))
        .unwrap()
    }

    fn table(path: CString) -> ShmDsmCheckpointTable {
        ShmDsmCheckpointTable::create(
            path,
            DSM_BUCKET_NANOS,
            DEFAULT_DSM_CHECKPOINT_SLOT_COUNT,
            DEFAULT_DSM_CHECKPOINT_STRING_POOL_BYTES,
        )
        .unwrap()
    }

    fn mapping(t: &ShmDsmCheckpointTable) -> Arc<ShmDsmCheckpointMapping> {
        t.mem.load_full().unwrap()
    }

    fn checkpoint<'a>(hash: u64, edge_tags: &'a [&'a str]) -> DsmCheckpointInput<'a> {
        DsmCheckpointInput {
            hash,
            parent_hash: 42,
            edge_tags,
            pathway_latency_ns: 1_500_000_000,
            edge_latency_ns: 20_000_000,
            payload_size_bytes: 512,
        }
    }

    fn drain_all(t: &ShmDsmCheckpointTable) -> Vec<DsmCheckpoint> {
        t.drain_buckets(true)
            .into_iter()
            .flat_map(|b| b.checkpoints)
            .collect()
    }

    fn decode(bytes: &[u8]) -> DDSketch {
        DDSketch::from_encoded(bytes).unwrap()
    }

    /// The occupied slots of the active bucket, in slot order.
    fn occupied(mem: &ShmDsmCheckpointMapping) -> Vec<&ShmEntry> {
        mem.bucket(mem.header().active_idx.load(Relaxed))
            .entries
            .iter()
            .filter(|e| {
                let h = e.key_hash.load(Relaxed);
                h != SLOT_EMPTY && h != SLOT_INIT
            })
            .collect()
    }

    /// The bin ranges are constants because array lengths have to be; check them against the
    /// mapping they stand for.
    #[test]
    fn bin_ranges_are_the_backend_bins_they_claim() {
        let sketch = backend_sketch().unwrap();
        assert_eq!(sketch.bin_index(1e-6), Some(LATENCY_MIN_INDEX));
        assert_eq!(
            sketch.bin_index(1e5),
            Some(LATENCY_MIN_INDEX + LATENCY_BINS as i32 - 1)
        );
        assert_eq!(sketch.bin_index(1.0), Some(PAYLOAD_MIN_INDEX));
        assert_eq!(
            sketch.bin_index(1e9),
            Some(PAYLOAD_MIN_INDEX + PAYLOAD_BINS as i32 - 1)
        );
    }

    /// Writers bin values without a sketch at hand; the bin they pick has to be the sketch's.
    #[test]
    fn writers_bin_values_as_the_sketch_does() {
        let sketch = backend_sketch().unwrap();
        let mut v: f64 = 1e-7;
        while v < 1e10 {
            for x in [v, v.next_up(), v.next_down()] {
                assert_eq!(Some(backend_index(x)), sketch.bin_index(x), "value {x}");
            }
            v *= 1.0037;
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_checkpoint_round_trips_into_backend_sketches() {
        let t = table(test_path());
        let tags = ["direction:out", "topic:orders", "type:kafka"];
        assert!(t.add_checkpoint(&checkpoint(7, &tags)));
        assert!(t.add_checkpoint(&checkpoint(7, &tags)));

        let checkpoints = drain_all(&t);
        assert_eq!(
            checkpoints.len(),
            1,
            "the second checkpoint did not find the first's slot"
        );
        let g = &checkpoints[0];
        assert_eq!(g.hash, 7);
        assert_eq!(g.parent_hash, 42);
        assert_eq!(g.edge_tags, tags);

        // Bin-exact: the same sketch as adding the values straight into the backend mapping.
        for (bytes, value) in [
            (&g.pathway_latency, 1.5),
            (&g.edge_latency, 0.02),
            (&g.payload_size, 512.0),
        ] {
            let mut expected = backend_sketch().unwrap();
            expected.add_with_count(value, 2.0).unwrap();
            assert_eq!(decode(bytes).into_pb(), expected.into_pb());
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_checkpoint_keeps_its_first_inputs_key_fields() {
        let t = table(test_path());
        t.add_checkpoint(&checkpoint(7, &["topic:a"]));
        t.add_checkpoint(&DsmCheckpointInput {
            parent_hash: 2,
            ..checkpoint(7, &["topic:b", "type:kafka"])
        });
        t.add_checkpoint(&checkpoint(8, &["topic:c"]));

        let mut checkpoints = drain_all(&t);
        checkpoints.sort_by_key(|g| g.hash);
        assert_eq!(checkpoints.len(), 2);
        let first = checkpoints.iter().find(|g| g.hash == 7).unwrap();
        assert_eq!(first.edge_tags, ["topic:a"]);
        assert_eq!(first.parent_hash, 42);
        assert_eq!(decode(&first.pathway_latency).count(), 2.0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn values_outside_the_bins_land_at_their_ends() {
        let t = table(test_path());
        t.add_checkpoint(&DsmCheckpointInput {
            pathway_latency_ns: 0,
            edge_latency_ns: -5,
            payload_size_bytes: 0,
            ..checkpoint(1, &[])
        });
        t.add_checkpoint(&DsmCheckpointInput {
            pathway_latency_ns: 1,
            edge_latency_ns: i64::MAX,
            payload_size_bytes: i64::MAX,
            ..checkpoint(1, &[])
        });

        let g = &drain_all(&t)[0];
        let pathway = decode(&g.pathway_latency).into_pb();
        assert_eq!(pathway.zero_count, 1.0);
        let positive = pathway.positive_values.unwrap();
        assert_eq!(positive.contiguous_bin_index_offset, LATENCY_MIN_INDEX);
        assert_eq!(positive.contiguous_bin_counts, [1.0]);

        let edge = decode(&g.edge_latency).into_pb();
        assert_eq!(edge.zero_count, 1.0, "a negative latency counts as zero");
        assert_eq!(
            edge.positive_values.unwrap().contiguous_bin_index_offset,
            LATENCY_MIN_INDEX + LATENCY_BINS as i32 - 1
        );

        let payload = decode(&g.payload_size).into_pb();
        assert_eq!(
            payload.zero_count, 0.0,
            "an unknown size is not counted at all"
        );
        let positive = payload.positive_values.unwrap();
        assert_eq!(
            positive.contiguous_bin_index_offset,
            PAYLOAD_MIN_INDEX + PAYLOAD_BINS as i32 - 1
        );
        assert_eq!(positive.contiguous_bin_counts, [1.0]);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn edge_tags_beyond_the_maximum_are_dropped() {
        let t = table(test_path());
        let tags: Vec<String> = (0..MAX_EDGE_TAGS + 3).map(|i| format!("k{i}:v")).collect();
        let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
        t.add_checkpoint(&checkpoint(3, &tags));
        assert_eq!(drain_all(&t)[0].edge_tags, &tags[..MAX_EDGE_TAGS]);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_worker_writes_into_the_sidecars_table() {
        let path = test_path();
        let owner = table(path.clone());
        let worker = ShmDsmCheckpointTable::open(&path).unwrap();

        let owned = OwnedDsmCheckpoint {
            hash: 6,
            parent_hash: 0,
            edge_tags: vec!["direction:in".into()],
            pathway_latency_ns: 1_000,
            edge_latency_ns: 1_000,
            payload_size_bytes: 0,
        };
        let mut buf = Vec::new();
        assert!(worker.add_checkpoint(&owned.as_shm_input(&mut buf)));

        let checkpoints = drain_all(&owner);
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].hash, 6);
        assert_eq!(checkpoints[0].edge_tags, ["direction:in"]);
    }

    /// The slot size an opener validates against is its own schema's; opening another schema's
    /// segment would put every slot boundary in the wrong place.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_segment_opens_only_as_its_own_schema() {
        let dsm_path = test_path();
        let _dsm = table(dsm_path.clone());
        assert!(ShmSpanConcentrator::open(&dsm_path).is_err());
        assert!(ShmDsmCheckpointTable::open(&dsm_path).is_ok());

        let stats_path = test_path();
        let _stats = ShmSpanConcentrator::create(
            stats_path.clone(),
            DSM_BUCKET_NANOS,
            DEFAULT_SLOT_COUNT,
            DEFAULT_STRING_POOL_BYTES,
        )
        .unwrap();
        assert!(ShmDsmCheckpointTable::open(&stats_path).is_err());

        mapping(&_dsm)
            .header()
            .schema_version
            .store(DataStreamsCheckpoints::VERSION + 1, Relaxed);
        assert!(ShmDsmCheckpointTable::open(&dsm_path).is_err());
    }

    /// A forced drain dates the bucket still being written as the current interval, and the
    /// other one as the interval before it; a regular drain closes the interval just ended.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn buckets_are_dated_by_the_interval_they_close() {
        let t = table(test_path());
        let mem = mapping(&t);
        let now = 123 * DSM_BUCKET_NANOS + 4_000;

        t.add_checkpoint(&checkpoint(1, &[]));
        let buckets = mem.drain_checkpoint_buckets(true, now);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].start, 123 * DSM_BUCKET_NANOS);
        assert_eq!(buckets[0].duration, DSM_BUCKET_NANOS);

        t.add_checkpoint(&checkpoint(1, &[]));
        let buckets = mem.drain_checkpoint_buckets(false, now);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].start, 122 * DSM_BUCKET_NANOS);
        assert!(mem.drain_checkpoint_buckets(false, now).is_empty());
    }

    /// The edge-tag count is a peer's to write; a count no writer produces drops only its own
    /// group, and its histograms are still cleared rather than left for the slot's next key.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_malformed_tag_count_drops_only_its_own_checkpoint() {
        let t = table(test_path());
        for h in 1..=3 {
            t.add_checkpoint(&checkpoint(h, &["topic:x"]));
        }
        let mem = mapping(&t);
        occupied(&mem)[0]
            .key
            .edge_tag_count
            .store(MAX_EDGE_TAGS as u8 + 1, Relaxed);

        assert_eq!(drain_all(&t).len(), 2);
        assert!(drain_all(&t).is_empty());
    }

    /// Edge tags are copied out of the pool under one allowance for the whole bucket, however
    /// often a peer repeats a large reference.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn one_large_reference_repeated_across_the_tags_is_bounded() {
        const POOL: usize = 4096;
        let t = ShmDsmCheckpointTable::create(test_path(), DSM_BUCKET_NANOS, 8, POOL).unwrap();
        t.add_checkpoint(&checkpoint(1, &["topic:x"]));
        t.add_checkpoint(&checkpoint(2, &["topic:y"]));
        {
            let mem = mapping(&t);
            for entry in occupied(&mem) {
                for wire in &entry.key.edge_tags {
                    wire.offset.store(0, Relaxed);
                    wire.len.store(POOL as u32, Relaxed);
                }
                entry.key.edge_tag_count.store(MAX_EDGE_TAGS as u8, Relaxed);
            }
        }

        let exported: usize = drain_all(&t)
            .iter()
            .flat_map(|g| &g.edge_tags)
            .map(String::len)
            .sum();
        assert!(
            exported <= 3 * POOL,
            "exported {exported} bytes from a {POOL}-byte pool"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn string_references_outside_the_pool_export_nothing() {
        let t = table(test_path());
        t.add_checkpoint(&checkpoint(1, &["topic:x", "type:kafka"]));
        {
            let mem = mapping(&t);
            let entries = occupied(&mem);
            entries[0].key.edge_tags[0]
                .offset
                .store(DEFAULT_DSM_CHECKPOINT_STRING_POOL_BYTES as u32 - 2, Relaxed);
            entries[0].key.edge_tags[0].len.store(1 << 20, Relaxed);
        }
        let g = &drain_all(&t)[0];
        assert_eq!(g.edge_tags, ["", "type:kafka"]);
    }
}
