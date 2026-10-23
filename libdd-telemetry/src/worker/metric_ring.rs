// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A bounded multi-producer / single-consumer ring buffer for metric points.
//!
//! Metric points are the highest-frequency telemetry action. Routing every point through the
//! worker's tokio mpsc channel requires a heap-boxed [`TelemetryActions`](super::TelemetryActions)
//! message and a receiver wakeup per point. This dominates the per-`add_point` cost. The ring
//! buffer replaces that per-point cost with:
//!   * a lock-free reserve (a compare-and-swap on the write cursor) followed by a release-store
//!     publish,
//!   * a single active consumer (the telemetry worker) that batch-drains all pending points,
//!   * an amortized wakeup: the consumer is notified once every `NOTIFY_INTERVAL` points (and
//!     whenever a producer finds the buffer full), not once per point, and
//!   * a bounded number of buffered points without waiting for space: a producer that would lap the
//!     consumer drops its point and returns.
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

    /// A future the consumer awaits to be woken when points are available.
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
    /// space while the producer checks capacity. Safe to call from any thread.
    #[must_use = "a `false` result means that the point was dropped"]
    pub fn try_push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) -> bool {
        let seq = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        self.try_push_from(value, key, tags, seq, read)
    }

    #[inline]
    fn try_push_from(
        &self,
        value: f64,
        key: ContextKey,
        tags: Vec<Tag>,
        mut seq: u64,
        mut read: u64,
    ) -> bool {
        // Reserve a sequence only when its slot is free. The slot of `seq` holds the point of
        // `seq - RING_SIZE` until the consumer advances `read_pos` past it. A sequence that is
        // reserved must always be published, because the consumer stops at the first empty slot.
        // So the check for space comes before the reservation, and a full buffer reserves nothing.
        loop {
            if seq.wrapping_sub(read) >= RING_SIZE as u64 {
                // A stale write cursor can be behind the reader, and a stale read cursor can make
                // space look full. Recheck both before dropping the point.
                let refreshed_read = self.read_pos.fetch_add(0, Ordering::Acquire);
                let current = self.write_pos.load(Ordering::Relaxed);
                if current != seq || seq.wrapping_sub(refreshed_read) < RING_SIZE as u64 {
                    seq = current;
                    read = refreshed_read;
                    continue;
                }
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
                Err(current) => {
                    seq = current;
                    read = self.read_pos.load(Ordering::Acquire);
                }
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

    /// Drain currently published points in order, invoking `f` for each.
    ///
    /// If another drain is in progress, this call returns without consuming points.
    pub fn drain(&self, mut f: impl FnMut(f64, ContextKey, Vec<Tag>)) {
        if self
            .draining
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let _guard = DrainGuard(&self.draining);

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

    #[cfg_attr(miri, ignore)] // The deadline is not meaningful under Miri.
    #[test]
    fn full_buffer_drops_the_point_and_does_not_block() {
        let check = std::thread::spawn(check_full_buffer_drops_the_point);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !check.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "a full metric buffer blocked its producer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        check.join().unwrap();
    }

    fn check_full_buffer_drops_the_point() {
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
        ring.push(-1.0, key(0, MetricType::Count), Vec::new());

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
    fn full_buffer_wakes_the_consumer() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let ring = MetricRing::new();
        for i in 0..RING_SIZE as u32 {
            assert!(ring.try_push(1.0, key(i, MetricType::Count), Vec::new()));
        }

        // Consume the notification from filling the ring before checking the full-buffer path.
        let mut context = Context::from_waker(Waker::noop());
        let mut initial = std::pin::pin!(ring.notified());
        assert!(initial.as_mut().poll(&mut context).is_ready());
        let mut notified = std::pin::pin!(ring.notified());
        assert!(notified.as_mut().poll(&mut context).is_pending());

        assert!(!ring.try_push(1.0, key(0, MetricType::Count), Vec::new()));
        assert!(notified.as_mut().poll(&mut context).is_ready());
    }

    #[test]
    fn stale_producer_cursor_does_not_drop_a_point_when_the_ring_is_empty() {
        for start in [0, u64::MAX] {
            let ring = MetricRing::new();
            ring.write_pos.store(start, Ordering::Relaxed);
            ring.read_pos.store(start, Ordering::Relaxed);

            // A producer pauses after reading this cursor. Another producer publishes, and the
            // consumer drains that point before the first producer resumes.
            let stale_cursor = ring.write_pos.load(Ordering::Relaxed);
            assert!(ring.try_push(1.0, key(1, MetricType::Count), Vec::new()));
            let mut delivered = Vec::new();
            ring.drain(|value, _, _| delivered.push(value));
            assert_eq!(delivered, vec![1.0]);

            assert!(ring.try_push_from(
                2.0,
                key(2, MetricType::Count),
                Vec::new(),
                stale_cursor,
                ring.read_pos.load(Ordering::Acquire),
            ));
            ring.drain(|value, _, _| delivered.push(value));
            assert_eq!(delivered, vec![1.0, 2.0]);
        }
    }

    #[test]
    fn stale_consumer_cursor_does_not_drop_a_point_after_a_drain() {
        let ring = MetricRing::new();
        for i in 0..RING_SIZE as u32 {
            assert!(ring.try_push(i as f64, key(i, MetricType::Count), Vec::new()));
        }
        let stale_read = ring.read_pos.load(Ordering::Acquire);
        ring.drain(|_, _, _| {});
        let seq = ring.write_pos.load(Ordering::Relaxed);

        assert!(ring.try_push_from(7.0, key(7, MetricType::Count), Vec::new(), seq, stale_read,));
        let mut delivered = Vec::new();
        ring.drain(|value, _, _| delivered.push(value));
        assert_eq!(delivered, vec![7.0]);
    }

    #[test]
    fn dropped_point_tags_are_not_delivered() {
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

    #[test]
    fn overlapping_drains_do_not_run_concurrently() {
        let ring = Arc::new(MetricRing::new());
        assert!(ring.try_push(1.0, key(1, MetricType::Count), Vec::new()));
        assert!(ring.try_push(2.0, key(2, MetricType::Count), Vec::new()));

        let entered = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let first = {
            let ring = ring.clone();
            let entered = entered.clone();
            let resume = resume.clone();
            std::thread::spawn(move || {
                let mut delivered = Vec::new();
                ring.drain(|_, key, _| {
                    delivered.push(key.index());
                    if key.index() == 1 {
                        entered.wait();
                        resume.wait();
                    }
                });
                delivered
            })
        };

        entered.wait();
        let mut overlapping = Vec::new();
        ring.drain(|_, key, _| overlapping.push(key.index()));
        resume.wait();
        let delivered = first.join().unwrap();

        assert!(overlapping.is_empty());
        assert_eq!(delivered, vec![1, 2]);

        assert!(ring.try_push(3.0, key(3, MetricType::Count), Vec::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ring.drain(|_, _, _| panic!("consumer callback failed"));
        }));
        assert!(result.is_err());
        assert!(ring.try_push(4.0, key(4, MetricType::Count), Vec::new()));
        let mut delivered_after_panic = Vec::new();
        ring.drain(|_, key, _| delivered_after_panic.push(key.index()));
        assert_eq!(delivered_after_panic, vec![4]);
    }

    #[cfg_attr(miri, ignore)] // very slow
    #[test]
    fn multi_producer_batch_drain_delivers_every_accepted_point_once() {
        let ring = Arc::new(MetricRing::new());
        let producers = 4u32;
        let per_producer = 50_000u32;
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
                    std::hint::spin_loop();
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
