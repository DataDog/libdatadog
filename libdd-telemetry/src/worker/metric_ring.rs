// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A bounded multi-producer / single-consumer ring buffer for metric points.
//!
//! Metric points are the highest-frequency telemetry action. Sending each point through the
//! worker's `tokio::sync::mpsc` mailbox would heap-box a [`super::TelemetryActions`] message and
//! wake the worker per point. Even a dedicated `tokio::sync::mpsc` channel allocates linked chunks
//! dynamically and wakes the receiver on every empty-to-non-empty transition instead of batching
//! wakeups. Pre-allocating a fixed ring lets application threads publish directly with amortized
//! batch wakeups while the worker drains batches between telemetry requests.
//!
//! If the ring fills while the worker waits for HTTP, a producer wakes the worker and drops the
//! incoming point. Waiting for space here can stall application code while it holds a runtime lock
//! such as the Python GIL.
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
//! them, and points are consumed in reservation order. After reading, the consumer restores `0`.
//!
//! # Safety
//!
//! Each slot's [`UnsafeCell`]s are written by exactly one producer (the thread that reserved that
//! sequence) and read by at most one consumer. A guard prevents overlapping drain calls, and the
//! ready flag orders each producer's writes before the consumer's reads. The read cursor orders
//! the consumer's reads before a producer reuses the slot.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Notify;

use crate::Tag;
use crate::data::metrics::MetricType;
use crate::metrics::ContextKey;

/// Number of slots. Must be a power of two.
pub(super) const RING_SIZE: usize = 2048;
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

struct DrainGuard<'a>(&'a AtomicBool);

impl Drop for DrainGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
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
    /// Prevents overlapping calls to the safe `drain` method.
    draining: AtomicBool,
    /// Wakes the consumer. Notified every [`NOTIFY_INTERVAL`] points and when the buffer is full.
    notify: Notify,
}

// SAFETY: the drain guard ensures a single active reader. Cursor and ready flag synchronization
// orders each slot's producer and consumer accesses.
unsafe impl Send for MetricRing {}
unsafe impl Sync for MetricRing {}

impl MetricRing {
    pub fn new() -> Self {
        let slots = (0..RING_SIZE).map(|_| Slot::empty()).collect::<Vec<_>>();
        MetricRing {
            slots: slots.into_boxed_slice(),
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            draining: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    /// Wait for a batch notification or a producer that found the ring full. A partial batch
    /// does not necessarily wake the consumer. The telemetry worker also drains on a timer.
    pub fn notified(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.notify.notified()
    }

    /// Publish a metric point, or drop it when the buffer appears full. It does not wait for space.
    /// Use [`MetricRing::try_push`] to learn whether the point was dropped. Safe to call from any
    /// thread.
    pub fn push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) {
        let _ = self.try_push(value, key, tags);
    }

    /// Publish a metric point without waiting for space. When the buffer appears full, the point
    /// is dropped, the consumer is woken, and `false` is returned. A concurrent drain can free
    /// space while the producer checks capacity. The ring also drops further points if it
    /// exhausts its 64-bit sequence numbers. Safe to call from any thread.
    #[must_use = "a `false` result means that the point was dropped"]
    pub fn try_push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) -> bool {
        let seq = loop {
            // Pair with `drain`'s release store on `read_pos` so the consumer's slot reads,
            // `std::mem::take` on `tags`, and clearing of `ready` for every sequence before `read`
            // happen-before this producer reuses the slot.
            let read = self.read_pos.load(Ordering::Acquire);
            let seq = self.write_pos.load(Ordering::Relaxed);
            // Stop at u64::MAX so a delayed CAS can never match a reused sequence number.
            if seq >= read.saturating_add(RING_SIZE as u64) {
                // Re-read `read_pos` in case the consumer freed space while we loaded `write_pos`.
                // `Relaxed` is enough here. If we retry, the next iteration acquires `read_pos`.
                // If we drop, we do not touch any slot.
                if self.read_pos.load(Ordering::Relaxed) != read {
                    continue;
                }
                self.notify.notify_one();
                return false;
            }
            if self
                .write_pos
                .compare_exchange_weak(seq, seq + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break seq;
            }
        };

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

    /// Drain published points in order, invoking `f` for each. Points reserved after this batch
    /// starts are left for the next call, even if producers refill the slots as they are freed.
    ///
    /// If another drain is in progress, this call returns without consuming points.
    pub fn drain(&self, mut f: impl FnMut(f64, ContextKey, Vec<Tag>)) {
        if self.draining.swap(true, Ordering::Acquire) {
            return;
        }
        let _guard = DrainGuard(&self.draining);

        let mut seq = self.read_pos.load(Ordering::Relaxed);
        // `end` only caps how many slots this batch drains so continuous refills cannot starve the
        // caller. `Relaxed` is enough because `slot.ready` (not `write_pos`) orders slot payload
        // accesses, and `read_pos` prevents producers from reusing `seq`'s slot before we advance
        // it. Inspecting `slot.ready` is always race-free because it is atomic, and we stop before
        // touching `value` or `tags` if `ready` is `0`.
        let end = self.write_pos.load(Ordering::Relaxed);
        while seq < end {
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

            // Clear `ready` before advancing `read_pos`. The release store on `read_pos` pairs
            // with `try_push`'s acquire load so the consumer's slot reads, `std::mem::take` on
            // `tags`, and `ready = 0` all happen-before a producer reuses this slot. `seq < end <=
            // u64::MAX`, so `seq += 1` cannot overflow.
            slot.ready.store(0, Ordering::Relaxed);
            seq += 1;
            self.read_pos.store(seq, Ordering::Release);

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

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
    fn full_buffer_drops_without_reserving_and_wakes_the_consumer() {
        use futures::FutureExt;

        let ring = MetricRing::new();
        let context = key(0, MetricType::Gauge);
        for i in 0..RING_SIZE {
            assert!(ring.try_push(i as f64, context, Vec::new()));
        }
        // Consume the batch notification so the next wakeup must come from the dropped point.
        assert!(ring.notified().now_or_never().is_some());
        assert!(ring.notified().now_or_never().is_none());
        assert!(!ring.try_push(-1.0, context, Vec::new()));
        assert!(ring.notified().now_or_never().is_some());

        let mut drained = 0;
        ring.drain(|value, _, _| {
            assert_eq!(value, drained as f64);
            drained += 1;
        });
        assert_eq!(drained, RING_SIZE);
        assert!(ring.try_push(7.0, context, Vec::new()));
        let mut delivered = Vec::new();
        ring.drain(|value, _, _| delivered.push(value));
        assert_eq!(delivered, [7.0]);
    }

    #[test]
    fn exhausted_cursor_keeps_the_last_point_and_does_not_reuse_a_sequence() {
        let ring = MetricRing::new();
        ring.read_pos.store(u64::MAX - 1, Ordering::Relaxed);
        ring.write_pos.store(u64::MAX - 1, Ordering::Relaxed);
        let context = key(0, MetricType::Count);

        assert!(ring.try_push(1.0, context, Vec::new()));
        assert!(!ring.try_push(2.0, context, Vec::new()));
        let mut delivered = Vec::new();
        ring.drain(|value, _, _| delivered.push(value));
        assert_eq!(delivered, [1.0]);
        assert!(!ring.try_push(3.0, context, Vec::new()));
    }

    #[test]
    fn drain_returns_when_producers_refill_freed_slots() {
        let ring = MetricRing::new();
        let context = key(0, MetricType::Count);
        for _ in 0..RING_SIZE {
            assert!(ring.try_push(1.0, context, Vec::new()));
        }

        let mut drained = 0;
        ring.drain(|value, _, _| {
            assert_eq!(value, 1.0, "a refill must wait for the next drain");
            drained += 1;
            assert!(ring.try_push(2.0, context, Vec::new()));
        });
        assert_eq!(drained, RING_SIZE);
        let mut remaining = 0;
        ring.drain(|value, _, _| {
            assert_eq!(value, 2.0);
            remaining += 1;
        });
        assert_eq!(remaining, RING_SIZE);
    }

    #[test]
    fn nested_drain_leaves_the_batch_to_the_active_consumer() {
        let ring = MetricRing::new();
        let context = key(0, MetricType::Count);
        ring.push(1.0, context, Vec::new());
        ring.push(2.0, context, Vec::new());
        let mut delivered = Vec::new();
        ring.drain(|value, _, _| {
            delivered.push(value);
            ring.drain(|_, _, _| panic!("overlapping drain consumed a point"));
        });
        assert_eq!(delivered, [1.0, 2.0]);
    }

    #[test]
    fn multi_producer_batch_drain_delivers_every_accepted_point_once() {
        let ring = Arc::new(MetricRing::new());
        let producers = 4u32;
        let per_producer = if cfg!(miri) { 8 } else { 50_000u32 };
        let done = Arc::new(AtomicBool::new(false));

        // Consumer thread: drain until every accepted point is accounted for.
        let consumer = {
            let ring = ring.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                let mut delivered = Vec::new();
                loop {
                    let producers_done = done.load(Ordering::Acquire);
                    ring.drain(|value, key, _| {
                        assert_eq!(value as u32, key.index());
                        delivered.push(key.index());
                    });
                    if producers_done {
                        break;
                    }
                    std::thread::yield_now();
                }
                delivered
            })
        };

        // Each producer records what the buffer accepted. A producer that finds the buffer full
        // drops the point and continues, so the producers never wait for the consumer.
        let mut handles = Vec::new();
        for producer in 0..producers {
            let ring = ring.clone();
            handles.push(std::thread::spawn(move || {
                let mut accepted = Vec::new();
                for i in 0..per_producer {
                    let id = producer * per_producer + i;
                    if ring.try_push(id as f64, key(id, MetricType::Count), Vec::new()) {
                        accepted.push(id);
                    }
                }
                accepted
            }));
        }
        let mut accepted = Vec::new();
        for h in handles {
            accepted.extend(h.join().unwrap());
        }
        done.store(true, Ordering::Release);
        let mut delivered = consumer.join().unwrap();

        assert!(!accepted.is_empty());
        accepted.sort_unstable();
        delivered.sort_unstable();
        assert_eq!(
            delivered, accepted,
            "every accepted point is delivered one time"
        );
    }
}
