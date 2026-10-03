// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A bounded multi-producer / single-consumer ring buffer for metric points.
//!
//! Metric points are the highest-frequency telemetry action, so routing every point through the
//! worker's tokio mpsc channel — a heap-boxed [`TelemetryActions`] message plus a receiver wakeup
//! per point — dominates the per-`add_point` cost. This ring buffer replaces that per-point cost
//! with:
//!   * a lock-free reserve (a compare-and-swap on the write cursor) followed by a release-store
//!     publish,
//!   * a single consumer (the telemetry worker) that batch-drains all pending points,
//!   * an amortized wakeup — the consumer is notified once every [`NOTIFY_INTERVAL`] points (and
//!     whenever a producer finds the buffer full), not once per point, and
//!   * bounded memory without blocking — a producer that would lap the consumer drops its point and
//!     returns. A producer never waits for the consumer.
//!
//! # Why a full buffer drops the point
//!
//! Producers run on application threads, in hot paths such as span creation. The consumer is the
//! telemetry worker, and it does not drain while it waits for the response to a telemetry request.
//! A slow or unresponsive agent can keep it there for the whole request timeout. If producers
//! waited for free space, every application thread that records a metric would stall for that
//! time. In a runtime with a global interpreter lock the caller holds that lock, so the whole
//! process would stall. Telemetry is best-effort: a dropped point is the correct trade-off.
//!
//! # Protocol (Disruptor-style, one writer per slot)
//!
//! Each slot's ready flag is an encoded `Option<ContextKey>`: `0` means "empty", any other value
//! means "a fully-written point" (the [`ContextKey`] is stashed in the high-and-low bits with a
//! sentinel bit so it is never `0`). A producer reserves a unique sequence by advancing the write
//! cursor with a compare-and-swap, and only when the slot of that sequence is free. It then writes
//! `value`/`tags` into that slot's cells and **release-stores** the encoded key. The consumer
//! reads slots in sequence order; it **acquire-loads** the flag and stops at the first `0` (a point
//! not yet published), so the producers' field writes are always visible before the consumer reads
//! them, and points are consumed in publication order. After reading, the consumer restores `0`.
//!
//! # Safety
//!
//! Each slot's [`UnsafeCell`]s are written by exactly one producer (the thread that reserved that
//! sequence) and read by exactly one consumer (the worker), and the two are ordered by the
//! release/acquire on the ready flag. No two threads ever touch the same slot's cells concurrently,
//! which is what makes the `unsafe impl Sync` sound.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Notify;

use crate::data::metrics::MetricType;
use crate::metrics::ContextKey;
use crate::Tag;

/// Number of slots. Must be a power of two.
const RING_SIZE: usize = 2048;
const RING_MASK: u64 = RING_SIZE as u64 - 1;
/// Notify the consumer once every this many published points. Power of two, `<= RING_SIZE`.
const NOTIFY_INTERVAL: u64 = 1024;
const NOTIFY_MASK: u64 = NOTIFY_INTERVAL - 1;

/// Sentinel bit that makes an encoded [`ContextKey`] non-zero (so `0` unambiguously means "empty"
/// even for context index `0`, metric type `Gauge`).
const READY_BIT: u64 = 1 << 63;

struct Slot {
    value: UnsafeCell<f64>,
    tags: UnsafeCell<Vec<Tag>>,
    /// Encoded `Option<ContextKey>`: `0` = empty, otherwise a published point (see module docs).
    ready: AtomicU64,
}

impl Slot {
    fn empty() -> Self {
        Slot {
            value: UnsafeCell::new(0.0),
            tags: UnsafeCell::new(Vec::new()),
            ready: AtomicU64::new(0),
        }
    }
}

fn metric_type_to_bits(t: MetricType) -> u64 {
    match t {
        MetricType::Gauge => 0,
        MetricType::Count => 1,
        MetricType::Distribution => 2,
        MetricType::Rate => 3,
    }
}

fn metric_type_from_bits(b: u64) -> MetricType {
    match b & 0b11 {
        0 => MetricType::Gauge,
        1 => MetricType::Count,
        2 => MetricType::Distribution,
        _ => MetricType::Rate,
    }
}

fn encode_key(key: ContextKey) -> u64 {
    READY_BIT | (metric_type_to_bits(key.metric_type()) << 32) | key.index() as u64
}

fn decode_key(v: u64) -> ContextKey {
    ContextKey::from_parts((v & 0xFFFF_FFFF) as u32, metric_type_from_bits(v >> 32))
}

pub struct MetricRing {
    slots: Box<[Slot]>,
    /// Next sequence to reserve; producers advance it with a compare-and-swap to claim a slot.
    write_pos: AtomicU64,
    /// Next sequence to consume; published by the single consumer and read by producers to detect
    /// a full buffer.
    read_pos: AtomicU64,
    /// Wakes the consumer. Notified every [`NOTIFY_INTERVAL`] points and when the buffer is full.
    notify: Notify,
}

// SAFETY: see the module-level "Safety" note — per-slot cells have a single writer and a single
// reader, ordered by the release/acquire on `ready`.
unsafe impl Send for MetricRing {}
unsafe impl Sync for MetricRing {}

impl MetricRing {
    pub fn new() -> Self {
        let slots = (0..RING_SIZE).map(|_| Slot::empty()).collect::<Vec<_>>();
        MetricRing {
            slots: slots.into_boxed_slice(),
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }

    /// A future the consumer awaits to be woken when points are available.
    pub fn notified(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.notify.notified()
    }

    /// Publish a metric point, or drop it when the buffer is full. It never waits for the consumer.
    /// Use [`MetricRing::try_push`] to learn whether the point was dropped. Safe to call from any
    /// thread.
    pub fn push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) {
        let _ = self.try_push(value, key, tags);
    }

    /// Publish a metric point. Lock-free, and it never waits for the consumer: when the buffer is
    /// full the point is dropped, the consumer is woken, and `false` is returned. Safe to call
    /// from any thread.
    #[must_use = "a `false` result means that the buffer was full and the point was dropped"]
    pub fn try_push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) -> bool {
        // Reserve a sequence only when its slot is free. The slot of `seq` holds the point of
        // `seq - RING_SIZE` until the consumer advances `read_pos` past it. A sequence that is
        // reserved must always be published, because the consumer stops at the first empty slot.
        // So the check for space comes before the reservation, and a full buffer reserves nothing.
        let mut seq = self.write_pos.load(Ordering::Relaxed);
        loop {
            if seq.wrapping_sub(self.read_pos.load(Ordering::Acquire)) >= RING_SIZE as u64 {
                self.notify.notify_one();
                return false;
            }
            match self.write_pos.compare_exchange_weak(
                seq,
                seq.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => seq = current,
            }
        }

        let slot = &self.slots[(seq & RING_MASK) as usize];
        // SAFETY: we exclusively own this slot until we publish (release-store `ready`); the
        // consumer will not touch it until then. `read_pos` only grows, and it was already past
        // `seq - RING_SIZE` when we reserved `seq`, so the previous occupant has been fully
        // consumed: the consumer clears `ready` before it advances `read_pos`.
        unsafe {
            *slot.value.get() = value;
            *slot.tags.get() = tags;
        }
        slot.ready.store(encode_key(key), Ordering::Release);

        // Amortized wakeup: nudge the consumer once per NOTIFY_INTERVAL points.
        if seq & NOTIFY_MASK == NOTIFY_MASK {
            self.notify.notify_one();
        }
        true
    }

    /// Drain all currently-published points in order, invoking `f` for each. Single-consumer only.
    pub fn drain(&self, mut f: impl FnMut(f64, ContextKey, Vec<Tag>)) {
        loop {
            // Only the consumer writes `read_pos`, so a relaxed load of our own cursor is fine.
            let seq = self.read_pos.load(Ordering::Relaxed);
            let slot = &self.slots[(seq & RING_MASK) as usize];
            let encoded = slot.ready.load(Ordering::Acquire);
            if encoded == 0 {
                // Next point not published yet: stop (points are consumed strictly in order).
                break;
            }
            // SAFETY: `ready != 0` (acquire) happens-after the producer's release-store, so its
            // field writes to this slot are visible and complete.
            let value = unsafe { *slot.value.get() };
            let tags = unsafe { std::mem::take(&mut *slot.tags.get()) };
            let key = decode_key(encoded);

            // Free the slot, then advance the cursor so producers can reserve it again. The order
            // matters: a producer may write the slot as soon as it sees the new `read_pos`.
            slot.ready.store(0, Ordering::Release);
            self.read_pos.store(seq.wrapping_add(1), Ordering::Release);

            f(value, key, tags);
        }
    }
}

impl Default for MetricRing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    fn key(index: u32, t: MetricType) -> ContextKey {
        ContextKey::from_parts(index, t)
    }

    #[test]
    fn encode_decode_roundtrip() {
        for (idx, t) in [
            (0u32, MetricType::Gauge),
            (1, MetricType::Count),
            (u32::MAX, MetricType::Distribution),
            (12345, MetricType::Rate),
        ] {
            let k = key(idx, t);
            let e = encode_key(k);
            assert_ne!(
                e, 0,
                "encoded key must never collide with the empty sentinel"
            );
            assert_eq!(decode_key(e), k);
        }
    }

    #[test]
    fn single_producer_single_consumer_preserves_all_points() {
        let ring = MetricRing::new();
        let n = RING_SIZE as u32 * 4; // force wraparound several times
        let mut consumed = 0u64;
        let mut sum = 0.0f64;
        let mut next_expected = 0u32;
        for i in 0..n {
            assert!(ring.try_push(i as f64, key(i, MetricType::Count), Vec::new()));
            // Drain after each point, so the buffer is never full.
            ring.drain(|v, k, _| {
                assert_eq!(k.index(), next_expected, "points must arrive in order");
                assert_eq!(v as u32, next_expected);
                next_expected += 1;
                consumed += 1;
                sum += v;
            });
        }
        assert_eq!(consumed, n as u64);
        assert_eq!(sum, (0..n).map(|i| i as f64).sum::<f64>());
    }

    #[test]
    fn full_buffer_drops_the_point_and_does_not_block() {
        // No consumer runs here. This is the state of the worker while it waits for the response
        // to a telemetry request. A push that waited for free space would never return.
        let ring = MetricRing::new();
        for i in 0..RING_SIZE as u32 {
            assert!(ring.try_push(i as f64, key(i, MetricType::Count), Vec::new()));
        }
        for i in 0..100u32 {
            assert!(
                !ring.try_push(-1.0, key(i, MetricType::Count), Vec::new()),
                "a full buffer must drop the point"
            );
        }

        // A dropped point reserves no sequence, so the consumer sees no gap: it reads every
        // accepted point, in order.
        let mut next_expected = 0u32;
        ring.drain(|v, k, _| {
            assert_eq!(k.index(), next_expected);
            assert_eq!(v as u32, next_expected);
            next_expected += 1;
        });
        assert_eq!(next_expected, RING_SIZE as u32);

        // The buffer accepts points again after the drain, across the wraparound.
        for i in 0..RING_SIZE as u32 {
            assert!(ring.try_push(i as f64, key(i, MetricType::Gauge), Vec::new()));
        }
        assert!(!ring.try_push(-1.0, key(0, MetricType::Gauge), Vec::new()));
        let mut drained = 0u32;
        ring.drain(|v, k, _| {
            assert_eq!(k.index(), drained);
            assert_eq!(v as u32, drained);
            drained += 1;
        });
        assert_eq!(drained, RING_SIZE as u32);
    }

    #[test]
    fn dropped_point_releases_its_tags() {
        let ring = MetricRing::new();
        for i in 0..RING_SIZE as u32 {
            assert!(ring.try_push(0.0, key(i, MetricType::Count), Vec::new()));
        }
        let tags = vec![Tag::new("a", "b").unwrap()];
        assert!(!ring.try_push(1.0, key(0, MetricType::Count), tags));
        let mut tagged = 0;
        ring.drain(|_, _, t| tagged += t.len());
        assert_eq!(
            tagged, 0,
            "the tags of a dropped point must not reach the consumer"
        );
    }

    #[cfg_attr(miri, ignore)] // very slow
    #[test]
    fn multi_producer_batch_drain_delivers_every_accepted_point_once() {
        let ring = Arc::new(MetricRing::new());
        let producers = 4u32;
        let per_producer = 50_000u32;
        let done = Arc::new(AtomicBool::new(false));
        let consumed = Arc::new(AtomicU64::new(0));
        let value_sum = Arc::new(AtomicU64::new(0)); // sum of integer values, as bits

        // Consumer thread: drain until every accepted point is accounted for.
        let consumer = {
            let ring = ring.clone();
            let done = done.clone();
            let consumed = consumed.clone();
            let value_sum = value_sum.clone();
            std::thread::spawn(move || loop {
                ring.drain(|v, _k, _t| {
                    consumed.fetch_add(1, Ordering::Relaxed);
                    value_sum.fetch_add(v as u64, Ordering::Relaxed);
                });
                if done.load(Ordering::Acquire)
                    && ring.read_pos.load(Ordering::Acquire)
                        == ring.write_pos.load(Ordering::Acquire)
                {
                    break;
                }
                std::hint::spin_loop();
            })
        };

        // Each producer counts what the buffer accepted. A producer that finds the buffer full
        // drops the point and continues, so the producers never wait for the consumer.
        let mut handles = Vec::new();
        for _ in 0..producers {
            let ring = ring.clone();
            handles.push(std::thread::spawn(move || {
                let mut accepted = 0u64;
                let mut accepted_sum = 0u64;
                for i in 0..per_producer {
                    if ring.try_push(i as f64, key(i, MetricType::Count), Vec::new()) {
                        accepted += 1;
                        accepted_sum += i as u64;
                    }
                }
                (accepted, accepted_sum)
            }));
        }
        let mut accepted = 0u64;
        let mut accepted_sum = 0u64;
        for h in handles {
            let (count, sum) = h.join().unwrap();
            accepted += count;
            accepted_sum += sum;
        }
        done.store(true, Ordering::Release);
        consumer.join().unwrap();

        assert!(accepted > 0);
        assert!(accepted <= (producers * per_producer) as u64);
        assert_eq!(
            consumed.load(Ordering::Relaxed),
            accepted,
            "every accepted point is delivered one time"
        );
        assert_eq!(value_sum.load(Ordering::Relaxed), accepted_sum);
    }
}
