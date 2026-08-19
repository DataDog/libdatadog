// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::v04::Span;
use super::TraceData;
use rand::Rng;
use std::ops::{Deref, DerefMut};

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
    rand::thread_rng().gen_bool(PCT_OF_SPANS_RETURNED_DROPPED)
}

/// Reset the spans fields to their default value without
/// freeing the the capacity of the collections
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

/// A thread-safe pool of recyclable [`Span`] allocations.
///
/// Spans are returned to the pool via [`SpanPool::add_spans`] (typically indirectly, by
/// dropping a [`PooledChunks`]) and can be re-used by a producer via [`SpanPool::get_span`].
/// Reusing spans keeps their pre-allocated `meta`/`metrics`/... buffers alive across trace
/// flushes, avoiding the alloc/free churn of building fresh spans every time.
///
/// The pool is backed by a bounded channel so it never grows without limit: once it is full,
/// returned spans are simply dropped.
#[derive(Debug, Clone)]
pub struct SpanPool<T: TraceData> {
    queue: crossbeam_channel::Sender<Span<T>>,
    receiver: crossbeam_channel::Receiver<Span<T>>,
}

impl<T: TraceData> SpanPool<T> {
    /// Create a new pool with the default capacity.
    pub fn new(pool_capacity: usize) -> Self {
        Self::with_capacity(pool_capacity)
    }

    /// Create a new pool holding at most `capacity` recycled spans.
    pub fn with_capacity(capacity: usize) -> Self {
        let (queue, receiver) = crossbeam_channel::bounded(capacity);
        Self { queue, receiver }
    }

    /// Reset and return the given spans to the pool so they can be reused later.
    ///
    /// Spans are dropped instead of pooled when the drop policy fires (to let memory scale
    /// back down over time) or when the pool is already full.
    pub fn add_spans<I: IntoIterator<Item = Span<T>>>(&self, spans: I) {
        for mut span in spans {
            if !drop_policy() {
                reset_span(&mut span);
                // Bounded channel: drop the span instead of blocking when the pool is full.
                let _ = self.queue.try_send(span);
            }
        }
    }

    /// Get a span from the pool, or a freshly-allocated default one if the pool is empty.
    pub fn get_span(&self) -> Span<T> {
        self.receiver.try_recv().unwrap_or_default()
    }

    /// Number of spans currently held in the pool.
    pub fn len(&self) -> usize {
        self.receiver.len()
    }

    /// Whether the pool currently holds no spans.
    pub fn is_empty(&self) -> bool {
        self.receiver.is_empty()
    }

    /// Wrap owned trace chunks so that, when the returned [`PooledChunks`] is dropped, its
    /// spans are returned to this pool.
    pub fn wrap_chunks(&self, chunks: Vec<Vec<Span<T>>>) -> PooledChunks<'_, T> {
        PooledChunks::new(chunks, Some(self))
    }
}

/// Owned trace chunks (`Vec<Vec<Span<T>>>`) that return their spans to a [`SpanPool`] on drop.
///
/// Dereferences to the underlying `Vec<Vec<Span<T>>>`, so it can be processed in place exactly
/// like a plain vector of chunks. When no pool is attached (see [`PooledChunks::unpooled`]) it
/// behaves like a plain vector whose spans are simply dropped.
#[derive(Debug)]
pub struct PooledChunks<'a, T: TraceData> {
    chunks: Vec<Vec<Span<T>>>,
    pool: Option<&'a SpanPool<T>>,
}

impl<'a, T: TraceData> PooledChunks<'a, T> {
    /// Wrap `chunks`, returning their spans to `pool` on drop when one is provided.
    pub fn new(chunks: Vec<Vec<Span<T>>>, pool: Option<&'a SpanPool<T>>) -> Self {
        Self { chunks, pool }
    }

    /// Wrap `chunks` without any pool: the spans are dropped normally on drop.
    ///
    /// Useful for span types that own no reusable allocations (e.g. borrowed slice-backed
    /// spans), for which pooling brings no benefit.
    pub fn unpooled(chunks: Vec<Vec<Span<T>>>) -> Self {
        Self::new(chunks, None)
    }

    /// Take ownership of the inner chunks, disabling pooling for these spans.
    ///
    /// Used by call sites that must consume the chunks (e.g. formats that transform spans into
    /// a different representation), where the original spans cannot be recycled.
    pub fn into_chunks(mut self) -> Vec<Vec<Span<T>>> {
        std::mem::take(&mut self.chunks)
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
            pool.add_spans(chunks.into_iter().flatten());
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
        let pool = SpanPool::<crate::span::BytesData>::new(100);
        {
            // No drop policy control here, but with a single span it is very likely retained.
            let chunks = pool.wrap_chunks(vec![vec![span("a")]]);
            drop(chunks);
        }
        // A span pulled from the pool is always in its reset (default) state.
        let s = pool.get_span();
        assert_eq!(s.name, BytesString::default());
    }

    #[test]
    fn unpooled_chunks_do_not_feed_the_pool() {
        let pool = SpanPool::<crate::span::BytesData>::new(100);
        drop(PooledChunks::unpooled(vec![vec![span("a")]]));
        assert!(pool.is_empty());
    }

    #[test]
    fn into_chunks_disables_pooling() {
        let pool = SpanPool::<crate::span::BytesData>::new(100);
        let chunks = pool.wrap_chunks(vec![vec![span("a")]]);
        let inner = chunks.into_chunks();
        assert_eq!(inner.len(), 1);
        assert!(pool.is_empty());
    }

    #[test]
    fn pool_is_bounded() {
        let pool = SpanPool::<crate::span::BytesData>::with_capacity(2);
        // drop_policy retains ~90% of spans; push far more than capacity and ensure the pool
        // never exceeds its bound.
        for _ in 0..1000 {
            pool.add_spans([span("x")]);
        }
        assert!(pool.len() <= 2);
    }
}
