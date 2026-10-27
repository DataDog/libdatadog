// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! v0.5 wire format: `[dictionary, chunks]`, where spans refer to strings by dictionary index.

use crate::msgpack_encoder::flatten_value_write_infallible;
use crate::msgpack_encoder::packing::{
    Bounded, PushError, header_len, rotate_tail_to_front, str_len, total, within,
};
use crate::span::v04::Span;
use crate::span::{SharedDictBytes, TraceData, v05};
use libdd_common::ResultInfallibleExt;
use rmp::encode::{ByteBuf, RmpWrite, ValueWriteError, write_array_len, write_str};
use serde::Serialize;

/// Builds one v0.5 payload from v0.4 chunks, keeping the complete payload within a byte limit
/// once a chunk was pushed. An empty payload exceeds limits below its few bytes: see
/// [`Self::encoded_len`].
///
/// Each accepted chunk is converted and encoded once, span by span, against the payload's
/// dictionary; no intermediate representation of the whole payload is built. The dictionary
/// and spans are those of [`crate::trace_utils::convert_trace_chunks_v04_to_v05`] for the
/// accepted chunks, serialized as `rmp_serde` would. Only the order of entries within each
/// span's `meta` and `metrics` maps may differ, as with any two encodings of those hash maps.
pub struct ChunkPacker {
    dict: SharedDictBytes,
    /// Encoded size of the strings in `dict`.
    strings_len: usize,
    /// Encoded chunks, without the payload header and dictionary that precede them.
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
        let dict = SharedDictBytes::default();
        let strings_len = dict.iter().map(|s| str_len(s.as_str().len())).sum();
        Self {
            dict,
            strings_len,
            buf: Vec::with_capacity(capacity.min(limit)),
            limit,
            chunks: 0,
        }
    }

    /// Adds `spans` as the next chunk, unless the payload would exceed the limit.
    ///
    /// # Errors
    ///
    /// - [`PushError::Full`] when the chunk does not fit. The work spent on a rejected chunk is
    ///   bounded by the limit, except for strings copied into the dictionary from borrowed span
    ///   text before the overflow is detected.
    /// - [`PushError::Invalid`] when a span cannot be converted to v0.5.
    ///
    /// Either way the payload is left unchanged.
    pub fn push<T: TraceData>(&mut self, spans: &[Span<T>]) -> Result<(), PushError> {
        let Some(chunks) = self
            .chunks
            .checked_add(1)
            .filter(|chunks| u32::try_from(*chunks).is_ok())
        else {
            return Err(PushError::Full);
        };
        let (len, strings, strings_len) = (self.buf.len(), self.dict.len(), self.strings_len);
        let result = self.encode_chunk(spans, chunks).and_then(|()| {
            let prefix = self.prefix_len(chunks);
            if prefix.is_some_and(|prefix| within(self.limit, &[prefix, self.buf.len()])) {
                Ok(())
            } else {
                Err(PushError::Full)
            }
        });
        match result {
            Ok(()) => self.chunks = chunks,
            Err(_) => {
                self.buf.truncate(len);
                self.dict.map.truncate(strings);
                self.strings_len = strings_len;
            }
        }
        result
    }

    fn encode_chunk<T: TraceData>(
        &mut self,
        spans: &[Span<T>],
        chunks: usize,
    ) -> Result<(), PushError> {
        let max = remaining(self.limit, self.prefix_len(chunks));
        let mut writer = Bounded::new(&mut self.buf, max, self.limit);
        // A span count beyond u32 cannot fit within a limit that is a usize.
        let count = u32::try_from(spans.len()).map_err(|_| PushError::Full)?;
        write_array_len(&mut writer, count).map_err(|_| PushError::Full)?;
        for span in spans {
            let strings = self.dict.len();
            let span = v05::from_v04_span(span, &mut self.dict).map_err(PushError::Invalid)?;
            if u32::try_from(self.dict.len()).is_err() {
                return Err(PushError::Full);
            }
            self.strings_len = self.dict.map.as_slice()[strings..]
                .keys()
                .try_fold(self.strings_len, |len, s| {
                    len.checked_add(str_len(s.as_str().len()))
                })
                .ok_or(PushError::Full)?;
            writer.set_max(remaining(
                self.limit,
                prefix_len(&self.dict, self.strings_len, chunks),
            ));
            if let Err(error) = span.serialize(&mut rmp_serde::Serializer::new(&mut writer)) {
                return Err(if writer.is_full() {
                    PushError::Full
                } else {
                    PushError::Invalid(error.into())
                });
            }
        }
        Ok(())
    }

    /// Number of chunks in the payload.
    pub fn len(&self) -> usize {
        self.chunks
    }

    pub fn is_empty(&self) -> bool {
        self.chunks == 0
    }

    /// Size of the payload [`Self::finish`] would return, saturating at `usize::MAX`.
    ///
    /// Within the limit once a chunk was pushed. For an empty packer, this is the size of the
    /// bare envelope, which can exceed the limit: check it before sending an empty payload.
    pub fn encoded_len(&self) -> usize {
        self.prefix_len(self.chunks)
            .map_or(usize::MAX, |prefix| prefix.saturating_add(self.buf.len()))
    }

    /// Completes the payload. An empty packer yields a valid payload without chunks, which may
    /// exceed the limit (see [`Self::encoded_len`]).
    pub fn finish(mut self) -> Vec<u8> {
        let body = self.buf.len();
        if let Some(prefix) = self.prefix_len(self.chunks) {
            self.buf.reserve_exact(prefix);
        }
        let mut buf = ByteBuf::from_vec(self.buf);
        write_prefix(&mut buf, &self.dict, self.chunks)
            .map_err(flatten_value_write_infallible)
            .unwrap_infallible();
        let mut bytes = buf.into_vec();
        rotate_tail_to_front(&mut bytes, body);
        bytes
    }

    fn prefix_len(&self, chunks: usize) -> Option<usize> {
        prefix_len(&self.dict, self.strings_len, chunks)
    }
}

/// Encoded size of [`write_prefix`], or `None` if it cannot be represented.
fn prefix_len(dict: &SharedDictBytes, strings_len: usize, chunks: usize) -> Option<usize> {
    total(&[1, header_len(dict.len()), strings_len, header_len(chunks)])
}

/// Room left for chunks once `prefix` bytes are taken, none if `prefix` is unrepresentable.
fn remaining(limit: usize, prefix: Option<usize>) -> usize {
    prefix.map_or(0, |prefix| limit.saturating_sub(prefix))
}

/// Writes everything that precedes the chunks: the payload header, the dictionary, and the
/// chunks array header.
fn write_prefix<W: RmpWrite>(
    writer: &mut W,
    dict: &SharedDictBytes,
    chunks: usize,
) -> Result<(), ValueWriteError<W::Error>> {
    write_array_len(writer, 2)?;
    // `push` keeps the dictionary size and the chunk count within u32.
    write_array_len(writer, dict.len() as u32)?;
    for s in dict.iter() {
        write_str(writer, s.as_str())?;
    }
    write_array_len(writer, chunks as u32)?;
    Ok(())
}
