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
//!   * bounded memory without waiting for space: a producer that would lap the consumer drops its
//!     point and returns.
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
//! sequence) and read by one consumer at a time. The ready flag orders the producer's writes
//! before the consumer's reads. The read cursor orders the consumer's reads before the next
//! producer reuses the slot. A guard prevents concurrent calls to `drain` from reading the same
//! slot at once.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
    /// Ensures that at most one consumer accesses the slots.
    draining: AtomicBool,
    /// Wakes the consumer. Notified every [`NOTIFY_INTERVAL`] points and when the buffer is full.
    notify: Notify,
}

// SAFETY: see the module-level "Safety" note.
unsafe impl Send for MetricRing {}
unsafe impl Sync for MetricRing {}

struct DrainGuard<'a>(&'a AtomicBool);

impl Drop for DrainGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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

    /// Publish a metric point, or drop it when the buffer is full without waiting for space.
    /// Use [`MetricRing::try_push`] to learn whether the point was dropped. Safe to call from
    /// any thread.
    pub fn push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) {
        let _ = self.try_push(value, key, tags);
    }

    /// Publish a metric point without waiting for space. When the buffer is full, the point is
    /// dropped, the consumer is woken, and `false` is returned. Safe to call from any thread.
    #[must_use = "a `false` result means that the buffer was full and the point was dropped"]
    pub fn try_push(&self, value: f64, key: ContextKey, tags: Vec<Tag>) -> bool {
        // Reserve a sequence only when its slot is free. The slot of `seq` holds the point of
        // `seq - RING_SIZE` until the consumer advances `read_pos` past it. A sequence that is
        // reserved must always be published, because the consumer stops at the first empty slot.
        // So the check for space comes before the reservation, and a full buffer reserves nothing.
        let mut seq = self.write_pos.load(Ordering::Relaxed);
        loop {
            if seq.wrapping_sub(self.read_pos.load(Ordering::Acquire)) >= RING_SIZE as u64 {
                // The write snapshot may precede the read cursor, or the read snapshot may be
                // stale. Refresh the read cursor with an RMW, then check the write snapshot again
                // before treating the buffer as full.
                let read = self.read_pos.fetch_add(0, Ordering::Acquire);
                let current = self.write_pos.load(Ordering::Relaxed);
                if current != seq || seq.wrapping_sub(read) < RING_SIZE as u64 {
                    seq = current;
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

    /// Drain all currently-published points in order, invoking `f` for each.
    ///
    /// A concurrent or reentrant call returns without draining.
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
            // Only the consumer advances `read_pos`, so a relaxed load of our cursor is fine.
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
        // Exercise the sequence counter wrap as well as the slot index wrap.
        let start = u64::MAX - 2;
        ring.write_pos.store(start, Ordering::Relaxed);
        ring.read_pos.store(start, Ordering::Relaxed);
        let tags = vec![Tag::new("phase", "accepted").unwrap()];
        for i in 0..RING_SIZE as u32 {
            let point_tags = if i == 0 { tags.clone() } else { Vec::new() };
            assert!(ring.try_push(i as f64, key(i, MetricType::Count), point_tags));
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
        ring.drain(|v, k, point_tags| {
            assert_eq!(k.index(), next_expected);
            assert_eq!(v as u32, next_expected);
            if next_expected == 0 {
                assert_eq!(point_tags, tags);
            } else {
                assert!(point_tags.is_empty());
            }
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

        // Consume the notification from filling the ring before testing the full-buffer path.
        let mut context = Context::from_waker(Waker::noop());
        {
            let mut initial = std::pin::pin!(ring.notified());
            assert!(initial.as_mut().poll(&mut context).is_ready());
        }
        let mut notified = std::pin::pin!(ring.notified());
        assert!(notified.as_mut().poll(&mut context).is_pending());

        assert!(!ring.try_push(1.0, key(0, MetricType::Count), Vec::new()));
        assert!(notified.as_mut().poll(&mut context).is_ready());
    }

    #[test]
    fn concurrent_drains_do_not_overlap() {
        let ring = Arc::new(MetricRing::new());
        assert!(ring.try_push(1.0, key(0, MetricType::Count), Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let consumer = {
            let ring = ring.clone();
            std::thread::spawn(move || {
                let mut consumed = 0;
                ring.drain(|_, _, _| {
                    consumed += 1;
                    if consumed == 1 {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                });
                consumed
            })
        };

        entered_rx.recv().unwrap();
        assert!(ring.try_push(2.0, key(1, MetricType::Count), Vec::new()));
        let mut concurrent_consumed = 0;
        ring.drain(|_, _, _| concurrent_consumed += 1);
        release_tx.send(()).unwrap();
        let first_consumed = consumer.join().unwrap();

        assert_eq!(concurrent_consumed, 0);
        assert_eq!(first_consumed, 2);
    }

    #[test]
    fn drain_can_resume_after_callback_panics() {
        let ring = MetricRing::new();
        assert!(ring.try_push(1.0, key(0, MetricType::Count), Vec::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ring.drain(|_, _, _| panic!("callback failed"));
        }));
        assert!(result.is_err());

        assert!(ring.try_push(2.0, key(1, MetricType::Count), Vec::new()));
        let mut consumed = 0;
        ring.drain(|_, _, _| consumed += 1);
        assert_eq!(consumed, 1);
    }

    #[cfg_attr(miri, ignore)] // threaded stress test
    #[test]
    fn concurrent_producers_do_not_drop_points_when_the_ring_has_space() {
        const PRODUCERS: usize = 4;
        const ATTEMPTS: u64 = 100_000;

        let ring = Arc::new(MetricRing::new());
        let acknowledged = Arc::new(
            (0..PRODUCERS)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>(),
        );
        let done = Arc::new(AtomicBool::new(false));
        let consumer = {
            let ring = ring.clone();
            let acknowledged = acknowledged.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                while !done.load(Ordering::Acquire) {
                    ring.drain(|_, context, _| {
                        acknowledged[context.index() as usize].fetch_add(1, Ordering::Release);
                    });
                    std::hint::spin_loop();
                }
            })
        };

        let mut producers = Vec::new();
        for index in 0..PRODUCERS {
            let ring = ring.clone();
            let acknowledged = acknowledged.clone();
            producers.push(std::thread::spawn(move || {
                let mut accepted = 0;
                let mut rejected = 0;
                for _ in 0..ATTEMPTS {
                    if ring.try_push(1.0, key(index as u32, MetricType::Count), Vec::new()) {
                        accepted += 1;
                        while acknowledged[index].load(Ordering::Acquire) < accepted {
                            std::hint::spin_loop();
                        }
                    } else {
                        rejected += 1;
                    }
                }
                rejected
            }));
        }

        let rejected: u64 = producers.into_iter().map(|p| p.join().unwrap()).sum();
        done.store(true, Ordering::Release);
        consumer.join().unwrap();
        assert_eq!(rejected, 0, "the ring held at most one point per producer");
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
