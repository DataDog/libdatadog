// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Span, TraceChunk};
use crate::span::TraceData;
use rand::{Rng as _, SeedableRng as _};
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use thread_local::ThreadLocal;

/// V1 counterpart of [`crate::span::span_pool`]'s drop policy: see its docs for the rationale.
///
/// When this function returns true, do not add the returned batch to the queue.
///
/// Why are we doing this?
///
/// If we keep recycling allocations forever, two things are going to happen:
/// * We will keep the **maximum** number of items ever used by the program alive, even if memory
///   usage scales down.
/// * As items get reused and grow and shrink, they will tend to grow to have the maximum size ever
///   seen.
///
/// Dropping a fixed pct of what's returned ensures that we eventually free memory if usage
/// spikes, and then goes down.
fn drop_policy() -> bool {
    const PCT_RETURNED_DROPPED: f64 = 0.1;
    thread_local! {
        // https://xkcd.com/221/
        static RNG: RefCell<rand::rngs::SmallRng> = RefCell::new(rand::rngs::SmallRng::seed_from_u64(4));
    }
    RNG.with_borrow_mut(|r| r.gen_bool(PCT_RETURNED_DROPPED))
}

/// Max items per recycled batch. Larger batches are split so no thread hoards a big batch in its
/// local cache.
const MAX_BATCH_SIZE: usize = 20;

/// Split a batch into pieces of at most [`MAX_BATCH_SIZE`] items, keeping each piece's capacity
/// for reuse.
fn split_batch<X>(batch: Vec<X>) -> impl Iterator<Item = Vec<X>> {
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

/// Reset a span's own fields, keeping collection capacity for reuse.
///
/// Destructures `span` so that a newly added field fails to compile here rather than being
/// silently left unreset.
fn reset_span<T: TraceData>(
    Span {
        service,
        name,
        resource,
        r#type,
        span_id,
        parent_id,
        start,
        duration,
        error,
        span_kind,
        env,
        version,
        component,
        attributes,
        span_links,
        span_events,
    }: &mut Span<T>,
) {
    *service = Default::default();
    *name = Default::default();
    *resource = Default::default();
    *r#type = Default::default();
    *span_id = Default::default();
    *parent_id = Default::default();
    *start = Default::default();
    *duration = Default::default();
    *error = Default::default();
    *span_kind = Default::default();
    *env = Default::default();
    *version = Default::default();
    *component = Default::default();
    attributes.clear();
    span_links.clear();
    span_events.clear();
}

#[derive(Debug, Default)]
struct SpanBatch<T: TraceData>(Vec<Span<T>>);

impl<T: TraceData> SpanBatch<T> {
    fn reset(mut spans: Vec<Span<T>>) -> Self {
        for span in &mut spans {
            reset_span(span);
        }
        Self(spans)
    }
}

#[derive(Debug)]
struct SpanPoolV1Inner<T: TraceData> {
    queue: crossbeam_channel::Sender<SpanBatch<T>>,
    receiver: crossbeam_channel::Receiver<SpanBatch<T>>,
    thread_cache: ThreadLocal<RefCell<SpanThreadCache<T>>>,
    /// Total spans currently held in the global queue; the capacity bound is in spans.
    len: AtomicUsize,
    capacity: usize,
}

#[derive(Debug, Default)]
struct SpanThreadCache<T: TraceData> {
    /// Last cache pulled from the channel. Spans are popped from it by `get_span`.
    last_cache: Option<SpanBatch<T>>,
    /// One recycled empty `Vec` kept for `pull_empty_spans`. Never returned to the pool.
    empty_batch: Option<Vec<Span<T>>>,
}

/// Thread-safe pool of recyclable [`Span`] allocations, handed out to build [`TraceChunk`]s.
///
/// V1 counterpart of [`crate::span::span_pool::SpanPool`], operating on v1 [`Span`]s directly
/// instead of v0.4 ones. See that module's docs for the general shape (per-thread cache,
/// batching, drop policy).
///
/// Unlike v0.4, where a "chunk" is a plain `Vec<Span<T>>` with no chunk-level allocation of its
/// own, a [`TraceChunk`] also owns an `attributes` map. That map is small and short-lived enough
/// that recycling the chunk shell itself isn't worth the bookkeeping: benchmarking showed no
/// measurable benefit over also pooling chunk shells, compared to just recycling their
/// [`Span`]s and letting the shell (and its `attributes` map) be freshly allocated each time. So
/// this only recycles [`Span`]s, which carry the expensive allocations (`attributes`,
/// `span_links`, `span_events`).
///
/// [`SpanPoolV1::add_chunks`] drains each chunk's spans into the pool; the emptied chunk shell is
/// dropped. [`SpanPoolV1::get_chunk`] returns a fresh chunk shell with a capacity-retained `spans`
/// buffer pulled from the pool. Spans are then pushed into it one at a time via
/// [`SpanPoolV1::get_span`].
#[derive(Debug, Clone)]
pub struct SpanPoolV1<T: TraceData> {
    inner: Arc<SpanPoolV1Inner<T>>,
}

impl<T: TraceData> SpanPoolV1<T> {
    /// New pool holding at most `capacity` recycled spans.
    pub fn with_capacity(capacity: usize) -> Self {
        let (queue, receiver) = crossbeam_channel::unbounded();
        let capacity = capacity.max(MAX_BATCH_SIZE);
        Self {
            inner: Arc::new(SpanPoolV1Inner {
                queue,
                receiver,
                thread_cache: ThreadLocal::new(),
                len: AtomicUsize::new(0),
                capacity,
            }),
        }
    }

    /// Drain each chunk's spans into the pool, for reuse; the emptied chunk shell is dropped.
    /// Spans are dropped (not pooled) when the drop policy fires or the pool is full.
    pub fn add_chunks<I: IntoIterator<Item = TraceChunk<T>>>(&self, chunks: I) {
        for mut chunk in chunks {
            self.add_spans(std::mem::take(&mut chunk.spans));
        }
    }

    /// Reset and return the given spans (drained from a single chunk) to the pool for reuse.
    /// The drop policy draws once for the whole batch, mirroring v0.4's per-chunk granularity.
    fn add_spans(&self, spans: Vec<Span<T>>) {
        if spans.is_empty() || drop_policy() {
            return;
        }
        for piece in split_batch(spans) {
            let piece_len = piece.len();
            let current = self.inner.len.load(Ordering::Relaxed);
            if current + piece_len > self.inner.capacity {
                return;
            }
            // Best-effort: concurrent producers can briefly exceed `capacity` (the channel
            // itself is unbounded).
            self.inner.len.fetch_add(piece_len, Ordering::Relaxed);
            if self.inner.queue.send(SpanBatch::reset(piece)).is_err() {
                return;
            }
        }
    }

    /// Get a fresh chunk shell, with its `spans` vec backed by a capacity-retained buffer pulled
    /// from the pool (or a fresh one if the pool is empty). Push spans into it via
    /// [`SpanPoolV1::get_span`] to build a new chunk.
    pub fn get_chunk(&self) -> TraceChunk<T> {
        TraceChunk {
            spans: self.pull_empty_spans(),
            ..Default::default()
        }
    }

    /// Get a reset span from the pool, or a fresh default if empty, to push into a chunk being
    /// built.
    pub fn get_span(&self) -> Span<T> {
        loop {
            let cell = self.inner.thread_cache.get_or_default();
            {
                let mut slot = cell.borrow_mut();
                if let Some(cache) = slot.last_cache.as_mut() {
                    if let Some(span) = cache.0.pop() {
                        if cache.0.is_empty() {
                            // Recycle the now-empty `Vec` for `pull_empty_spans`.
                            let empty = std::mem::take(cache);
                            slot.last_cache = None;
                            if slot.empty_batch.is_none() {
                                slot.empty_batch = Some(empty.0);
                            }
                        }
                        return span;
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
                Err(_) => return Span::default(),
            }
        }
    }

    /// Get an empty `Vec<Span<T>>` with retained capacity for building a chunk, or a fresh one if
    /// the thread has none cached. Not counted in the pool's length; never returned to the pool.
    fn pull_empty_spans(&self) -> Vec<Span<T>> {
        self.inner
            .thread_cache
            .get_or_default()
            .borrow_mut()
            .empty_batch
            .take()
            .unwrap_or_default()
    }

    /// Spans currently held in the pool's global queue (channel only, not thread-local caches).
    pub fn pooled_spans(&self) -> usize {
        self.inner.len.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.pooled_spans() == 0
    }

    /// Wrap chunks so they're returned to this pool on drop of the [`PooledTraceChunks`].
    pub fn wrap_chunks(&self, chunks: Vec<TraceChunk<T>>) -> PooledTraceChunks<'_, T> {
        PooledTraceChunks::new(chunks, Some(self))
    }
}

/// Owned trace chunks that return to a [`SpanPoolV1`] on drop. Deref's to the inner
/// `Vec<TraceChunk<T>>` for in-place processing. With no pool ([`PooledTraceChunks::unpooled`])
/// chunks just drop normally.
#[derive(Debug)]
pub struct PooledTraceChunks<'a, T: TraceData> {
    chunks: Vec<TraceChunk<T>>,
    pool: Option<&'a SpanPoolV1<T>>,
}

impl<'a, T: TraceData> PooledTraceChunks<'a, T> {
    pub fn new(chunks: Vec<TraceChunk<T>>, pool: Option<&'a SpanPoolV1<T>>) -> Self {
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
    use crate::span::v1::{AttributeValue, Span};
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
    fn returned_chunks_feed_the_span_pool() {
        let pool = SpanPoolV1::<crate::span::BytesData>::with_capacity(100);
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
                chunk.spans[0].attributes.insert(
                    BytesString::from_slice(b"k").unwrap(),
                    AttributeValue::Int(1),
                );
                chunks.push(chunk);
            }
            let pooled = pool.wrap_chunks(chunks);
            drop(pooled);
        }
        assert!(!pool.is_empty());

        let mut retained_span_capacity = false;
        for _ in 0..100 {
            let span = pool.get_span();
            assert!(span.attributes.is_empty());
            retained_span_capacity = span.attributes.capacity() > 0 || retained_span_capacity;
        }
        assert!(
            retained_span_capacity,
            "no span actually retained capacity on its attributes collection"
        );
    }

    #[test]
    fn get_chunk_recycles_a_capacity_retained_spans_vec() {
        let pool = SpanPoolV1::<crate::span::BytesData>::with_capacity(100);
        let mut chunks = Vec::new();
        for i in 0..50 {
            chunks.push(chunk_with_span(&format!("s{i}")));
        }
        drop(pool.wrap_chunks(chunks));
        assert!(!pool.is_empty());

        // Draining a whole cached span batch on this thread recycles its emptied `Vec` for
        // `pull_empty_spans`; a fresh thread-local cache has none to give yet.
        for _ in 0..pool.pooled_spans() {
            pool.get_span();
        }

        let chunk = pool.get_chunk();
        assert!(chunk.spans.is_empty());
        assert!(
            chunk.spans.capacity() > 0,
            "chunk's spans vec should have been backed by a capacity-retained buffer"
        );
    }

    #[test]
    fn unpooled_chunks_do_not_feed_the_pool() {
        let pool = SpanPoolV1::<crate::span::BytesData>::with_capacity(100);
        drop(PooledTraceChunks::unpooled(vec![chunk_with_span("a")]));
        assert!(pool.is_empty());
    }

    #[test]
    fn into_chunks_disables_pooling() {
        let pool = SpanPoolV1::<crate::span::BytesData>::with_capacity(100);
        let chunks = pool.wrap_chunks(vec![chunk_with_span("a")]);
        let inner = chunks.into_chunks();
        assert_eq!(inner.len(), 1);
        assert!(pool.is_empty());
    }

    #[test]
    fn pools_are_bounded() {
        let pool = SpanPoolV1::<crate::span::BytesData>::with_capacity(20);
        // drop_policy keeps ~90%; push far more than capacity and check the bound holds.
        for _ in 0..1000 {
            pool.add_chunks(std::iter::once(chunk_with_span("x")));
        }
        assert!(pool.pooled_spans() <= 20);
    }

    #[test]
    fn retain_mut_returns_dropped_chunks_to_the_pool() {
        let pool = SpanPoolV1::<crate::span::BytesData>::with_capacity(100);
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
            let batch: Vec<u32> = (0..n as u32).collect();
            let pieces: Vec<Vec<u32>> = split_batch(batch).collect();
            for piece in &pieces {
                assert!(
                    piece.len() <= MAX_BATCH_SIZE,
                    "n={n}: piece of len {} exceeds MAX_BATCH_SIZE={MAX_BATCH_SIZE}",
                    piece.len()
                );
            }
            let total: usize = pieces.iter().map(Vec::len).sum();
            assert_eq!(total, n, "n={n}: items lost or duplicated across pieces");
        }
    }
}
