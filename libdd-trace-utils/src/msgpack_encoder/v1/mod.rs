// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span_v04;

use crate::span::v04::Span;
use crate::span::TraceData;
use rmp::encode::{
    write_array_len, write_bin, write_map_len, write_sint, write_str, write_uint, write_uint8,
    ByteBuf, RmpWrite, ValueWriteError,
};
use std::borrow::Borrow;
use std::collections::HashMap;

/// Integer keys for the top-level V1 trace payload map.
#[repr(u8)]
enum TraceKey {
    Chunks = 11,
}

/// Integer keys for V1 chunk-level fields.
#[repr(u8)]
enum ChunkKey {
    Priority = 1,
    Origin = 2,
    Spans = 4,
    TraceId = 6,
}

/// Streaming string intern table.
///
/// The first time a string is written, it is emitted as a msgpack `str` and assigned an
/// incrementing integer ID. On subsequent occurrences only the ID is emitted as a msgpack `uint`.
/// ID 0 is reserved for the empty string (written as fixint `0`).
///
/// The string table is scoped per payload: each `to_vec` / `write_to_slice` call starts with a
/// fresh table so deduplication is payload-local.
pub(crate) struct StringTable {
    seen: HashMap<String, u32>,
    next_id: u32,
}

impl StringTable {
    fn new() -> Self {
        Self {
            seen: HashMap::new(),
            next_id: 1,
        }
    }

    /// Writes `s` to `writer` using string interning.
    ///
    /// - Empty string → fixint `0`
    /// - First occurrence of `s` → msgpack `str`, ID recorded for future references
    /// - Subsequent occurrence → msgpack `uint` carrying the previously assigned ID
    pub(crate) fn write_interned<W: RmpWrite, S: AsRef<str>>(
        &mut self,
        writer: &mut W,
        s: S,
    ) -> Result<(), ValueWriteError<W::Error>> {
        let s = s.as_ref();
        if s.is_empty() {
            write_uint8(writer, 0)?;
            return Ok(());
        }
        if let Some(&id) = self.seen.get(s) {
            write_uint(writer, id as u64)?;
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.seen.insert(s.to_string(), id);
            write_str(writer, s)?;
        }
        Ok(())
    }
}

/// Promoted fields extracted from spans and written at the chunk level.
struct ChunkAttrs {
    /// Full 128-bit trace ID (encodes as 16-byte big-endian binary).
    trace_id: u128,
    /// Sampling priority from `_sampling_priority_v1` metric on the root span.
    sampling_priority: Option<i32>,
    /// Origin tag from `_dd.origin` meta on the root span.
    origin: Option<String>,
}

fn extract_chunk_attrs<T: TraceData>(spans: &[Span<T>]) -> ChunkAttrs {
    let mut trace_id = 0u128;
    let mut sampling_priority = None;
    let mut origin: Option<String> = None;

    for span in spans {
        // Any span gives us the trace_id.
        trace_id = span.trace_id;

        // Chunk-level attributes come from the root span (parent_id == 0).
        if span.parent_id == 0 {
            // HashMap::get accepts &Q where K: Borrow<Q>; T::Text: Borrow<str> so &str works.
            if let Some(v) = span.metrics.get("_sampling_priority_v1") {
                sampling_priority = Some(*v as i32);
            }
            if let Some(v) = span.meta.get("_dd.origin") {
                origin = Some(v.borrow().to_owned());
            }
        }
    }

    ChunkAttrs {
        trace_id,
        sampling_priority,
        origin,
    }
}

/// Encodes all traces as a V1 msgpack payload.
///
/// Top-level format:
/// ```text
/// Map {
///   TraceKey::Chunks (11) → Array[Chunk, ...]
/// }
/// ```
fn encode_payload<W: RmpWrite, T: TraceData, S: AsRef<[Span<T>]>>(
    writer: &mut W,
    traces: &[S],
) -> Result<(), ValueWriteError<W::Error>> {
    let mut table = StringTable::new();

    // Top-level map contains only the chunks array for now.
    write_map_len(writer, 1)?;
    write_uint8(writer, TraceKey::Chunks as u8)?;

    write_array_len(writer, traces.len() as u32)?;
    for trace in traces {
        encode_chunk(writer, trace.as_ref(), &mut table)?;
    }

    Ok(())
}

/// Encodes one chunk (a group of spans sharing a trace ID).
///
/// ```text
/// Map {
///   ChunkKey::TraceId  (6) → bin[16]       // 128-bit big-endian
///   ChunkKey::Origin   (2) → str|uint       // optional, interned
///   ChunkKey::Priority (1) → int            // optional
///   ChunkKey::Spans    (4) → Array[Span, ...]
/// }
/// ```
fn encode_chunk<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    spans: &[Span<T>],
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    let attrs = extract_chunk_attrs(spans);

    let fields = 2u32 // trace_id + spans are always present
        + attrs.origin.is_some() as u32
        + attrs.sampling_priority.is_some() as u32;

    write_map_len(writer, fields)?;

    // 128-bit trace ID as 16-byte big-endian binary.
    write_uint8(writer, ChunkKey::TraceId as u8)?;
    write_bin(writer, &attrs.trace_id.to_be_bytes())?;

    if let Some(ref origin) = attrs.origin {
        write_uint8(writer, ChunkKey::Origin as u8)?;
        table.write_interned(writer, origin.as_str())?;
    }

    if let Some(priority) = attrs.sampling_priority {
        write_uint8(writer, ChunkKey::Priority as u8)?;
        write_sint(writer, priority as i64)?;
    }

    write_uint8(writer, ChunkKey::Spans as u8)?;
    write_array_len(writer, spans.len() as u32)?;
    for span in spans {
        span_v04::encode_span(writer, span, table)?;
    }

    Ok(())
}

/// Serializes traces into a slice using the V1 msgpack format.
///
/// # Errors
/// Returns a `ValueWriteError` if the underlying writer fails.
pub fn write_to_slice<T: TraceData, S: AsRef<[Span<T>]>>(
    slice: &mut &mut [u8],
    traces: &[S],
) -> Result<(), ValueWriteError> {
    encode_payload(slice, traces)
}

/// Serializes traces into a `Vec<u8>` using the V1 msgpack format.
pub fn to_vec<T: TraceData, S: AsRef<[Span<T>]>>(traces: &[S]) -> Vec<u8> {
    to_vec_with_capacity(traces, 0)
}

/// Serializes traces into a `Vec<u8>` with a pre-allocated capacity.
pub fn to_vec_with_capacity<T: TraceData, S: AsRef<[Span<T>]>>(
    traces: &[S],
    capacity: u32,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    #[allow(clippy::expect_used)]
    encode_payload(&mut buf, traces).expect("infallible: the error is std::convert::Infallible");
    buf.into_vec()
}

/// Returns the number of bytes the V1 payload for `traces` would occupy.
pub fn to_len<T: TraceData, S: AsRef<[Span<T>]>>(traces: &[S]) -> u32 {
    let mut counter = super::CountLength(0);
    #[allow(clippy::expect_used)]
    encode_payload(&mut counter, traces).expect("infallible: CountLength never fails");
    counter.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::SpanBytes;
    use libdd_tinybytes::BytesString;
    use std::collections::HashMap;

    fn make_span(
        service: &str,
        name: &str,
        trace_id: u128,
        span_id: u64,
        parent_id: u64,
    ) -> SpanBytes {
        SpanBytes {
            service: BytesString::from_slice(service.as_bytes()).unwrap(),
            name: BytesString::from_slice(name.as_bytes()).unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id,
            span_id,
            parent_id,
            start: 1_000_000,
            duration: 500,
            ..Default::default()
        }
    }

    #[test]
    fn test_to_vec_non_empty() {
        let spans = vec![make_span("svc", "op", 42, 1, 0)];
        let traces = vec![spans];
        let encoded = to_vec(&traces);
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_to_vec_empty_traces() {
        let traces: Vec<Vec<SpanBytes>> = vec![];
        let encoded = to_vec(&traces);
        // Must still produce a valid msgpack map with an empty chunks array.
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_string_interning_reduces_size() {
        // Two spans with the same service name — second occurrence should use the integer ID.
        let s1 = make_span("my-service", "op1", 1, 1, 0);
        let s2 = make_span("my-service", "op2", 2, 2, 0);
        let traces_two = vec![vec![s1], vec![s2]];

        // Single span for baseline.
        let s_single = make_span("my-service", "op1", 1, 1, 0);
        let traces_single = vec![vec![s_single]];

        let encoded_two = to_vec(&traces_two);
        let encoded_single = to_vec(&traces_single);

        // The two-trace payload should be less than 2× the single-trace payload
        // if interning is working (the second "my-service" is encoded as an integer).
        assert!(
            encoded_two.len() < 2 * encoded_single.len(),
            "Interning should reduce size: two={} single={}",
            encoded_two.len(),
            encoded_single.len()
        );
    }

    #[test]
    fn test_chunk_level_attrs_origin_and_priority() {
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("_dd.origin"),
            BytesString::from_static("lambda"),
        );
        let mut metrics = HashMap::new();
        metrics.insert(BytesString::from_static("_sampling_priority_v1"), 1.0f64);

        let root = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 99,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            metrics,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![root]]);
        assert!(!encoded.is_empty());
        // The payload must contain "lambda" somewhere (the origin string).
        let lambda_bytes = b"lambda";
        assert!(
            encoded
                .windows(lambda_bytes.len())
                .any(|w| w == lambda_bytes),
            "origin 'lambda' should appear in payload"
        );
    }

    #[test]
    fn test_to_len_matches_to_vec() {
        let spans = vec![
            make_span("svc", "op", 1, 1, 0),
            make_span("svc", "child", 1, 2, 1),
        ];
        let traces = vec![spans];
        let encoded = to_vec(&traces);
        let len = to_len(&traces);
        assert_eq!(encoded.len() as u32, len);
    }
}
