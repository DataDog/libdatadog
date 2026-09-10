// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::v04::Span;
use super::TraceData;
use rand::{Rng as _, SeedableRng as _};
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thread_local::ThreadLocal;

/// When this function returns true, do not add the returned span batch to the queue.
///
/// Why are we doing this?
///
/// If we keep recylcing spans forever, two things are going to happen
/// * We will keep the **maximum** number of spans ever used by the program alive, even if memory
///   usage scales down
/// * As spans get reused and atributes data structure are pushed and popped, they will tend to grow
///   to have the maximum size of attributes
///
/// The policy operates at chunk granularity: `add_chunks` draws once per chunk and either
/// recycles the whole chunk or drops it. Dropping a fixed pct of chunks returned ensures that
/// we eventually free memory if span usage spikes, and then goes down.
fn drop_policy() -> bool {
    const PCT_OF_SPANS_RETURNED_DROPPED: f64 = 0.1;
    thread_local! {
        // https://xkcd.com/221/
        static RNG: RefCell<rand::rngs::SmallRng> = RefCell::new(rand::rngs::SmallRng::seed_from_u64(4));
    }
    RNG.with_borrow_mut(|r| r.gen_bool(PCT_OF_SPANS_RETURNED_DROPPED))
}

/// Reset fields to default, keeping collection capacity for reuse.
fn reset_span<T: TraceData>(
    Span {
        service,
        name,
        resource,
        r#type,
        trace_id,
        span_id,
        parent_id,
        start,
        duration,
        error,
        meta,
        metrics,
        meta_struct,
        span_links,
        span_events,
    }: &mut Span<T>,
) {
    *service = Default::default();
    *name = Default::default();
    *resource = Default::default();
    *r#type = Default::default();
    *trace_id = Default::default();
    *span_id = Default::default();
    *parent_id = Default::default();
    *start = Default::default();
    *duration = Default::default();
    *error = Default::default();
    meta.clear();
    metrics.clear();
    meta_struct.clear();
    span_links.clear();
    span_events.clear();
}

fn reset_chunk<T: TraceData>(chunk: &mut Vec<Span<T>>) {
    for span in chunk {
        reset_span(span);
    }
}

/// Max spans per recycled batch. Larger chunks are split so no thread hoards a big batch in its
/// local cache.
const MAX_BATCH_SIZE: usize = 20;

/// Split a chunk into pieces of at most [`MAX_BATCH_SIZE`] spans, keeping each piece's spans'
/// buffer capacity for reuse
fn split_chunk<T: TraceData>(chunk: Vec<Span<T>>) -> impl Iterator<Item = Vec<Span<T>>> {
    let pieces = (chunk.len() / 20) + 1;
    let batch_len = chunk.len() / pieces;
    let mut remaining = chunk;
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

/// Thread-safe pool of recyclable [`Span`] allocations.
///
/// Spans come back as whole chunks (`Vec<Span<T>>`) via [`SpanPool::add_chunks`] (usually by
/// dropping a [`PooledChunks`]) and are handed out by [`SpanPool::get_span`]. Reuse keeps the
/// pre-allocated `meta`/`metrics`/... buffers alive across flushes, skipping alloc churn.
///
/// Backed by an unbounded crossbeam channel of batch (one send per batch, not per span). The
/// capacity (in spans) bounds the channel only. Thread-local caches are not counted, so idle
/// threads holding cached spans don't reduce the pool's headroom.
///
/// `get_span` first hits a per-thread cache ([`ThreadLocal`]) and only when empty does it
/// dequeue a fresh span batch. This keeps the single-producer path lock-free and gives each thread
/// a local batch under contention.
#[derive(Debug, Clone)]
pub struct SpanPool<T: TraceData> {
    inner: Arc<SpanPoolInner<T>>,
}

/// A collection of recycled spans
/// Spans in this collection have been reset, while keeping
/// the capacity of linked collections
#[derive(Debug, Default)]
struct SpanBatch<T: TraceData>(Vec<Span<T>>);

impl<T: TraceData> SpanBatch<T> {
    /// Takes a list of spans, reset them while keeping the collections capacity
    fn reset(mut spans: Vec<Span<T>>) -> Self {
        reset_chunk(&mut spans);
        Self(spans)
    }
}

#[derive(Debug)]
struct SpanPoolInner<T: TraceData> {
    queue: crossbeam_channel::Sender<SpanBatch<T>>,
    receiver: crossbeam_channel::Receiver<SpanBatch<T>>,
    /// Per-thread cache: the last batch pulled from the channel plus one recycled empty `Vec`.
    thread_cache: ThreadLocal<RefCell<ThreadCache<T>>>,
    /// Total spans currently held in the global queue (channel); the capacity bound is in spans.
    len: AtomicUsize,
    /// Maximum number of recycled spans the pool will hold.
    capacity: usize,
}

#[derive(Debug, Default)]
struct ThreadCache<T: TraceData> {
    /// Last cache pulled from the channel. Spans are popped from it by `get_span`.
    last_cache: Option<SpanBatch<T>>,
    /// One recycled empty `Vec` kept for `pull_empty_chunk`. Never returned to the pool.
    empty_chunk: Option<Vec<Span<T>>>,
}

impl<T: TraceData> SpanPool<T> {
    /// New pool holding at most `capacity` recycled spans.
    pub fn with_capacity(capacity: usize) -> Self {
        let (queue, receiver) = crossbeam_channel::unbounded();
        // Capacity can never be smaller than a batch
        let capacity = capacity.max(MAX_BATCH_SIZE);
        Self {
            inner: Arc::new(SpanPoolInner {
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
    /// Spans are dropped (not pooled) when the drop policy fires or the pool is full.
    pub fn add_chunks<I: IntoIterator<Item = Vec<Span<T>>>>(&self, chunks: I) {
        for mut chunk in chunks {
            if chunk.is_empty() || drop_policy() {
                continue;
            }
            reset_chunk(&mut chunk);
            for piece in split_chunk(chunk) {
                let piece_len = piece.len();
                // Reserve span-count against the cap. Drop the piece if it won't fit.
                let current = self.inner.len.load(Ordering::Relaxed);
                if current + piece_len > self.inner.capacity {
                    return;
                }
                // The capacity bound is best-effort: we checks-and-increments `len` with
                // relaxed atomics, so concurrent producers can briefly exceed
                // `capacity` (the channel itself is unbounded).
                //
                // Since there is usually a single task returning chunks to the queue (the exporter
                // task) the bound should be exact.
                self.inner.len.fetch_add(piece_len, Ordering::Relaxed);
                if self.inner.queue.send(SpanBatch::reset(piece)).is_err() {
                    return;
                }
            }
        }
    }

    /// Get a span from the pool, or a fresh default if empty.
    /// Tries the per-thread cache first (lock-free), dequeues a new batch only when it's empty.
    pub fn get_span(&self) -> Span<T> {
        loop {
            let cell = self.inner.thread_cache.get_or_default();
            {
                let mut slot = cell.borrow_mut();
                if let Some(cache) = slot.last_cache.as_mut() {
                    if let Some(span) = cache.0.pop() {
                        if cache.0.is_empty() {
                            // Recycle the now-empty `Vec` for `pull_empty_chunk`.
                            let empty = std::mem::take(cache);
                            slot.last_cache = None;
                            if slot.empty_chunk.is_none() {
                                slot.empty_chunk = Some(empty.0);
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
    pub fn pull_empty_chunk(&self) -> Vec<Span<T>> {
        self.inner
            .thread_cache
            .get_or_default()
            .borrow_mut()
            .empty_chunk
            .take()
            .unwrap_or_default()
    }

    /// Spans currently held in the global queue (channel only, not thread-local caches).
    /// Decremented by batch when a batch is dequeued, so idle threads holding cached spans don't
    /// count against the capacity.
    pub fn len(&self) -> usize {
        self.inner.len.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Wrap chunks so they're returned to this pool on drop of the [`PooledChunks`].
    pub fn wrap_chunks(&self, chunks: Vec<Vec<Span<T>>>) -> PooledChunks<'_, T> {
        PooledChunks::new(chunks, Some(self))
    }
}

pub struct PooledChunkRefMut<'a, 'b, T: TraceData> {
    chunk: &'b mut Vec<Span<T>>,
    pool: MaybePool<'a, T>,
}

impl<'a, 'b, T: TraceData> PooledChunkRefMut<'a, 'b, T> {
    pub fn retain_mut<F: FnMut(&mut Span<T>) -> bool>(&mut self, mut f: F) {
        let dropped_spans = self.chunk.extract_if(.., |s| !f(s));
        self.pool.add_spans(dropped_spans);
    }
}

impl<'a, 'b, T: TraceData> Deref for PooledChunkRefMut<'a, 'b, T> {
    type Target = Vec<Span<T>>;

    fn deref(&self) -> &Self::Target {
        &*self.chunk
    }
}

/// A reference to a `SpanPool` that might be enabled or disabled
pub struct MaybePool<'a, T: TraceData> {
    pool: Option<&'a SpanPool<T>>,
}

impl<T: TraceData> MaybePool<'_, T> {
    pub fn add_spans<I: IntoIterator<Item = Span<T>>>(&self, spans: I) {
        let Some(pool) = self.pool else {
            // No pool: still drive the iterator so lazy `extract_if` side-effects run.
            spans.into_iter().for_each(drop);
            return;
        };
        let chunk: Vec<Span<T>> = spans.into_iter().collect();
        if !chunk.is_empty() {
            pool.add_chunks(std::iter::once(chunk));
        }
    }

    pub fn add_chunks<I: IntoIterator<Item = Vec<Span<T>>>>(&self, chunks: I) {
        let Some(pool) = self.pool else {
            // No pool: still drive the iterator so lazy `extract_if` side-effects run.
            chunks.into_iter().for_each(drop);
            return;
        };
        pool.add_chunks(chunks);
    }

    /// Get an empty chunk: recycled from the pool's thread-local cache when a pool is attached,
    /// or a fresh `Vec` otherwise.
    pub fn pull_empty_chunk(&self) -> Vec<Span<T>> {
        match self.pool {
            Some(pool) => pool.pull_empty_chunk(),
            None => Vec::new(),
        }
    }
}

/// Owned trace chunks that return to a [`SpanPool`] on drop. Deref's to the inner
/// `Vec<Vec<Span<T>>>` for in-place processing. With no pool ([`PooledChunks::unpooled`]) spans
/// just drop normally.
#[derive(Debug)]
pub struct PooledChunks<'a, T: TraceData> {
    chunks: Vec<Vec<Span<T>>>,
    pool: Option<&'a SpanPool<T>>,
}

impl<'a, T: TraceData> PooledChunks<'a, T> {
    pub fn new(chunks: Vec<Vec<Span<T>>>, pool: Option<&'a SpanPool<T>>) -> Self {
        Self { chunks, pool }
    }

    /// Wrap `chunks` with no pool, spans drop normally. For span types with no reusable
    /// allocations (e.g. borrowed slice-backed spans).
    pub fn unpooled(chunks: Vec<Vec<Span<T>>>) -> Self {
        Self::new(chunks, None)
    }

    /// Take the inner chunks, disabling pooling. For call sites that consume the chunks (e.g.
    /// formats transforming spans into another representation).
    pub fn into_chunks(mut self) -> Vec<Vec<Span<T>>> {
        std::mem::take(&mut self.chunks)
    }

    pub fn chunks_mut(&mut self) -> (MaybePool<'a, T>, &mut Vec<Vec<Span<T>>>) {
        (MaybePool { pool: self.pool }, &mut self.chunks)
    }

    pub fn retain_mut<F: for<'b> FnMut(&mut PooledChunkRefMut<'a, 'b, T>) -> bool>(
        &mut self,
        mut f: F,
    ) {
        let dropped_chunks = self.chunks.extract_if(.., |chunk| {
            !f(&mut PooledChunkRefMut {
                chunk,
                pool: MaybePool { pool: self.pool },
            })
        });
        MaybePool { pool: self.pool }.add_chunks(dropped_chunks);
    }
}

impl<T: TraceData> Deref for PooledChunks<'_, T> {
    type Target = Vec<Vec<Span<T>>>;

    fn deref(&self) -> &Self::Target {
        &self.chunks
    }
}

impl<T: TraceData> DerefMut for PooledChunks<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.chunks
    }
}

impl<T: TraceData> Drop for PooledChunks<'_, T> {
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
    use crate::span::v04::{SpanBytes, VecMap};
    use libdd_tinybytes::BytesString;

    fn span(name: &str) -> SpanBytes {
        SpanBytes {
            name: BytesString::from_slice(name.as_bytes()).unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn returned_spans_are_recycled_and_reset() {
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(100);
        {
            // No drop-policy control here, but a single span is very likely retained.
            // If we drop 10% of spans, the likelyhood all spans are dropped is 1/10**100
            // which is basically never happening if we ran this test until the heat death of
            // this universe
            let chunks = pool.wrap_chunks(vec![
                vec![SpanBytes {
                    name: "a".into(),
                    meta: VecMap::with_capacity(100),
                    ..Default::default()
                }];
                100
            ]);
            drop(chunks);
        }
        let mut retained_capacity = false;
        for _ in 0..100 {
            let s = pool.get_span();
            assert_eq!(s.name, BytesString::default());
            retained_capacity = s.meta.capacity() > 0 || retained_capacity;
        }
        assert!(
            retained_capacity,
            "no span actually retained capacity on the span meta collection"
        );
    }

    #[test]
    fn unpooled_chunks_do_not_feed_the_pool() {
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(100);
        drop(PooledChunks::unpooled(vec![vec![span("a")]]));
        assert!(pool.is_empty());
    }

    #[test]
    fn into_chunks_disables_pooling() {
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(100);
        let chunks = pool.wrap_chunks(vec![vec![span("a")]]);
        let inner = chunks.into_chunks();
        assert_eq!(inner.len(), 1);
        assert!(pool.is_empty());
    }

    #[test]
    fn pool_is_bounded() {
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(20);
        // drop_policy keeps ~90%; push far more than capacity and check the bound holds.
        for _ in 0..1000 {
            pool.add_chunks(std::iter::once(vec![span("x")]));
        }
        assert!(pool.len() <= 20);
    }

    #[test]
    fn split_chunk_empty_produces_nothing() {
        let pieces: Vec<Vec<SpanBytes>> = split_chunk(Vec::new()).collect();
        assert!(pieces.is_empty(), "an empty chunk should yield no pieces");
    }

    #[test]
    fn split_chunk_single_span_is_one_piece() {
        let chunk = vec![span("a")];
        let pieces: Vec<Vec<SpanBytes>> = split_chunk(chunk).collect();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), 1);
        assert_eq!(pieces[0][0].name.as_ref(), "a");
    }

    #[test]
    fn split_chunk_exactly_max_batch_size_is_one_piece() {
        let chunk: Vec<SpanBytes> = (0..MAX_BATCH_SIZE)
            .map(|i| span(&format!("s{i}")))
            .collect();
        let pieces: Vec<Vec<SpanBytes>> = split_chunk(chunk).collect();
        assert_eq!(
            pieces.len(),
            1,
            "a {MAX_BATCH_SIZE}-span chunk should not be split"
        );
        assert_eq!(pieces[0].len(), MAX_BATCH_SIZE);
    }

    #[test]
    fn split_chunk_just_over_max_batch_size_is_split() {
        let n = MAX_BATCH_SIZE + 1;
        let chunk: Vec<SpanBytes> = (0..n).map(|i| span(&format!("s{i}"))).collect();
        let pieces: Vec<Vec<SpanBytes>> = split_chunk(chunk).collect();
        assert!(
            pieces.len() >= 2,
            "a {}-span chunk should be split into >= 2 pieces, got {}",
            n,
            pieces.len()
        );
    }

    #[test]
    fn split_chunk_pieces_never_exceed_max_batch_size() {
        for &n in &[
            0usize,
            1,
            2,
            MAX_BATCH_SIZE - 1,
            MAX_BATCH_SIZE,
            MAX_BATCH_SIZE + 1,
            50,
            100,
            257,
        ] {
            let chunk: Vec<SpanBytes> = (0..n).map(|i| span(&format!("s{i}"))).collect();
            let pieces: Vec<Vec<SpanBytes>> = split_chunk(chunk).collect();
            for piece in &pieces {
                assert!(
                    piece.len() <= MAX_BATCH_SIZE,
                    "n={n}: piece of len {} exceeds MAX_BATCH_SIZE={MAX_BATCH_SIZE}",
                    piece.len()
                );
            }
        }
    }

    #[test]
    fn split_chunk_preserves_all_spans_exactly_once() {
        for &n in &[0usize, 1, MAX_BATCH_SIZE, MAX_BATCH_SIZE + 1, 50, 100, 257] {
            let chunk: Vec<SpanBytes> = (0..n).map(|i| span(&format!("s{i}"))).collect();
            let pieces: Vec<Vec<SpanBytes>> = split_chunk(chunk).collect();

            // No spans lost or duplicated.
            let total: usize = pieces.iter().map(Vec::len).sum();
            assert_eq!(
                total, n,
                "n={n}: total spans across pieces should equal input"
            );

            // Every original span name appears exactly once across all pieces.
            let mut names: Vec<&str> = pieces
                .iter()
                .flat_map(|piece| piece.iter().map(|s| s.name.as_ref()))
                .collect();
            names.sort_unstable();
            // Names are all distinct (s0, s1, ...), so compare sorted against generated names.
            let mut expected_names: Vec<String> = (0..n).map(|i| format!("s{i}")).collect();
            expected_names.sort_unstable();
            let expected_refs: Vec<&str> = expected_names.iter().map(String::as_str).collect();
            assert_eq!(
                names, expected_refs,
                "n={n}: spans were lost, duplicated, or reordered"
            );
        }
    }

    #[test]
    fn large_chunks_are_split_into_max_size_pieces() {
        // MAX_CHUNK_SIZE=20, so 50 spans => 20 + 20 + 10. Capacity holds all pieces; we want the
        // split, not the bound. Drop policy may drop the whole chunk, so retry until one makes it.
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(100);
        loop {
            let big_chunk: Vec<SpanBytes> = (0..50).map(|_| span("x")).collect();
            pool.add_chunks(std::iter::once(big_chunk));
            if !pool.is_empty() {
                break;
            }
        }

        let mut count = 0;
        while let Ok(cache) = pool.inner.receiver.try_recv() {
            assert!(
                cache.0.len() <= MAX_BATCH_SIZE,
                "chunk of {} spans exceeds max",
                cache.0.len()
            );
            count += 1;
        }
        assert!(
            count >= 2,
            "expected the 50-span chunk to be split into >= 2 pieces, got {count}"
        );
    }
}
