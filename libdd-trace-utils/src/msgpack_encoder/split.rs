// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Encode first, then split oversized payloads between chunks, and finally between spans.
//!
//! This is an alternative to bounded encoding: the first attempt allocates the whole encoded
//! payload, so `limit` bounds returned payloads, not temporary memory. Each result is a complete
//! encoding (including its own dictionary for V1). Delivery of one result does not depend on
//! the others. Encoding to v0.4 retains the existing downgrade semantics.
//!
//! Input uses the V1 model so trace identity and sampling metadata remain available even when
//! the root span is in another part. Perform topology-dependent processing before splitting;
//! this API preserves existing span attributes rather than inventing new roots or top-level flags.

use crate::span::{
    TraceData,
    v1::{Span, TraceChunk, TracerPayload},
};
use std::ops::Range;

/// Existing wire formats supported by the V1 input model.
#[derive(Clone, Copy, Debug)]
pub enum Format {
    V04,
    V1,
}

/// Original input covered by an outcome. Span indices are relative to the original chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    chunks: Range<usize>,
    spans: Option<Range<usize>>,
}

impl Part {
    /// Chunk indices in the original payload.
    pub fn chunks(&self) -> Range<usize> {
        self.chunks.clone()
    }

    /// Span indices within a singleton chunk, or `None` for all spans in the selected chunks.
    pub fn spans(&self) -> Option<Range<usize>> {
        self.spans.clone()
    }

    fn view<'a, T: TraceData>(
        &self,
        source: &'a TracerPayload<T>,
    ) -> impl ExactSizeIterator<Item = ChunkView<'a, T>> {
        let spans = self.spans.clone();
        source.chunks[self.chunks.clone()]
            .iter()
            .map(move |chunk| ChunkView {
                chunk,
                spans: spans
                    .as_ref()
                    .map_or(chunk.spans.as_slice(), |range| &chunk.spans[range.clone()]),
            })
    }

    fn bisect<T: TraceData>(&self, source: &TracerPayload<T>) -> Option<(Self, Self)> {
        if self.chunks.len() > 1 {
            let middle = self.chunks.start + self.chunks.len() / 2;
            return Some((
                Self {
                    chunks: self.chunks.start..middle,
                    spans: None,
                },
                Self {
                    chunks: middle..self.chunks.end,
                    spans: None,
                },
            ));
        }
        let chunk = source.chunks.get(self.chunks.start)?;
        let spans = self.spans.clone().unwrap_or(0..chunk.spans.len());
        if spans.len() <= 1 {
            return None;
        }
        let middle = spans.start + spans.len() / 2;
        Some((
            Self {
                chunks: self.chunks.clone(),
                spans: Some(spans.start..middle),
            },
            Self {
                chunks: self.chunks.clone(),
                spans: Some(middle..spans.end),
            },
        ))
    }
}

pub(super) struct ChunkView<'a, T: TraceData> {
    pub chunk: &'a TraceChunk<T>,
    pub spans: &'a [Span<T>],
}

/// A fitting payload or an indivisible oversized part. Oversized parts contain one span,
/// an empty chunk, or the empty payload envelope; the caller decides how to report/drop them.
#[derive(Debug)]
pub enum Outcome {
    Encoded { part: Part, bytes: Vec<u8> },
    Oversized { part: Part, encoded_size: usize },
}

/// Lazily encodes candidates. Stop iterating to cancel before the next outcome.
/// Each `next()` may encode several candidates synchronously before returning an outcome.
pub struct Payloads<'a, T: TraceData> {
    source: &'a TracerPayload<T>,
    format: Format,
    limit: usize,
    next: Option<Part>,
    pending: Vec<Part>,
}

/// Encode once when the payload fits. On overflow, split whole chunks first; split spans only
/// when an individual chunk is too large. The limit includes the complete encoded envelope.
///
/// The original payload and chunk metadata are reused for every part. Spans are neither cloned
/// nor modified, and each appears in exactly one terminal outcome, in input order. There is no
/// span truncation: a single span that cannot fit is returned as [`Outcome::Oversized`].
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
///         Outcome::Oversized { part, encoded_size } => {
///             eprintln!("{part:?}: {encoded_size} bytes")
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
        next: Some(Part {
            chunks: 0..source.chunks.len(),
            spans: None,
        }),
        pending: Vec::new(),
    }
}

impl<T: TraceData> Iterator for Payloads<'_, T> {
    type Item = Outcome;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(part) = self.next.take().or_else(|| self.pending.pop()) {
            let bytes = match self.format {
                Format::V04 => super::v04::encode_selection(self.source, part.view(self.source)),
                Format::V1 => super::v1::encode_selection(self.source, part.view(self.source)),
            };
            let encoded_size = bytes.len();
            if encoded_size <= self.limit {
                return Some(Outcome::Encoded { part, bytes });
            }
            drop(bytes);
            if let Some((left, right)) = part.bisect(self.source) {
                self.pending.push(right);
                self.next = Some(left);
            } else {
                return Some(Outcome::Oversized { part, encoded_size });
            }
        }
        None
    }
}

impl<T: TraceData> std::iter::FusedIterator for Payloads<'_, T> {}
