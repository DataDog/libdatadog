// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Span, TraceChunk};
use crate::span::TraceData;
use rand::{Rng as _, SeedableRng as _};
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thread_local::ThreadLocal;

/// V1 counterpart of [`crate::span::span_pool`]'s drop policy: see its docs for the rationale.
/// The policy operates at the same granularity as this pool (whole [`TraceChunk`]s).
fn drop_policy() -> bool {
    const PCT_OF_CHUNKS_RETURNED_DROPPED: f64 = 0.1;
    thread_local! {
        // https://xkcd.com/221/
        static RNG: RefCell<rand::rngs::SmallRng> = RefCell::new(rand::rngs::SmallRng::seed_from_u64(4));
    }
    RNG.with_borrow_mut(|r| r.gen_bool(PCT_OF_CHUNKS_RETURNED_DROPPED))
}

/// Reset fields to default, keeping collection capacity for reuse.
fn reset_span<T: TraceData>(span: &mut Span<T>) {
    span.service = Default::default();
    span.name = Default::default();
    span.resource = Default::default();
    span.r#type = Default::default();
    span.span_id = Default::default();
    span.parent_id = Default::default();
    span.start = Default::default();
    span.duration = Default::default();
    span.error = Default::default();
    span.span_kind = Default::default();
    span.env = Default::default();
    span.version = Default::default();
    span.component = Default::default();
    span.attributes.clear();
    span.span_links.clear();
    span.span_events.clear();
}

/// Reset a chunk's own fields, keeping collection capacity for reuse (both `attributes` and the
/// `spans` vec itself, whose elements are reset in place rather than dropped).
fn reset_chunk<T: TraceData>(chunk: &mut TraceChunk<T>) {
    chunk.trace_id = Default::default();
    chunk.priority = Default::default();
    chunk.origin = Default::default();
    chunk.sampling_mechanism = Default::default();
    chunk.dropped_trace = Default::default();
    chunk.attributes.clear();
    for span in &mut chunk.spans {
        reset_span(span);
    }
}

/// Max chunks per recycled batch. Larger batches are split so no thread hoards a big batch in its
/// local cache.
const MAX_BATCH_SIZE: usize = 20;

/// Split a batch into pieces of at most [`MAX_BATCH_SIZE`] chunks, keeping each piece's chunks'
/// buffer capacity for reuse.
fn split_batch<T: TraceData>(
    batch: Vec<TraceChunk<T>>,
) -> impl Iterator<Item = Vec<TraceChunk<T>>> {
    let pieces = (batch.len() / MAX_BATCH_SIZE) + 1;
    let batch_len = batch.len() / pieces;
    let mut remaining = batch;
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        if remaining.len() <= MAX_BATCH_SIZE {
            let mut leftover = std::mem::take(&mut remaining);
            leftover.shrink_to(MAX_BATCH_SIZE);
            return Some(leftover);
        }
        let at = remaining.len() - batch_len;
        Some(remaining.split_off(at))
    })
}

/// Thread-safe pool of recyclable [`TraceChunk`] allocations.
///
/// V1 counterpart of [`crate::span::span_pool::SpanPool`]. Unlike v0.4, where a "chunk" is a
/// plain `Vec<Span<T>>` with no chunk-level allocation of its own, a [`TraceChunk`] owns its own
/// `attributes` map in addition to its `spans`. So the recyclable unit here is the whole
/// [`TraceChunk`], not the individual [`Span`]s inside it.
///
/// See [`crate::span::span_pool::SpanPool`]'s docs for the channel/thread-local-cache/capacity
/// design this mirrors.
#[derive(Debug, Clone)]
pub struct TraceChunkPool<T: TraceData> {
    inner: Arc<TraceChunkPoolInner<T>>,
}

/// A collection of recycled chunks. Chunks in this collection have been reset, while keeping the
/// capacity of their linked collections.
#[derive(Debug, Default)]
struct TraceChunkBatch<T: TraceData>(Vec<TraceChunk<T>>);

impl<T: TraceData> TraceChunkBatch<T> {
    /// Takes a list of chunks, resets them while keeping their collections' capacity.
    fn reset(mut chunks: Vec<TraceChunk<T>>) -> Self {
        for chunk in &mut chunks {
            reset_chunk(chunk);
        }
        Self(chunks)
    }
}

#[derive(Debug)]
struct TraceChunkPoolInner<T: TraceData> {
    queue: crossbeam_channel::Sender<TraceChunkBatch<T>>,
    receiver: crossbeam_channel::Receiver<TraceChunkBatch<T>>,
    /// Per-thread cache: the last batch pulled from the channel plus one recycled empty `Vec`.
    thread_cache: ThreadLocal<RefCell<ThreadCache<T>>>,
    /// Total chunks currently held in the global queue (channel); the capacity bound is in
    /// chunks.
    len: AtomicUsize,
    /// Maximum number of recycled chunks the pool will hold.
    capacity: usize,
}

#[derive(Debug, Default)]
struct ThreadCache<T: TraceData> {
    /// Last cache pulled from the channel. Chunks are popped from it by `get_chunk`.
    last_cache: Option<TraceChunkBatch<T>>,
    /// One recycled empty `Vec` kept for `pull_empty_chunks`. Never returned to the pool.
    empty_batch: Option<Vec<TraceChunk<T>>>,
}

impl<T: TraceData> TraceChunkPool<T> {
    /// New pool holding at most `capacity` recycled chunks.
    pub fn with_capacity(capacity: usize) -> Self {
        let (queue, receiver) = crossbeam_channel::unbounded();
        // Capacity can never be smaller than a batch
        let capacity = capacity.max(MAX_BATCH_SIZE);
        Self {
            inner: Arc::new(TraceChunkPoolInner {
                queue,
                receiver,
                thread_cache: ThreadLocal::new(),
                len: AtomicUsize::new(0),
                capacity,
            }),
        }
    }

    /// Reset and return the given chunks to the pool for reuse.
    ///
    /// Chunks are dropped (not pooled) when the drop policy fires or the pool is full.
    pub fn add_chunks<I: IntoIterator<Item = TraceChunk<T>>>(&self, chunks: I) {
        let batch: Vec<TraceChunk<T>> = chunks
            .into_iter()
            .filter(|_| !drop_policy())
            .collect::<Vec<_>>();
        if batch.is_empty() {
            return;
        }
        for piece in split_batch(batch) {
            let piece_len = piece.len();
            // Reserve chunk-count against the cap. Drop the piece if it won't fit.
            let current = self.inner.len.load(Ordering::Relaxed);
            if current + piece_len > self.inner.capacity {
                return;
            }
            // The capacity bound is best-effort: we check-and-increment `len` with relaxed
            // atomics, so concurrent producers can briefly exceed `capacity` (the channel itself
            // is unbounded).
            self.inner.len.fetch_add(piece_len, Ordering::Relaxed);
            if self
                .inner
                .queue
                .send(TraceChunkBatch::reset(piece))
                .is_err()
            {
                return;
            }
        }
    }

    /// Get a chunk from the pool, or a fresh default if empty.
    /// Tries the per-thread cache first (lock-free), dequeues a new batch only when it's empty.
    pub fn get_chunk(&self) -> TraceChunk<T> {
        loop {
            let cell = self.inner.thread_cache.get_or_default();
            {
                let mut slot = cell.borrow_mut();
                if let Some(cache) = slot.last_cache.as_mut() {
                    if let Some(chunk) = cache.0.pop() {
                        if cache.0.is_empty() {
                            // Recycle the now-empty `Vec` for `pull_empty_chunks`.
                            let empty = std::mem::take(cache);
                            slot.last_cache = None;
                            if slot.empty_batch.is_none() {
                                slot.empty_batch = Some(empty.0);
                            }
                        }
                        return chunk;
                    }
                }
            }
            match self.inner.receiver.try_recv() {
                Ok(cache) => {
                    self.inner.len.fetch_sub(cache.0.len(), Ordering::Relaxed);
                    self.inner
                        .thread_cache
                        .get_or_default()
                        .borrow_mut()
                        .last_cache = Some(cache);
                }
                Err(_) => return TraceChunk::default(),
            }
        }
    }

    /// Get an empty `Vec<TraceChunk<T>>` with retained capacity for building a payload's chunk
    /// list, or a fresh one if the thread has none cached. Not counted in the pool's length;
    /// never returned to the pool.
    pub fn pull_empty_chunks(&self) -> Vec<TraceChunk<T>> {
        self.inner
            .thread_cache
            .get_or_default()
            .borrow_mut()
            .empty_batch
            .take()
            .unwrap_or_default()
    }

    /// Chunks currently held in the global queue (channel only, not thread-local caches).
    /// Decremented by batch when a batch is dequeued, so idle threads holding cached chunks don't
    /// count against the capacity.
    pub fn len(&self) -> usize {
        self.inner.len.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Wrap chunks so they're returned to this pool on drop of the [`PooledTraceChunks`].
    pub fn wrap_chunks(&self, chunks: Vec<TraceChunk<T>>) -> PooledTraceChunks<'_, T> {
        PooledTraceChunks::new(chunks, Some(self))
    }
}

/// Owned trace chunks that return to a [`TraceChunkPool`] on drop. Deref's to the inner
/// `Vec<TraceChunk<T>>` for in-place processing. With no pool ([`PooledTraceChunks::unpooled`])
/// chunks just drop normally.
#[derive(Debug)]
pub struct PooledTraceChunks<'a, T: TraceData> {
    chunks: Vec<TraceChunk<T>>,
    pool: Option<&'a TraceChunkPool<T>>,
}

impl<'a, T: TraceData> PooledTraceChunks<'a, T> {
    pub fn new(chunks: Vec<TraceChunk<T>>, pool: Option<&'a TraceChunkPool<T>>) -> Self {
        Self { chunks, pool }
    }

    /// Wrap `chunks` with no pool, chunks drop normally. For chunk types with no reusable
    /// allocations (e.g. borrowed slice-backed spans).
    pub fn unpooled(chunks: Vec<TraceChunk<T>>) -> Self {
        Self::new(chunks, None)
    }

    /// Take the inner chunks, disabling pooling. For call sites that consume the chunks (e.g.
    /// formats transforming chunks into another representation).
    pub fn into_chunks(mut self) -> Vec<TraceChunk<T>> {
        std::mem::take(&mut self.chunks)
    }

    /// Removes chunks for which `f` returns `false`, returning them to the pool (if any) instead
    /// of dropping them outright.
    pub fn retain_mut<F: FnMut(&mut TraceChunk<T>) -> bool>(&mut self, mut f: F) {
        let dropped_chunks: Vec<TraceChunk<T>> = self.chunks.extract_if(.., |c| !f(c)).collect();
        match self.pool {
            Some(pool) => pool.add_chunks(dropped_chunks),
            None => drop(dropped_chunks),
        }
    }
}

impl<T: TraceData> Deref for PooledTraceChunks<'_, T> {
    type Target = Vec<TraceChunk<T>>;

    fn deref(&self) -> &Self::Target {
        &self.chunks
    }
}

impl<T: TraceData> DerefMut for PooledTraceChunks<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.chunks
    }
}

impl<T: TraceData> Drop for PooledTraceChunks<'_, T> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool {
            let chunks = std::mem::take(&mut self.chunks);
            pool.add_chunks(chunks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v1::AttributeValue;
    use libdd_tinybytes::BytesString;

    fn chunk_with_span(name: &str) -> TraceChunk<crate::span::BytesData> {
        TraceChunk {
            spans: vec![Span {
                name: BytesString::from_slice(name.as_bytes()).unwrap(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn returned_chunks_are_recycled_and_reset() {
        let pool = TraceChunkPool::<crate::span::BytesData>::with_capacity(100);
        {
            // No drop-policy control here, but with 100 chunks it's astronomically unlikely all
            // get dropped by the 10% drop policy.
            let mut chunks = Vec::new();
            for _ in 0..100 {
                let mut chunk = chunk_with_span("a");
                chunk.attributes.insert(
                    BytesString::from_slice(b"k").unwrap(),
                    AttributeValue::Int(1),
                );
                chunks.push(chunk);
            }
            let pooled = pool.wrap_chunks(chunks);
            drop(pooled);
        }
        let mut retained_capacity = false;
        for _ in 0..100 {
            let c = pool.get_chunk();
            assert!(c.attributes.is_empty());
            retained_capacity = c.attributes.capacity() > 0 || retained_capacity;
        }
        assert!(
            retained_capacity,
            "no chunk actually retained capacity on its attributes collection"
        );
    }

    #[test]
    fn unpooled_chunks_do_not_feed_the_pool() {
        let pool = TraceChunkPool::<crate::span::BytesData>::with_capacity(100);
        drop(PooledTraceChunks::unpooled(vec![chunk_with_span("a")]));
        assert!(pool.is_empty());
    }

    #[test]
    fn into_chunks_disables_pooling() {
        let pool = TraceChunkPool::<crate::span::BytesData>::with_capacity(100);
        let chunks = pool.wrap_chunks(vec![chunk_with_span("a")]);
        let inner = chunks.into_chunks();
        assert_eq!(inner.len(), 1);
        assert!(pool.is_empty());
    }

    #[test]
    fn pool_is_bounded() {
        let pool = TraceChunkPool::<crate::span::BytesData>::with_capacity(20);
        // drop_policy keeps ~90%; push far more than capacity and check the bound holds.
        for _ in 0..1000 {
            pool.add_chunks(std::iter::once(chunk_with_span("x")));
        }
        assert!(pool.len() <= 20);
    }

    #[test]
    fn retain_mut_returns_dropped_chunks_to_the_pool() {
        let pool = TraceChunkPool::<crate::span::BytesData>::with_capacity(100);
        let mut chunks = Vec::new();
        for i in 0..50 {
            chunks.push(chunk_with_span(&format!("s{i}")));
        }
        let mut pooled = pool.wrap_chunks(chunks);
        pooled.retain_mut(|_| false);
        assert!(pooled.is_empty());
        drop(pooled);
        assert!(!pool.is_empty(), "dropped chunks should feed back the pool");
    }

    #[test]
    fn split_batch_pieces_never_exceed_max_batch_size() {
        for &n in &[
            0usize,
            1,
            MAX_BATCH_SIZE - 1,
            MAX_BATCH_SIZE,
            MAX_BATCH_SIZE + 1,
            50,
            100,
            257,
        ] {
            let batch: Vec<TraceChunk<crate::span::BytesData>> =
                (0..n).map(|i| chunk_with_span(&format!("s{i}"))).collect();
            let pieces: Vec<Vec<TraceChunk<crate::span::BytesData>>> = split_batch(batch).collect();
            for piece in &pieces {
                assert!(
                    piece.len() <= MAX_BATCH_SIZE,
                    "n={n}: piece of len {} exceeds MAX_BATCH_SIZE={MAX_BATCH_SIZE}",
                    piece.len()
                );
            }
            let total: usize = pieces.iter().map(Vec::len).sum();
            assert_eq!(total, n, "n={n}: chunks lost or duplicated across pieces");
        }
    }
}
