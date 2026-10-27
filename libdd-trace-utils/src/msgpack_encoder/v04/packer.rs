// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::encode_chunk;
use crate::msgpack_encoder::packing::{Bounded, PushError, header_len, rotate_tail_to_front};
use crate::span::TraceData;
use crate::span::v04::Span;
use rmp::encode::write_array_len;

/// Builds one v0.4 payload from chunks, keeping the complete payload within a byte limit.
///
/// Each accepted chunk is encoded once. The result is byte-for-byte what
/// [`super::to_vec_from_v04`] produces for the accepted chunks.
pub struct ChunkPacker {
    buf: Vec<u8>,
    limit: usize,
    chunks: usize,
}

impl ChunkPacker {
    /// Starts an empty payload that may grow to `limit` bytes.
    pub fn new(limit: usize) -> Self {
        Self::with_capacity(limit, 0)
    }

    /// Like [`Self::new`], preallocating `capacity` bytes (at most `limit`).
    pub fn with_capacity(limit: usize, capacity: usize) -> Self {
        Self {
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
    pub fn push<T: TraceData>(&mut self, spans: &[Span<T>]) -> Result<(), PushError> {
        let Some(chunks) = self
            .chunks
            .checked_add(1)
            .filter(|chunks| u32::try_from(*chunks).is_ok())
        else {
            return Err(PushError::Full);
        };
        let len = self.buf.len();
        let max = self.limit.saturating_sub(header_len(chunks));
        if encode_chunk(&mut Bounded::new(&mut self.buf, max, self.limit), spans).is_err() {
            self.buf.truncate(len);
            return Err(PushError::Full);
        }
        self.chunks = chunks;
        Ok(())
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
        header_len(self.chunks) + self.buf.len()
    }

    /// Completes the payload. An empty packer yields a valid payload without chunks.
    pub fn finish(mut self) -> Vec<u8> {
        let body = self.buf.len();
        let mut header = [0u8; 5];
        let mut slice = &mut header[..];
        // Five bytes hold any array header, and `push` keeps `chunks` within u32.
        let _ = write_array_len(&mut slice, self.chunks as u32);
        let header = &header[..header_len(self.chunks)];
        self.buf.reserve_exact(header.len());
        self.buf.extend_from_slice(header);
        rotate_tail_to_front(&mut self.buf, body);
        self.buf
    }
}
