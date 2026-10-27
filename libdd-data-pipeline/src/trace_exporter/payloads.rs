// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{TraceExporterOutputFormat, error::TraceExporterError};
use libdd_trace_utils::{
    msgpack_decoder::decode::error::DecodeError,
    msgpack_encoder::{PushError, v1, v04, v05},
    span::{TraceData, v04::Span},
    tracer_metadata::TracerMetadata,
};
use std::io;

pub(super) const DEFAULT_MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024;
type Check<'a> = &'a (dyn Fn() -> bool + Sync);

enum Packer<'a> {
    V04(v04::ChunkPacker),
    V05(v05::ChunkPacker),
    V1(v1::ChunkPacker<'a>),
}

impl<'a> Packer<'a> {
    fn new(
        metadata: &'a TracerMetadata,
        format: TraceExporterOutputFormat,
        limit: usize,
        capacity: usize,
    ) -> Self {
        match format {
            TraceExporterOutputFormat::V04 => {
                Self::V04(v04::ChunkPacker::with_capacity(limit, capacity))
            }
            TraceExporterOutputFormat::V05 => {
                Self::V05(v05::ChunkPacker::with_capacity(limit, capacity))
            }
            TraceExporterOutputFormat::V1 => {
                Self::V1(v1::ChunkPacker::with_capacity(metadata, limit, capacity))
            }
        }
    }

    fn push<T: TraceData + 'a>(&mut self, spans: &'a [Span<T>]) -> Result<(), PushError> {
        match self {
            Self::V04(packer) => packer.push(spans),
            Self::V05(packer) => packer.push(spans),
            Self::V1(packer) => packer.push(spans),
        }
    }

    fn encoded_len(&self) -> usize {
        match self {
            Self::V04(packer) => packer.encoded_len(),
            Self::V05(packer) => packer.encoded_len(),
            Self::V1(packer) => packer.encoded_len(),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            Self::V04(packer) => packer.finish(),
            Self::V05(packer) => packer.finish(),
            Self::V1(packer) => packer.finish(),
        }
    }
}

pub(super) enum Payload {
    Encoded {
        indices: Vec<usize>,
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

impl Payload {
    fn invalid(index: usize, error: anyhow::Error) -> Self {
        Self::Invalid {
            index,
            error: TraceExporterError::Deserialization(DecodeError::InvalidFormat(
                error.to_string(),
            )),
        }
    }
}

/// Incremental packing retains one payload and, at a boundary, one lookahead
/// chunk. An oversized chunk must not flush its eligible neighbours prematurely.
pub(super) struct Payloads<'a, T: TraceData> {
    traces: &'a [Vec<Span<T>>],
    metadata: &'a TracerMetadata,
    format: TraceExporterOutputFormat,
    limit: usize,
    stop: Check<'a>,
    position: usize,
    indices: Vec<usize>,
    packer: Packer<'a>,
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
            stop: &never_stop,
            position: 0,
            indices: Vec::with_capacity(traces.len().min(1024)),
            packer: Packer::new(metadata, format, limit, 1024.min(limit)),
        }
    }

    pub(super) fn with_limits(mut self, capacity: usize, stop: Check<'a>) -> Self {
        self.packer = Packer::new(self.metadata, self.format, self.limit, capacity);
        self.stop = stop;
        self
    }

    pub(super) fn fallback_to_v04(&mut self) {
        // After emitting a payload, only its lookahead can remain buffered.
        // Rewind that chunk without revisiting previously emitted local drops.
        if let Some(&index) = self.indices.first() {
            self.position = index;
        }
        self.indices.clear();
        self.format = TraceExporterOutputFormat::V04;
        self.packer = Packer::new(self.metadata, self.format, self.limit, 0);
    }
}

fn never_stop() -> bool {
    false
}

pub(super) fn empty(
    metadata: &TracerMetadata,
    format: TraceExporterOutputFormat,
    limit: usize,
    stop: Check<'_>,
) -> Result<Vec<u8>, TraceExporterError> {
    if stop() {
        return Err(io::Error::from(io::ErrorKind::Interrupted).into());
    }
    let packer = Packer::new(metadata, format, limit, 0);
    let size = packer.encoded_len();
    if size == usize::MAX || size > limit {
        return Err(io::Error::from(io::ErrorKind::FileTooLarge).into());
    }
    Ok(packer.finish())
}

impl<'a, T: TraceData> Iterator for Payloads<'a, T> {
    type Item = Payload;

    fn next(&mut self) -> Option<Payload> {
        loop {
            if self.position == self.traces.len() && self.indices.is_empty() {
                return None;
            }
            if (self.stop)() {
                return Some(Payload::Stopped);
            }
            if self.position == self.traces.len() {
                let next = Packer::new(self.metadata, self.format, self.limit, 0);
                let packed = std::mem::replace(&mut self.packer, next);
                return Some(Payload::Encoded {
                    indices: std::mem::take(&mut self.indices),
                    bytes: packed.finish(),
                });
            }

            let index = self.position;
            let spans = &self.traces[index];
            match self.packer.push(spans) {
                Ok(()) => {
                    self.indices.push(index);
                    self.position += 1;
                }
                Err(PushError::Invalid(error)) => {
                    self.position += 1;
                    return Some(Payload::invalid(index, error));
                }
                Err(PushError::Full) if self.indices.is_empty() => {
                    self.position += 1;
                    return Some(Payload::Oversized { index });
                }
                Err(PushError::Full) => {
                    if (self.stop)() {
                        return Some(Payload::Stopped);
                    }
                    let mut next = Packer::new(self.metadata, self.format, self.limit, 0);
                    let result = next.push(spans);
                    self.position += 1;
                    match result {
                        Ok(()) => {
                            let packed = std::mem::replace(&mut self.packer, next);
                            let mut indices = Vec::with_capacity(self.indices.len());
                            indices.push(index);
                            std::mem::swap(&mut self.indices, &mut indices);
                            return Some(Payload::Encoded {
                                indices,
                                bytes: packed.finish(),
                            });
                        }
                        Err(PushError::Full) => return Some(Payload::Oversized { index }),
                        Err(PushError::Invalid(error)) => {
                            return Some(Payload::invalid(index, error));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
