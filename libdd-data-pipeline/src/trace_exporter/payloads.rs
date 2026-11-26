// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    TraceExporterOutputFormat, error::TraceExporterError, trace_serializer::TraceSerializer,
};
use libdd_trace_utils::{
    msgpack_encoder::split::ChunkRanges,
    span::{TraceData, v04::Span},
    tracer_metadata::TracerMetadata,
};
use std::{io, ops::Range};

pub(super) const DEFAULT_MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024;
pub(super) enum Payload {
    Encoded {
        indices: Range<usize>,
        bytes: Vec<u8>,
    },
    Oversized {
        index: usize,
    },
    Invalid {
        index: usize,
        error: TraceExporterError,
    },
    Stopped,
}

/// Ruby supplies v0.4-shaped input even when the negotiated wire format is V1.
/// Use the legacy codec for each candidate so promotion and dictionary scope
/// match a normal send of exactly those chunks.
pub(super) struct Payloads<'a, T: TraceData> {
    traces: &'a [Vec<Span<T>>],
    metadata: &'a TracerMetadata,
    format: TraceExporterOutputFormat,
    limit: usize,
    ranges: ChunkRanges,
    pub(super) initial_capacity: usize,
}

impl<'a, T: TraceData> Payloads<'a, T> {
    pub(super) fn new(
        traces: &'a [Vec<Span<T>>],
        metadata: &'a TracerMetadata,
        format: TraceExporterOutputFormat,
        limit: usize,
    ) -> Self {
        Self {
            traces,
            metadata,
            format,
            limit,
            ranges: ChunkRanges::new(0..traces.len()),
            initial_capacity: 0,
        }
    }

    pub(super) fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    pub(super) fn fallback_to_v04(&mut self) {
        self.format = TraceExporterOutputFormat::V04;
    }

    pub(super) fn next(&mut self, stop: &impl Fn() -> bool) -> Option<Payload> {
        // Empty batches are sent separately, only when they carry pending P0 headers.
        if self.traces.is_empty() {
            return None;
        }
        let (indices, encoded) = self.ranges.next_with(
            |r| {
                (!stop()).then(|| {
                    let capacity = std::mem::take(&mut self.initial_capacity)
                        .min(self.limit)
                        .min(u32::MAX as usize);
                    TraceSerializer::encode_range(
                        &self.traces[r],
                        self.metadata,
                        self.format,
                        capacity,
                    )
                })
            },
            |result| match result {
                None => false,
                Some(Err(_)) => true,
                Some(Ok(bytes)) => bytes.len() > self.limit,
            },
        )?;
        Some(match encoded {
            None => Payload::Stopped,
            Some(Err(error)) => Payload::Invalid {
                index: indices.start,
                error,
            },
            Some(Ok(bytes)) if bytes.len() > self.limit => Payload::Oversized {
                index: indices.start,
            },
            Some(Ok(bytes)) => Payload::Encoded { indices, bytes },
        })
    }
}

pub(super) fn empty(
    metadata: &TracerMetadata,
    format: TraceExporterOutputFormat,
    limit: usize,
    stop: &impl Fn() -> bool,
) -> Result<Vec<u8>, TraceExporterError> {
    if stop() {
        return Err(io::Error::from(io::ErrorKind::Interrupted).into());
    }
    let bytes = TraceSerializer::encode_range::<libdd_trace_utils::span::BytesData>(
        &[],
        metadata,
        format,
        0,
    )?;
    if bytes.len() > limit {
        return Err(io::Error::from(io::ErrorKind::FileTooLarge).into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
