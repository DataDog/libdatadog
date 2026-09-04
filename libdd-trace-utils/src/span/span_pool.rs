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

/// When this function returns true, do not add the returned span to the queue.
///
/// Why are we doing this?
///
/// If we keep recylcing spans forever, two things are going to happen
/// * We will keep the **maximum** number of spans ever used by the program alive, even if memory
///   usage scales down
/// * As spans get reused and atributes data structure are pushed and popped, they will tend to grow
///   to have the maximum size of attributes
///
/// Dropping a fixed pct of spans returned ensures that we eventually free memory
/// if span usage spikes, and then goes down.
fn drop_policy() -> bool {
    const PCT_OF_SPANS_RETURNED_DROPPED: f64 = 0.1;
    thread_local! {
        static RNG: RefCell<rand::rngs::SmallRng> = RefCell::new(rand::rngs::SmallRng::from_entropy());
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

/// Max spans per recycled chunk. Larger chunks are split so no thread hoards a big chunk in its
/// local cache.
const MAX_CHUNK_SIZE: usize = 20;

/// Split a chunk into pieces of at most [`MAX_CHUNK_SIZE`] spans, keeping each piece's spans'
/// buffer capacity for reuse
fn split_chunk<T: TraceData>(chunk: Vec<Span<T>>) -> impl Iterator<Item = Vec<Span<T>>> {
    let mut remaining = chunk;
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        if remaining.len() <= MAX_CHUNK_SIZE {
            return Some(std::mem::take(&mut remaining));
        }
        let at = remaining.len() - MAX_CHUNK_SIZE;
        Some(remaining.split_off(at))
    })
}

/// Thread-safe pool of recyclable [`Span`] allocations.
///
/// Spans come back as whole chunks (`Vec<Span<T>>`) via [`SpanPool::add_chunks`] (usually by
/// dropping a [`PooledChunks`]) and are handed out by [`SpanPool::get_span`]. Reuse keeps the
/// pre-allocated `meta`/`metrics`/... buffers alive across flushes, skipping alloc churn.
///
/// Backed by an unbounded crossbeam channel of chunks (one send per chunk, not per span). The
/// capacity (in spans) bounds the channel only
/// Thread-local caches are not counted, so idle threads holding cached spans don't reduce the pool's headroom.
///
/// Returning a piece reserves span-count atomically before sending, so the channel never exceeds capacity across threads.
/// `get_span` first hits a per-thread chunk cache ([`ThreadLocal`]) and only when empty does it
/// dequeue a fresh chunk. This keeps the single-producer path lock-free and gives each thread a
/// local chunk under contention.
#[derive(Debug, Clone)]
pub struct SpanPool<T: TraceData> {
    inner: Arc<SpanPoolInner<T>>,
}

#[derive(Debug)]
struct SpanPoolInner<T: TraceData> {
    queue: crossbeam_channel::Sender<Vec<Span<T>>>,
    receiver: crossbeam_channel::Receiver<Vec<Span<T>>>,
    /// Per-thread cache of the last chunk pulled from the channel.
    last_chunk: ThreadLocal<RefCell<Option<Vec<Span<T>>>>>,
    /// Total spans currently held in the global queue (channel); the capacity bound is in spans.
    len: AtomicUsize,
    /// Maximum number of recycled spans the pool will hold.
    capacity: usize,
}

impl<T: TraceData> SpanPool<T> {
    /// New pool holding at most `capacity` recycled spans.
    pub fn with_capacity(capacity: usize) -> Self {
        let (queue, receiver) = crossbeam_channel::unbounded();
        Self {
            inner: Arc::new(SpanPoolInner {
                queue,
                receiver,
                last_chunk: ThreadLocal::new(),
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
                // Reserve span-count atomically against the cap; drop the piece if it won't fit.
                let current = self.inner.len.load(Ordering::Relaxed);
                if current + piece_len > self.inner.capacity {
                    return;
                }
                self.inner.len.fetch_add(piece_len, Ordering::Relaxed);
                if let Err(_) = self.inner.queue.send(piece) {
                    return;
                }
            }
        }
    }

    /// Get a span from the pool, or a fresh default if empty.
    /// Tries the per-thread cache first (lock-free), dequeues a new chunk only when it's empty.
    pub fn get_span(&self) -> Span<T> {
        loop {
            let cell = self.inner.last_chunk.get_or_default();
            {
                let mut slot = cell.borrow_mut();
                if let Some(chunk) = slot.as_mut() {
                    if let Some(span) = chunk.pop() {
                        if chunk.is_empty() {
                            *slot = None;
                        }
                        return span;
                    }
                }
            }
            match self.inner.receiver.try_recv() {
                Ok(chunk) => {
                    // Chunk leaves the channel for the thread-local cache; len counts only the
                    // channel, so decrement by chunk here (one atomic per chunk, not per span).
                    self.inner.len.fetch_sub(chunk.len(), Ordering::Relaxed);
                    *self.inner.last_chunk.get_or_default().borrow_mut() = Some(chunk);
                }
                Err(_) => return Span::default(),
            }
        }
    }

    /// Spans currently held in the global queue (channel only, not thread-local caches).
    /// Decremented by chunk when a chunk is dequeued, so idle threads holding cached spans don't
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
    use crate::span::v04::SpanBytes;
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
            let chunks = pool.wrap_chunks(vec![vec![span("a")]]);
            drop(chunks);
        }
        let s = pool.get_span();
        assert_eq!(s.name, BytesString::default());
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
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(2);
        // drop_policy keeps ~90%; push far more than capacity and check the bound holds.
        for _ in 0..1000 {
            pool.add_chunks(std::iter::once(vec![span("x")]));
        }
        assert!(pool.len() <= 2);
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
        while let Ok(chunk) = pool.inner.receiver.try_recv() {
            assert!(
                chunk.len() <= MAX_CHUNK_SIZE,
                "chunk of {} spans exceeds max",
                chunk.len()
            );
            count += 1;
        }
        assert!(
            count >= 2,
            "expected the 50-span chunk to be split into >= 2 pieces, got {count}"
        );
    }
}
