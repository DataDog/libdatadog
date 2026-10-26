// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    TraceExporterOutputFormat, error::TraceExporterError, trace_serializer::TraceSerializer,
};
use libdd_trace_utils::{
    msgpack_encoder::v1::PayloadAttrs,
    span::{TraceData, v04::Span},
    tracer_metadata::TracerMetadata,
};
use std::io;

pub(super) const DEFAULT_MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024;
type Check<'a> = &'a (dyn Fn() -> bool + Sync);

struct Limit<'a> {
    limit: usize,
    exceeded: bool,
    stopped: bool,
    next_check: usize,
    stop: Check<'a>,
}

impl Limit<'_> {
    #[inline]
    fn check(&mut self, length: usize, amount: usize) -> io::Result<()> {
        if length >= self.next_check || amount >= 64 * 1024 {
            if (self.stop)() {
                self.stopped = true;
                // `Write::write_all` retries Interrupted, but cancellation is terminal.
                return Err(io::Error::other("trace encoding stopped"));
            }
            self.next_check = length.saturating_add(64 * 1024);
        }
        if amount > self.limit - length {
            self.exceeded = true;
            return Err(io::Error::from(io::ErrorKind::FileTooLarge));
        }
        Ok(())
    }
}

struct LimitedBuffer<'a> {
    bytes: Vec<u8>,
    limit: Limit<'a>,
}

impl LimitedBuffer<'_> {
    #[cold]
    fn grow(&mut self, needed: usize) {
        let capacity = needed
            .max(self.bytes.capacity().saturating_mul(2))
            .max(1024)
            .min(self.limit.limit);
        self.bytes.reserve_exact(capacity - self.bytes.len());
    }
}

impl io::Write for LimitedBuffer<'_> {
    #[inline]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.limit.check(self.bytes.len(), bytes.len())?;
        let needed = self.bytes.len() + bytes.len();
        if needed > self.bytes.capacity() {
            self.grow(needed);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    #[inline]
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write(bytes).map(|_| ())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Counter<'a> {
    length: usize,
    limit: Limit<'a>,
}
impl io::Write for Counter<'_> {
    #[inline]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.limit.check(self.length, bytes.len())?;
        self.length += bytes.len();
        Ok(bytes.len())
    }
    #[inline]
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write(bytes).map(|_| ())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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

struct Chunk<'a, T: TraceData> {
    index: usize,
    spans: &'a [Span<T>],
    size: usize,
}

impl<T: TraceData> AsRef<[Span<T>]> for Chunk<'_, T> {
    fn as_ref(&self) -> &[Span<T>] {
        self.spans
    }
}

/// Normal batches encode once. After an overflow, isolate invalid/oversized
/// chunks, then search for fitting prefixes of the survivors. Probes count bytes
/// without allocating output, and reuse extracted V1 metadata.
pub(super) struct Payloads<'a, T: TraceData> {
    traces: &'a [Vec<Span<T>>],
    metadata: &'a TracerMetadata,
    format: TraceExporterOutputFormat,
    limit: usize,
    capacity: usize,
    stop: Check<'a>,
    first: bool,
    scanned: usize,
    position: usize,
    eligible: Vec<Chunk<'a, T>>,
    attributes: Vec<PayloadAttrs<'a>>,
    pending: Option<Vec<usize>>,
}

enum Encoded {
    Fits { size: usize, bytes: Vec<u8> },
    TooLarge,
    Invalid(TraceExporterError),
    Stopped,
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
            capacity: 1024.min(limit),
            stop: &never_stop,
            first: true,
            scanned: 0,
            position: 0,
            eligible: Vec::new(),
            attributes: Vec::new(),
            pending: None,
        }
    }

    pub(super) fn with_limits(mut self, capacity: usize, stop: Check<'a>) -> Self {
        self.capacity = capacity.min(self.limit);
        self.stop = stop;
        self
    }

    pub(super) fn fallback_to_v04(&mut self) {
        self.pending = Some(
            self.eligible[self.position..]
                .iter()
                .map(|chunk| chunk.index)
                .collect(),
        );
        self.format = TraceExporterOutputFormat::V04;
        self.first = false;
        self.scanned = 0;
        self.position = 0;
        self.eligible.clear();
        self.attributes.clear();
    }

    fn input_len(&self) -> usize {
        self.pending.as_ref().map_or(self.traces.len(), Vec::len)
    }

    fn attrs(&self, spans: &'a [Span<T>]) -> Option<PayloadAttrs<'a>> {
        let mut attrs = PayloadAttrs::from_metadata(self.metadata);
        if self.format == TraceExporterOutputFormat::V1 {
            for span in spans {
                if (self.stop)() {
                    return None;
                }
                attrs.merge(PayloadAttrs::from_span(span, self.metadata));
            }
        }
        Some(attrs)
    }

    fn encode<S: AsRef<[Span<T>]>>(
        &self,
        chunks: &[S],
        attrs: PayloadAttrs<'_>,
        keep: bool,
    ) -> Encoded {
        if (self.stop)() {
            return Encoded::Stopped;
        }
        let limit = Limit {
            limit: self.limit,
            exceeded: false,
            stopped: false,
            next_check: 64 * 1024,
            stop: self.stop,
        };
        if keep {
            let mut writer = LimitedBuffer {
                bytes: Vec::with_capacity(self.capacity),
                limit,
            };
            let result = TraceSerializer::encode_to_writer(
                &mut writer,
                chunks,
                self.metadata,
                self.format,
                Some(attrs),
            );
            finish_encoding(result, writer.limit, writer.bytes.len(), writer.bytes)
        } else {
            let mut writer = Counter { length: 0, limit };
            let result = TraceSerializer::encode_to_writer(
                &mut writer,
                chunks,
                self.metadata,
                self.format,
                Some(attrs),
            );
            finish_encoding(result, writer.limit, writer.length, Vec::new())
        }
    }

    fn encode_range(&self, end: usize, keep: bool) -> Encoded {
        let chunks = &self.eligible[self.position..end];
        let mut attrs = PayloadAttrs::from_metadata(self.metadata);
        if self.format == TraceExporterOutputFormat::V1 {
            for value in &self.attributes[self.position..end] {
                if (self.stop)() {
                    return Encoded::Stopped;
                }
                attrs.merge(*value);
            }
        }
        self.encode(chunks, attrs, keep)
    }
}

fn finish_encoding(
    result: Result<(), TraceExporterError>,
    limit: Limit<'_>,
    size: usize,
    bytes: Vec<u8>,
) -> Encoded {
    if limit.stopped {
        Encoded::Stopped
    } else if limit.exceeded {
        Encoded::TooLarge
    } else {
        match result {
            Ok(()) => Encoded::Fits { size, bytes },
            Err(error) => Encoded::Invalid(error),
        }
    }
}

fn never_stop() -> bool {
    false
}

pub(super) fn empty<T: TraceData>(
    metadata: &TracerMetadata,
    format: TraceExporterOutputFormat,
    limit: usize,
    stop: Check<'_>,
) -> Result<Vec<u8>, TraceExporterError> {
    let mut writer = LimitedBuffer {
        bytes: Vec::with_capacity(1024.min(limit)),
        limit: Limit {
            limit,
            exceeded: false,
            stopped: false,
            next_check: 0,
            stop,
        },
    };
    let traces: &[Vec<Span<T>>] = &[];
    TraceSerializer::encode_to_writer(&mut writer, traces, metadata, format, None)?;
    Ok(writer.bytes)
}

impl<T: TraceData> Iterator for Payloads<'_, T> {
    type Item = Payload;
    fn next(&mut self) -> Option<Payload> {
        if self.traces.is_empty()
            || (!self.first
                && self.scanned == self.input_len()
                && self.position == self.eligible.len())
        {
            return None;
        }
        if (self.stop)() {
            return Some(Payload::Stopped);
        }
        if self.first {
            self.first = false;
            let mut attrs = PayloadAttrs::from_metadata(self.metadata);
            if self.format == TraceExporterOutputFormat::V1 {
                for chunk in self.traces {
                    let Some(value) = self.attrs(chunk) else {
                        return Some(Payload::Stopped);
                    };
                    attrs.merge(value);
                }
            }
            if let Encoded::Fits { bytes, .. } = self.encode(self.traces, attrs, true) {
                self.scanned = self.traces.len();
                return Some(Payload::Encoded {
                    indices: (0..self.traces.len()).collect(),
                    bytes,
                });
            }
        }
        while self.scanned < self.input_len() {
            let index = self
                .pending
                .as_ref()
                .map_or(self.scanned, |pending| pending[self.scanned]);
            let spans = &self.traces[index];
            let Some(attrs) = self.attrs(spans) else {
                return Some(Payload::Stopped);
            };
            let result = self.encode(std::slice::from_ref(spans), attrs, false);
            if matches!(result, Encoded::Stopped) {
                return Some(Payload::Stopped);
            }
            self.scanned += 1;
            match result {
                Encoded::Fits { size, .. } => {
                    self.eligible.push(Chunk { index, spans, size });
                    if self.format == TraceExporterOutputFormat::V1 {
                        self.attributes.push(attrs);
                    }
                }
                Encoded::TooLarge => return Some(Payload::Oversized { index }),
                Encoded::Invalid(error) => return Some(Payload::Invalid { index, error }),
                Encoded::Stopped => return Some(Payload::Stopped),
            }
        }
        if self.position == self.eligible.len() {
            return None;
        }
        let start = self.position;
        let mut best = start + 1;
        if self.format == TraceExporterOutputFormat::V04 {
            let mut body = 0usize;
            for (offset, chunk) in self.eligible[start..].iter().enumerate() {
                if (self.stop)() {
                    return Some(Payload::Stopped);
                }
                let count = offset + 1;
                if u32::try_from(count).is_err() {
                    break;
                }
                let header = if count < 16 {
                    1
                } else if count <= usize::from(u16::MAX) {
                    3
                } else {
                    5
                };
                // Each singleton measurement includes a one-byte outer array.
                let Some(next) = body.checked_add(chunk.size - 1) else {
                    break;
                };
                if next
                    .checked_add(header)
                    .is_none_or(|size| size > self.limit)
                {
                    break;
                }
                body = next;
                best = start + count;
            }
        } else {
            let mut upper = self.eligible.len();
            let mut probe = best;
            loop {
                match self.encode_range(probe, false) {
                    Encoded::Fits { .. } => {
                        best = probe;
                    }
                    Encoded::Stopped => return Some(Payload::Stopped),
                    _ => {
                        upper = probe - 1;
                        break;
                    }
                }
                if probe == upper {
                    break;
                }
                probe = (start + (probe - start).saturating_mul(2)).min(upper);
            }
            while best < upper {
                let middle = best + (upper - best).div_ceil(2);
                match self.encode_range(middle, false) {
                    Encoded::Fits { .. } => best = middle,
                    Encoded::Stopped => return Some(Payload::Stopped),
                    _ => upper = middle - 1,
                }
            }
        }
        loop {
            match self.encode_range(best, true) {
                Encoded::Fits { size, bytes } => {
                    self.capacity = size.min(self.limit);
                    let indices = self.eligible[start..best]
                        .iter()
                        .map(|chunk| chunk.index)
                        .collect();
                    self.position = best;
                    return Some(Payload::Encoded { indices, bytes });
                }
                Encoded::Stopped => return Some(Payload::Stopped),
                Encoded::TooLarge | Encoded::Invalid(_) if best > start + 1 => {
                    best = start + (best - start) / 2;
                }
                Encoded::TooLarge => {
                    self.position += 1;
                    return Some(Payload::Oversized {
                        index: self.eligible[start].index,
                    });
                }
                Encoded::Invalid(error) => {
                    self.position += 1;
                    return Some(Payload::Invalid {
                        index: self.eligible[start].index,
                        error,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
