// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Encode first, then bisect oversized payloads between SDK-supplied chunks.
//!
//! This is an alternative to bounded encoding: the first attempt allocates the whole encoded
//! payload, so `limit` bounds returned payloads, not temporary memory. Each returned byte vector
//! is a complete payload, with its own string table for V1. Encoding to v0.4 retains the
//! existing downgrade semantics.
//!
//! Partial flushing belongs to the SDK: chunk metadata can live on the root span. Keeping the
//! original chunks intact preserves that association and the order of their spans.

use crate::span::{TraceData, v1::TracerPayload};
use libdd_common::ResultInfallibleExt;
use rmp::encode::ByteBuf;
use std::ops::Range;

/// Existing wire formats supported by the V1 input model.
#[derive(Clone, Copy, Debug)]
pub enum Format {
    /// v0.4 output with the default [`super::v04::V1DowngradeOptions`].
    V04,
    V1,
}

impl Format {
    fn encode<T: TraceData>(self, source: &TracerPayload<T>, chunks: Range<usize>) -> Vec<u8> {
        let chunks = &source.chunks[chunks];
        let mut buf = ByteBuf::with_capacity(0);
        match self {
            Self::V04 => {
                super::v04::encode_payload_from_v1(&mut buf, source, chunks, Default::default())
            }
            Self::V1 => super::v1::encode_payload_from_v1(&mut buf, source, chunks),
        }
        .map_err(super::flatten_value_write_infallible)
        .unwrap_infallible();
        buf.into_vec()
    }
}

/// The result for a range of chunk indices in the original payload.
#[derive(Debug)]
pub enum Outcome {
    /// A complete payload whose encoded bytes fit the limit.
    Encoded {
        chunks: Range<usize>,
        bytes: Vec<u8>,
    },
    /// A single chunk (or the empty payload, `0..0`) whose complete encoding exceeds the limit.
    Oversized {
        chunks: Range<usize>,
        encoded_size: usize,
    },
}

/// Lazily encodes candidates. Stop iterating to cancel before the next outcome.
/// Each `next()` may encode several candidates synchronously before returning an outcome.
#[must_use = "payloads are only encoded when the iterator is consumed"]
pub struct Payloads<'a, T: TraceData> {
    source: &'a TracerPayload<T>,
    format: Format,
    limit: usize,
    ranges: ChunkRanges,
}

/// Whole-chunk bisection shared by the native and cross-format encoders.
pub struct ChunkRanges {
    // The fitting fast path needn't allocate a stack of pending ranges.
    first: Option<Range<usize>>,
    pending: Vec<Range<usize>>,
}

impl ChunkRanges {
    /// Start at the supplied range (which can be an empty control envelope).
    pub fn new(chunks: Range<usize>) -> Self {
        Self {
            first: Some(chunks),
            pending: Vec::new(),
        }
    }

    /// Visit candidates in input order. Bisect a rejected candidate only if it
    /// contains multiple chunks. No previously returned range is revisited.
    pub fn next_with<T>(
        &mut self,
        mut encode: impl FnMut(Range<usize>) -> T,
        split: impl Fn(&T) -> bool,
    ) -> Option<(Range<usize>, T)> {
        let mut chunks = self.first.take().or_else(|| self.pending.pop())?;
        loop {
            let result = encode(chunks.clone());
            if chunks.len() <= 1 || !split(&result) {
                return Some((chunks, result));
            }
            let middle = chunks.start + chunks.len() / 2;
            self.pending.push(middle..chunks.end);
            chunks.end = middle;
        }
    }
}

/// Encode once when the payload fits. On overflow, bisect the chunk range until each candidate
/// fits or cannot be split further. The limit includes the complete encoded envelope.
///
/// Each input chunk appears in exactly one outcome, in input order. A chunk that cannot fit on
/// its own is reported as [`Outcome::Oversized`], regardless of its span count. Iteration can
/// continue with the remaining chunks.
///
/// ```
/// use libdd_trace_utils::{
///     msgpack_encoder::split::{self, Format, Outcome},
///     span::v1::TracerPayloadBytes,
/// };
/// let payload = TracerPayloadBytes::default();
/// for result in split::encode(&payload, Format::V1, 1024) {
///     match result {
///         Outcome::Encoded { bytes, .. } => assert!(bytes.len() <= 1024),
///         Outcome::Oversized {
///             chunks,
///             encoded_size,
///         } => {
///             eprintln!("chunks {chunks:?}: {encoded_size} bytes")
///         }
///     }
/// }
/// ```
pub fn encode<T: TraceData>(
    source: &TracerPayload<T>,
    format: Format,
    limit: usize,
) -> Payloads<'_, T> {
    Payloads {
        source,
        format,
        limit,
        ranges: ChunkRanges::new(0..source.chunks.len()),
    }
}

impl<T: TraceData> Iterator for Payloads<'_, T> {
    type Item = Outcome;

    fn next(&mut self) -> Option<Self::Item> {
        let (chunks, bytes) = self.ranges.next_with(
            |r| self.format.encode(self.source, r),
            |b| b.len() > self.limit,
        )?;
        Some(if bytes.len() <= self.limit {
            Outcome::Encoded { chunks, bytes }
        } else {
            Outcome::Oversized {
                chunks,
                encoded_size: bytes.len(),
            }
        })
    }
}

impl<T: TraceData> std::iter::FusedIterator for Payloads<'_, T> {}
