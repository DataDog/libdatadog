// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Envelope, PayloadAttrs, StringTable, encode_chunk_from_v04};
use crate::msgpack_encoder::flatten_value_write_infallible;
use crate::msgpack_encoder::packing::{Bounded, PushError, rotate_tail_to_front};
use crate::span::TraceData;
use crate::span::v04::Span;
use crate::tracer_metadata::TracerMetadata;
use libdd_common::ResultInfallibleExt;
use libdd_common::mutable_metadata::MutableMetadata;
use rmp::encode::ByteBuf;
use std::sync::Arc;

/// Builds one V1 payload from v0.4 chunks, keeping the complete payload within a byte limit.
///
/// Each accepted chunk is encoded once, against the payload's string table, and the promoted
/// payload metadata follows the chunks in push order. The result is byte-for-byte what
/// [`super::to_vec_from_v04`] produces for the accepted chunks.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::{PushError, v1::ChunkPacker};
/// use libdd_trace_utils::span::v04::SpanSlice;
/// use libdd_trace_utils::tracer_metadata::TracerMetadata;
///
/// let metadata = TracerMetadata::default();
/// let chunks = vec![vec![SpanSlice::default()]; 3];
/// let mut packer = ChunkPacker::new(&metadata, 64);
/// let mut payloads = Vec::new();
/// for chunk in &chunks {
///     if let Err(PushError::Full) = packer.push(chunk) {
///         payloads.push(packer.finish());
///         packer = ChunkPacker::new(&metadata, 64);
///         packer.push(chunk).expect("fits in an empty payload");
///     }
/// }
/// payloads.push(packer.finish());
/// assert!(payloads.iter().all(|payload| payload.len() <= 64));
/// ```
pub struct ChunkPacker<'a> {
    metadata: &'a TracerMetadata,
    /// Snapshot taken once, so that every size check measures the trailer that is written.
    mutable_metadata: Arc<MutableMetadata>,
    attrs: PayloadAttrs<'a>,
    table: StringTable,
    buf: Vec<u8>,
    limit: usize,
    chunks: usize,
}

impl<'a> ChunkPacker<'a> {
    /// Starts an empty payload that may grow to `limit` bytes.
    pub fn new(metadata: &'a TracerMetadata, limit: usize) -> Self {
        Self::with_capacity(metadata, limit, 0)
    }

    /// Like [`Self::new`], preallocating `capacity` bytes (at most `limit`).
    pub fn with_capacity(metadata: &'a TracerMetadata, limit: usize, capacity: usize) -> Self {
        Self {
            metadata,
            mutable_metadata: metadata.mutable_metadata.load_full(),
            attrs: PayloadAttrs::new(metadata),
            table: StringTable::new(),
            buf: Vec::with_capacity(capacity.min(limit)),
            limit,
            chunks: 0,
        }
    }

    /// Adds `spans` as the next chunk, unless the payload would exceed the limit.
    ///
    /// # Errors
    ///
    /// [`PushError::Full`] when the chunk does not fit; the payload is left unchanged. The work
    /// spent on a rejected chunk is bounded by the limit.
    pub fn push<T: TraceData + 'a>(&mut self, spans: &'a [Span<T>]) -> Result<(), PushError>
    where
        T::Text: 'a,
    {
        let Some(chunks) = self
            .chunks
            .checked_add(1)
            .filter(|chunks| u32::try_from(*chunks).is_ok())
        else {
            return Err(PushError::Full);
        };
        let header = Envelope::header_len(chunks);
        let (len, strings, attrs) = (self.buf.len(), self.table.len(), self.attrs);

        self.attrs.fill(spans);
        let max = self.limit.saturating_sub(header);
        let mut writer = Bounded::new(&mut self.buf, max, self.limit);
        if encode_chunk_from_v04(&mut writer, spans, &mut self.table).is_ok() {
            let trailer = self.envelope().trailer_len(&self.table);
            if header + self.buf.len() + trailer <= self.limit {
                self.chunks = chunks;
                return Ok(());
            }
        }

        self.buf.truncate(len);
        self.table.truncate(strings);
        self.attrs = attrs;
        Err(PushError::Full)
    }

    /// Number of chunks in the payload.
    pub fn len(&self) -> usize {
        self.chunks
    }

    pub fn is_empty(&self) -> bool {
        self.chunks == 0
    }

    /// Size of the payload [`Self::finish`] would return.
    pub fn encoded_len(&self) -> usize {
        Envelope::header_len(self.chunks)
            + self.buf.len()
            + self.envelope().trailer_len(&self.table)
    }

    /// Completes the payload. An empty packer yields a valid payload without chunks.
    pub fn finish(mut self) -> Vec<u8> {
        let envelope_len =
            Envelope::header_len(self.chunks) + self.envelope().trailer_len(&self.table);
        self.buf.reserve_exact(envelope_len);
        let envelope = Envelope {
            metadata: self.metadata,
            runtime_id: &self.mutable_metadata.runtime_id,
            attrs: self.attrs,
        };
        let mut buf = ByteBuf::from_vec(self.buf);
        envelope
            .write_trailer(&mut buf, &mut self.table)
            .map_err(flatten_value_write_infallible)
            .unwrap_infallible();
        let body = buf.as_slice().len();
        // `push` keeps `chunks` within u32.
        envelope
            .write_header(&mut buf, self.chunks as u32)
            .map_err(flatten_value_write_infallible)
            .unwrap_infallible();
        let mut bytes = buf.into_vec();
        rotate_tail_to_front(&mut bytes, body);
        bytes
    }

    fn envelope(&self) -> Envelope<'_> {
        Envelope {
            metadata: self.metadata,
            runtime_id: &self.mutable_metadata.runtime_id,
            attrs: self.attrs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_encoder::CountLength;

    /// Size checks rely on these lengths matching what is written.
    #[test]
    fn envelope_lengths_match_writes() {
        let full = TracerMetadata {
            language: "ruby".into(),
            language_version: "3.4".into(),
            tracer_version: "2.0".into(),
            container_id: "c".into(),
            env: "3.4".into(),
            hostname: "h".into(),
            app_version: "ruby".into(),
            git_commit_sha: "sha".into(),
            ..Default::default()
        };
        full.mutable_metadata.set_runtime_id("r".into());
        let mut attrs = PayloadAttrs::new(&full);
        attrs.apm_mode = Some("h");
        for (metadata, attrs) in [
            (&TracerMetadata::default(), PayloadAttrs::new(&full)),
            (&full, attrs),
        ] {
            let runtime_id = metadata.mutable_metadata.load_full();
            let envelope = Envelope {
                metadata,
                runtime_id: &runtime_id.runtime_id,
                attrs,
            };
            for chunks in [0, 15, 16, 0xffff, 0x1_0000] {
                let mut counter = CountLength(0);
                envelope.write_header(&mut counter, chunks).unwrap();
                assert_eq!(counter.0 as usize, Envelope::header_len(chunks as usize));
            }
            let mut table = StringTable::new();
            let mut counter = CountLength(0);
            table.write_interned(&mut counter, "ruby").unwrap();
            let expected = envelope.trailer_len(&table);
            let mut counter = CountLength(0);
            envelope.write_trailer(&mut counter, &mut table).unwrap();
            assert_eq!(counter.0 as usize, expected);
        }
    }
}
