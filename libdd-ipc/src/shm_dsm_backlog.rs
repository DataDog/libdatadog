// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free shared-memory Data Streams Monitoring (DSM) backlog table.
//!
//! A backlog is the highest offset seen for a set of tags - a Kafka partition's last produced
//! offset, or a consumer group's last committed one - within a time bucket. The backend turns
//! the produce and commit offsets of a partition into consumer lag. PHP workers record offsets
//! with `ShmDsmBacklogTable::add_backlog` and cannot keep the maximum themselves, since they
//! hold no state across requests; the sidecar creates the table alongside the DSM checkpoint table
//! and drains both into the same payload.
//!
//! ## What a slot holds
//! One slot per backlog, keyed by the value of each [`BacklogTag`] it carries, holding the
//! highest offset recorded for it in the bucket. Every tag has a fixed position, which is where
//! its value is stored and where it comes in the `Tags` the payload reports; the order callers
//! give tags in plays no part.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, Ordering::*};
use strum::{EnumCount, IntoEnumIterator};
use zwohash::ZwoHasher;

use crate::shm_table::{
    self, Pool, Schema, ShmSafe, ShmTable, ShmTableMapping, WireStringRef, alloc_str, pool_eq,
    pool_string,
};

/// A tag a backlog can carry. The discriminant is the tag's position: in the slot, in the key's
/// hash, and in the `Tags` reported for the backlog.
///
/// Positions are durable. A new tag is appended at the end, after the last one; none is ever
/// reordered, removed or reused, so adding one leaves every existing backlog's `Tags` as they
/// were. (Appending one changes the slot, so it also bumps [`DataStreamsBacklogs`]'s version.)
/// The order of the first five is the Java tracer's (`DataStreamsTags`), so that a backlog is
/// reported with the same `Tags` whichever tracer sent it.
///
/// A tag's key, as it is written before the `:` in `Tags`, is its name in snake case
/// (`as_ref`, `from_str`); `iter` goes through the tags by position.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum_macros::AsRefStr,
    strum_macros::EnumCount,
    strum_macros::EnumIter,
    strum_macros::EnumString,
)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum BacklogTag {
    Topic = 0,
    Type = 1,
    KafkaClusterId = 2,
    ConsumerGroup = 3,
    Partition = 4,
}

/// Number of [`BacklogTag`]s, and of tag positions in a slot.
pub const BACKLOG_TAG_COUNT: usize = BacklogTag::COUNT;

// Which tags a key carries is a bitmask in one byte.
const _: () = assert!(BACKLOG_TAG_COUNT <= 8);

/// Default backlogs per bucket: one per partition for each topic produced to, and per consumer
/// group for each one consumed from.
pub const DEFAULT_DSM_BACKLOG_SLOT_COUNT: usize = 128;
/// Default per-bucket string pool size, for backlog tag values: 256 bytes per backlog.
pub const DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES: usize = DEFAULT_DSM_BACKLOG_SLOT_COUNT * 256;

/// Shared-memory DSM backlog table.
pub type ShmDsmBacklogTable = ShmTable<DataStreamsBacklogs>;

type ShmDsmBacklogMapping = ShmTableMapping<DataStreamsBacklogs>;
type ShmEntry = shm_table::ShmEntry<DataStreamsBacklogs>;

/// Backlog key as stored in the mapping.
#[repr(C)]
pub struct ShmBacklogKey {
    /// The value of each tag, by [`BacklogTag`] position; only the value is stored, not the
    /// `key:` before it.
    values: [WireStringRef; BACKLOG_TAG_COUNT],
    /// Bit `i` is set when the tag at position `i` is present; a present tag's value may be
    /// empty.
    present: AtomicU8,
}

// SAFETY: `#[repr(C)]` and made only of integer atomics and `ShmSafe` arrays of them.
unsafe impl ShmSafe for ShmBacklogKey {}

/// Backlog value, an integer atomic so that zeroed mmap memory is valid.
#[repr(C, align(8))]
pub struct ShmBacklogValue {
    /// Highest offset recorded; offsets are never negative, so zero is a valid start.
    offset: AtomicI64,
}

// SAFETY: `#[repr(C)]` and made only of an integer atomic.
unsafe impl ShmSafe for ShmBacklogValue {}

/// Schema of the DSM backlog table: one slot per backlog.
pub struct DataStreamsBacklogs;

impl Schema for DataStreamsBacklogs {
    const ID: u32 = 3;
    const VERSION: u32 = 1;

    type Key = ShmBacklogKey;
    type Value = ShmBacklogValue;
    type Input<'a> = DsmBacklogInput<'a>;

    fn hash(input: &DsmBacklogInput<'_>) -> u64 {
        let mut h = ZwoHasher::default();
        input.tags.hash(&mut h);
        h.finish()
    }

    fn write_key(k: &ShmBacklogKey, pool: &Pool, cursor: &AtomicU32, input: &DsmBacklogInput<'_>) {
        for (wire, value) in k.values.iter().zip(input.tags) {
            if let Some(value) = value {
                wire.store(alloc_str(pool, cursor, value));
            }
        }
        k.present.store(input.present(), Relaxed);
    }

    /// The values are compared one by one, so that a hash collision cannot merge two
    /// partitions' offsets.
    fn key_matches(k: &ShmBacklogKey, pool: &Pool, input: &DsmBacklogInput<'_>) -> bool {
        k.present.load(Relaxed) == input.present()
            && k.values
                .iter()
                .zip(input.tags)
                .all(|(wire, value)| value.is_none_or(|v| pool_eq(pool, wire.snapshot(), v)))
    }

    fn record(v: &ShmBacklogValue, input: &DsmBacklogInput<'_>) {
        v.offset.fetch_max(input.offset, Relaxed);
    }
}

/// One offset, ready to be fed into `ShmDsmBacklogTable::add_backlog`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DsmBacklogInput<'a> {
    /// The value of each tag naming the backlog, by [`BacklogTag`] position; `None` for a tag
    /// the backlog does not carry.
    pub tags: [Option<&'a str>; BACKLOG_TAG_COUNT],
    pub offset: i64,
}

impl<'a> DsmBacklogInput<'a> {
    /// An offset for a backlog without tags yet; see [`DsmBacklogInput::with`].
    pub fn new(offset: i64) -> Self {
        DsmBacklogInput {
            tags: [None; BACKLOG_TAG_COUNT],
            offset,
        }
    }

    /// Set `tag` to `value`, replacing any value it had.
    pub fn with(mut self, tag: BacklogTag, value: &'a str) -> Self {
        self.tags[tag as usize] = Some(value);
        self
    }

    /// An offset for the backlog named by `key:value` tags, given in any order.
    ///
    /// `None` when a tag is not `key:value` with a [`BacklogTag`] key: leaving it out would
    /// report its backlog under another backlog's tags. A key given twice keeps its last value.
    pub fn parse(tags: &[&'a str], offset: i64) -> Option<Self> {
        tags.iter().try_fold(Self::new(offset), |input, tag| {
            let (key, value) = tag.split_once(':')?;
            Some(input.with(key.parse().ok()?, value))
        })
    }

    fn present(&self) -> u8 {
        self.tags
            .iter()
            .enumerate()
            .filter(|(_, value)| value.is_some())
            .fold(0, |mask, (i, _)| mask | 1 << i)
    }

    /// The owned version, for sending over IPC.
    pub fn to_owned_backlog(&self) -> OwnedDsmBacklog {
        OwnedDsmBacklog {
            tags: BacklogTag::iter()
                .zip(self.tags)
                .filter_map(|(tag, value)| Some((tag, value?.to_owned())))
                .collect(),
            offset: self.offset,
        }
    }
}

/// Owned (serializable) version of [`DsmBacklogInput`].
///
/// The IPC fallback sends this to the sidecar, which creates the table if needed and adds the
/// offset.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedDsmBacklog {
    pub tags: Vec<(BacklogTag, String)>,
    pub offset: i64,
}

impl OwnedDsmBacklog {
    /// Borrow as a [`DsmBacklogInput`] for passing to `ShmDsmBacklogTable::add_backlog`.
    pub fn as_shm_input(&self) -> DsmBacklogInput<'_> {
        self.tags
            .iter()
            .fold(DsmBacklogInput::new(self.offset), |input, (tag, value)| {
                input.with(*tag, value)
            })
    }
}

/// One backlog drained out of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsmBacklog {
    /// `key:value` tags, in [`BacklogTag`] order.
    pub tags: Vec<String>,
    /// The highest offset recorded in the bucket.
    pub value: i64,
}

/// The backlogs of one time bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsmBacklogBucket {
    /// Start of the bucket, in nanoseconds since the Unix epoch, aligned to its duration.
    pub start: u64,
    pub duration: u64,
    pub backlogs: Vec<DsmBacklog>,
}

impl ShmTable<DataStreamsBacklogs> {
    /// Submit an offset to shared memory. Returns false if no current mapping is available.
    ///
    /// A negative offset - librdkafka's markers for an offset it does not know - is no offset
    /// at all and is dropped, which counts as handled.
    pub fn add_backlog(&self, input: &DsmBacklogInput<'_>) -> bool {
        input.offset < 0 || self.add(input)
    }

    /// Drain the inactive bucket, or both buckets if `force` is true, dating them as of `now`
    /// (nanoseconds since the Unix epoch).
    ///
    /// Dated as [`ShmDsmCheckpointTable::drain_buckets_at`] dates the checkpoint buckets, so that
    /// draining both tables at the same `now` gives the buckets of an interval the same start.
    ///
    /// [`ShmDsmCheckpointTable::drain_buckets_at`]:
    ///     crate::shm_dsm_checkpoint::ShmDsmCheckpointTable::drain_buckets_at
    pub fn drain_buckets_at(&self, force: bool, now: u64) -> Vec<DsmBacklogBucket> {
        self.with_current(|mem| mem.drain_backlog_buckets(force, now))
            .unwrap_or_default()
    }
}

impl ShmDsmBacklogMapping {
    fn drain_backlog_buckets(&self, force: bool, now: u64) -> Vec<DsmBacklogBucket> {
        self.drain_buckets(force, Self::drain_entry)
            .into_iter()
            .map(|b| DsmBacklogBucket {
                start: crate::shm_dsm_checkpoint::bucket_start(b.current, b.duration, now),
                duration: b.duration,
                backlogs: b.groups,
            })
            .collect()
    }

    /// Take one slot's offset and clear it, building the backlog to export.
    ///
    /// `None` when the slot's key is not one this writer could have produced; the offset is
    /// cleared either way, so that it is not left for whichever key next occupies the slot.
    fn drain_entry(entry: &ShmEntry, pool: &Pool, budget: &mut usize) -> Option<DsmBacklog> {
        let value = entry.value.offset.swap(0, Relaxed);
        let k = &entry.key;
        let present = k.present.load(Relaxed);
        if present >> BACKLOG_TAG_COUNT != 0 {
            return None;
        }
        let tags = BacklogTag::iter()
            .zip(&k.values)
            .filter(|&(tag, _)| present & 1 << tag as u8 != 0)
            .map(|(tag, wire)| {
                let value = pool_string(pool, wire.snapshot(), budget)?;
                Some(format!("{}:{value}", tag.as_ref()))
            })
            .collect::<Option<_>>()?;
        Some(DsmBacklog { tags, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm_dsm_checkpoint::{DSM_BUCKET_NANOS, ShmDsmCheckpointTable};
    use crate::shm_table::{SLOT_EMPTY, SLOT_INIT};
    use std::ffi::CString;
    use std::sync::Arc;
    use std::sync::atomic::Ordering::SeqCst;

    fn test_path() -> CString {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        CString::new(format!(
            "/dddsb-shm-t-{}-{}",
            unsafe { libc::getpid() },
            COUNTER.fetch_add(1, SeqCst)
        ))
        .unwrap()
    }

    fn table(path: CString) -> ShmDsmBacklogTable {
        ShmDsmBacklogTable::create(
            path,
            DSM_BUCKET_NANOS,
            DEFAULT_DSM_BACKLOG_SLOT_COUNT,
            DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES,
        )
        .unwrap()
    }

    fn mapping(t: &ShmDsmBacklogTable) -> Arc<ShmDsmBacklogMapping> {
        t.mem.load_full().unwrap()
    }

    fn produce(partition: &str, offset: i64) -> DsmBacklogInput<'_> {
        DsmBacklogInput::new(offset)
            .with(BacklogTag::Topic, "orders")
            .with(BacklogTag::Type, "kafka_produce")
            .with(BacklogTag::Partition, partition)
    }

    fn drain_all(t: &ShmDsmBacklogTable) -> Vec<DsmBacklog> {
        let mut backlogs: Vec<_> = t
            .drain_buckets_at(true, 0)
            .into_iter()
            .flat_map(|b| b.backlogs)
            .collect();
        backlogs.sort_by(|a, b| a.tags.cmp(&b.tags));
        backlogs
    }

    fn tags(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| (*t).to_owned()).collect()
    }

    /// The positions are part of the payload; this pins them, so that a reordering fails here.
    #[test]
    fn tag_positions_are_fixed() {
        let keys: Vec<_> = BacklogTag::iter().map(|t| t.as_ref().to_owned()).collect();
        assert_eq!(
            keys,
            [
                "topic",
                "type",
                "kafka_cluster_id",
                "consumer_group",
                "partition"
            ]
        );
        for (i, tag) in BacklogTag::iter().enumerate() {
            assert_eq!(tag as usize, i);
            assert_eq!(tag.as_ref().parse(), Ok(tag));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_backlog_keeps_the_highest_offset() {
        let t = table(test_path());
        for offset in [5, 12, 7] {
            assert!(t.add_backlog(&produce("0", offset)));
        }
        t.add_backlog(&produce("1", 3));

        assert_eq!(
            drain_all(&t),
            [
                DsmBacklog {
                    tags: tags(&["topic:orders", "type:kafka_produce", "partition:0"]),
                    value: 12,
                },
                DsmBacklog {
                    tags: tags(&["topic:orders", "type:kafka_produce", "partition:1"]),
                    value: 3,
                },
            ]
        );
        assert!(drain_all(&t).is_empty(), "a drain clears what it took");
    }

    /// However the tags are given, a backlog is one key and is reported in position order.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn tags_are_keyed_and_reported_by_position_not_given_order() {
        let t = table(test_path());
        let given = [
            [
                "partition:2",
                "consumer_group:billing",
                "type:kafka_commit",
                "topic:orders",
            ],
            [
                "topic:orders",
                "type:kafka_commit",
                "consumer_group:billing",
                "partition:2",
            ],
        ];
        for (offset, tags) in given.iter().enumerate() {
            t.add_backlog(&DsmBacklogInput::parse(tags, offset as i64 + 1).unwrap());
        }
        assert_eq!(
            drain_all(&t),
            [DsmBacklog {
                tags: tags(&[
                    "topic:orders",
                    "type:kafka_commit",
                    "consumer_group:billing",
                    "partition:2",
                ]),
                value: 2,
            }]
        );
    }

    /// A tag at a position between others takes its place there, leaving theirs in order.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_tag_comes_at_its_own_position() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 1).with(BacklogTag::KafkaClusterId, "c1"));
        assert_eq!(
            drain_all(&t)[0].tags,
            [
                "topic:orders",
                "type:kafka_produce",
                "kafka_cluster_id:c1",
                "partition:0"
            ]
        );
    }

    /// The type is part of the key: a partition's produce and commit offsets are two backlogs.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn the_type_tells_backlogs_apart() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 9));
        t.add_backlog(&produce("0", 4).with(BacklogTag::Type, "kafka_commit"));
        let values: Vec<_> = drain_all(&t).iter().map(|b| b.value).collect();
        assert_eq!(values, [4, 9]);
    }

    /// An absent tag and a present one with an empty value are different keys; the empty one is
    /// still reported, as the Java tracer reports an empty consumer group.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_empty_value_is_not_an_absent_tag() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 1));
        t.add_backlog(&produce("0", 2).with(BacklogTag::ConsumerGroup, ""));
        let backlogs = drain_all(&t);
        assert_eq!(backlogs.len(), 2);
        assert!(
            backlogs
                .iter()
                .any(|b| b.value == 2 && b.tags.contains(&"consumer_group:".to_owned()))
        );
    }

    #[test]
    fn parsing_refuses_tags_it_cannot_place() {
        assert!(DsmBacklogInput::parse(&["topic:orders", "region:eu"], 1).is_none());
        assert!(DsmBacklogInput::parse(&["topic"], 1).is_none());
        let parsed = DsmBacklogInput::parse(&["topic:a:b", "partition:1", "partition:2"], 1);
        assert_eq!(
            parsed,
            Some(
                DsmBacklogInput::new(1)
                    .with(BacklogTag::Topic, "a:b")
                    .with(BacklogTag::Partition, "2")
            )
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_offset_of_zero_is_reported() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 0));
        assert_eq!(drain_all(&t)[0].value, 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn negative_offsets_are_dropped() {
        let t = table(test_path());
        // RD_KAFKA_OFFSET_INVALID
        assert!(t.add_backlog(&produce("0", -1001)));
        assert!(drain_all(&t).is_empty());
    }

    /// A slot's value restarts from zero once drained, rather than carrying a maximum over.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn each_bucket_has_its_own_maximum() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 50));
        assert_eq!(drain_all(&t)[0].value, 50);
        t.add_backlog(&produce("0", 40));
        assert_eq!(drain_all(&t)[0].value, 40);
    }

    /// Two backlogs whose slots collide still keep their own offsets.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn colliding_backlogs_do_not_share_a_slot() {
        let t = ShmDsmBacklogTable::create(test_path(), DSM_BUCKET_NANOS, 1, 4096).unwrap();
        t.add_backlog(&produce("0", 1));
        // A one-slot table has nowhere for a second key; it must not land on the first.
        t.add_backlog(&produce("1", 9));
        let backlogs = drain_all(&t);
        assert_eq!(backlogs.len(), 1);
        assert_eq!(backlogs[0].value, 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_worker_writes_into_the_sidecars_table() {
        let path = test_path();
        let owner = table(path.clone());
        let worker = ShmDsmBacklogTable::open(&path).unwrap();

        let owned = produce("0", 77).to_owned_backlog();
        assert!(worker.add_backlog(&owned.as_shm_input()));
        assert_eq!(owned.as_shm_input(), produce("0", 77));
        assert_eq!(drain_all(&owner)[0].value, 77);
    }

    /// The backlog table and the checkpoint table are drained into one payload; an opener must
    /// never take one for the other.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_segment_opens_only_as_its_own_schema() {
        let path = test_path();
        let _backlogs = table(path.clone());
        assert!(ShmDsmCheckpointTable::open(&path).is_err());
        assert!(ShmDsmBacklogTable::open(&path).is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn buckets_are_dated_like_the_checkpoint_buckets() {
        let t = table(test_path());
        let now = 123 * DSM_BUCKET_NANOS + 4_000;

        t.add_backlog(&produce("0", 1));
        let buckets = t.drain_buckets_at(true, now);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].start, 123 * DSM_BUCKET_NANOS);

        t.add_backlog(&produce("0", 1));
        let buckets = t.drain_buckets_at(false, now);
        assert_eq!(buckets[0].start, 122 * DSM_BUCKET_NANOS);
    }

    /// The presence mask is a peer's to write; a mask naming a position no tag has drops only
    /// its own backlog.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_malformed_presence_mask_drops_only_its_own_backlog() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 1));
        t.add_backlog(&produce("1", 1));
        let mem = mapping(&t);
        let bucket = mem.bucket(mem.header().active_idx.load(Relaxed));
        let entry = bucket
            .entries
            .iter()
            .find(|e| {
                let h = e.key_hash.load(Relaxed);
                h != SLOT_EMPTY && h != SLOT_INIT
            })
            .unwrap();
        entry.key.present.store(1 << BACKLOG_TAG_COUNT, Relaxed);

        assert_eq!(drain_all(&t).len(), 1);
        assert!(drain_all(&t).is_empty());
    }

    /// A backlog with a value that does not resolve is dropped rather than exported with that
    /// value blank, which would merge it with, or pass it off as, another backlog.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn string_references_outside_the_pool_drop_their_backlog() {
        let t = table(test_path());
        t.add_backlog(&produce("0", 1));
        t.add_backlog(&produce("1", 2));
        let mem = mapping(&t);
        let bucket = mem.bucket(mem.header().active_idx.load(Relaxed));
        let entry = bucket
            .entries
            .iter()
            .find(|e| {
                let h = e.key_hash.load(Relaxed);
                h != SLOT_EMPTY && h != SLOT_INIT
            })
            .unwrap();
        let wire = &entry.key.values[BacklogTag::Partition as usize];
        wire.offset
            .store(DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES as u32 - 2, Relaxed);
        wire.len.store(1 << 20, Relaxed);

        let backlogs = drain_all(&t);
        assert_eq!(
            backlogs.len(),
            1,
            "only the untouched backlog should be exported"
        );
        assert!(!backlogs[0].tags.contains(&"partition:".to_owned()));
    }
}
