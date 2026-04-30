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
mod trace_key {
    pub const ENV_REF: u8 = 7;
    pub const HOSTNAME_REF: u8 = 8;
    pub const APP_VERSION_REF: u8 = 9;
    pub const CHUNKS: u8 = 11;
}

/// Integer keys for V1 chunk-level fields.
mod chunk_key {
    pub const PRIORITY: u8 = 1;
    pub const ORIGIN: u8 = 2;
    pub const SPANS: u8 = 4;
    pub const TRACE_ID: u8 = 6;
    /// Sampling mechanism (previously the `_dd.p.dm` span tag).
    pub const SAMPLING_MECHANISM: u8 = 7;
}

/// Streaming string intern table.
///
/// The first time a string is written, it is emitted as a msgpack `str` and assigned an
/// incrementing integer ID. On subsequent occurrences only the ID is emitted as a msgpack `uint`.
/// ID 0 is reserved for the empty string (pre-inserted in the constructor).
///
/// The string table is scoped per payload: each `to_vec` / `write_to_slice` call starts with a
/// fresh table so deduplication is payload-local.
pub(crate) struct StringTable {
    seen: HashMap<String, u32>,
}

impl StringTable {
    fn new() -> Self {
        let mut seen = HashMap::new();
        seen.insert(String::new(), 0);
        Self { seen }
    }

    /// Writes `s` to `writer` using string interning.
    ///
    /// - First occurrence of `s` → msgpack `str`, ID recorded for future references
    /// - Subsequent occurrence → msgpack `uint` carrying the previously assigned ID
    pub(crate) fn write_interned<W: RmpWrite, S: AsRef<str>>(
        &mut self,
        writer: &mut W,
        s: S,
    ) -> Result<(), ValueWriteError<W::Error>> {
        let s = s.as_ref();
        if let Some(&id) = self.seen.get(s) {
            write_uint(writer, id as u64)?;
        } else {
            let id = self.seen.len() as u32;
            self.seen.insert(s.to_string(), id);
            write_str(writer, s)?;
        }
        Ok(())
    }
}

/// Promoted fields extracted from the payload's spans, written at the top-level map.
struct PayloadAttrs<'a> {
    env: Option<&'a str>,
    hostname: Option<&'a str>,
    app_version: Option<&'a str>,
}

fn extract_payload_attrs<'a, T: TraceData + 'a, S: AsRef<[Span<T>]>>(
    traces: &'a [S],
) -> PayloadAttrs<'a>
where
    T::Text: 'a,
{
    let mut env = None;
    let mut hostname = None;
    let mut app_version = None;

    'outer: for trace in traces {
        for span in trace.as_ref() {
            if env.is_none() {
                env = span.meta.get("env").map(|v| v.borrow());
            }
            if hostname.is_none() {
                hostname = span.meta.get("_dd.hostname").map(|v| v.borrow());
            }
            if app_version.is_none() {
                app_version = span.meta.get("version").map(|v| v.borrow());
            }
            if env.is_some() && hostname.is_some() && app_version.is_some() {
                break 'outer;
            }
        }
    }

    PayloadAttrs {
        env,
        hostname,
        app_version,
    }
}

/// Promoted fields extracted from spans and written at the chunk level.
struct ChunkAttrs<'a> {
    /// Full 128-bit trace ID (encodes as 16-byte big-endian binary).
    trace_id: u128,
    /// Sampling priority from `_sampling_priority_v1` metric on the root span.
    sampling_priority: Option<i32>,
    /// Origin tag from `_dd.origin` meta on the root span.
    origin: Option<&'a str>,
    /// Sampling mechanism from `_dd.p.dm` meta on the root span.
    sampling_mechanism: Option<u32>,
}

fn extract_chunk_attrs<'a, T: TraceData>(spans: &'a [Span<T>]) -> ChunkAttrs<'a>
where
    T::Text: 'a,
{
    let mut trace_id = 0u128;
    let mut sampling_priority = None;
    let mut origin = None;
    let mut sampling_mechanism = None;

    for span in spans {
        trace_id = span.trace_id;

        // Root span: either no parent in this chunk, or tagged _dd.top_level=1 (remote parent).
        let is_root =
            span.parent_id == 0 || span.metrics.get("_dd.top_level").copied().unwrap_or(0.0) == 1.0;

        if is_root {
            if let Some(v) = span.metrics.get("_sampling_priority_v1") {
                sampling_priority = Some(*v as i32);
            }
            if let Some(v) = span.meta.get("_dd.origin") {
                origin = Some(v.borrow());
            }
            // _dd.p.dm is a signed integer sampling mechanism code stored as a string.
            if let Some(v) = span.meta.get("_dd.p.dm") {
                if let Ok(dm) = v.borrow().parse::<i32>() {
                    sampling_mechanism = Some(dm as u32);
                }
            }
            break;
        }
    }

    ChunkAttrs {
        trace_id,
        sampling_priority,
        origin,
        sampling_mechanism,
    }
}

/// Encodes all traces as a V1 msgpack payload.
///
/// Top-level format:
/// ```text
/// Map {
///   trace_key::ENV_REF      (7)  → str|uint       // optional, interned
///   trace_key::HOSTNAME_REF (8)  → str|uint       // optional, interned
///   trace_key::APP_VERSION  (9)  → str|uint       // optional, interned
///   trace_key::CHUNKS       (11) → Array[Chunk, ...]
/// }
/// ```
fn encode_payload<W: RmpWrite, T: TraceData, S: AsRef<[Span<T>]>>(
    writer: &mut W,
    traces: &[S],
) -> Result<(), ValueWriteError<W::Error>> {
    let mut table = StringTable::new();
    let payload_attrs = extract_payload_attrs(traces);

    let map_len = 1u32 // chunks always present
        + payload_attrs.env.is_some() as u32
        + payload_attrs.hostname.is_some() as u32
        + payload_attrs.app_version.is_some() as u32;

    write_map_len(writer, map_len)?;

    if let Some(env) = payload_attrs.env {
        write_uint8(writer, trace_key::ENV_REF)?;
        table.write_interned(writer, env)?;
    }

    if let Some(hostname) = payload_attrs.hostname {
        write_uint8(writer, trace_key::HOSTNAME_REF)?;
        table.write_interned(writer, hostname)?;
    }

    if let Some(app_version) = payload_attrs.app_version {
        write_uint8(writer, trace_key::APP_VERSION_REF)?;
        table.write_interned(writer, app_version)?;
    }

    write_uint8(writer, trace_key::CHUNKS)?;
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
///   chunk_key::TRACE_ID           (6) → bin[16]       // 128-bit big-endian
///   chunk_key::ORIGIN             (2) → str|uint       // optional, interned
///   chunk_key::PRIORITY           (1) → int            // optional
///   chunk_key::SAMPLING_MECHANISM (7) → uint           // optional
///   chunk_key::SPANS              (4) → Array[Span, ...]
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
        + attrs.sampling_priority.is_some() as u32
        + attrs.sampling_mechanism.is_some() as u32;

    write_map_len(writer, fields)?;

    write_uint8(writer, chunk_key::TRACE_ID)?;
    write_bin(writer, &attrs.trace_id.to_be_bytes())?;

    if let Some(origin) = attrs.origin {
        write_uint8(writer, chunk_key::ORIGIN)?;
        table.write_interned(writer, origin)?;
    }

    if let Some(priority) = attrs.sampling_priority {
        write_uint8(writer, chunk_key::PRIORITY)?;
        write_sint(writer, priority as i64)?;
    }

    if let Some(mechanism) = attrs.sampling_mechanism {
        write_uint8(writer, chunk_key::SAMPLING_MECHANISM)?;
        write_uint(writer, mechanism as u64)?;
    }

    write_uint8(writer, chunk_key::SPANS)?;
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
    // &mut &mut [u8] lets the caller see the slice shrink as bytes are written.
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
    let _ = encode_payload(&mut buf, traces);
    buf.into_vec()
}

/// Returns the number of bytes the V1 payload for `traces` would occupy.
pub fn to_encoded_byte_len<T: TraceData, S: AsRef<[Span<T>]>>(traces: &[S]) -> u32 {
    let mut counter = super::CountLength(0);
    let _ = encode_payload(&mut counter, traces);
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
    fn test_to_encoded_byte_len_matches_to_vec() {
        let spans = vec![
            make_span("svc", "op", 1, 1, 0),
            make_span("svc", "child", 1, 2, 1),
        ];
        let traces = vec![spans];
        let encoded = to_vec(&traces);
        let len = to_encoded_byte_len(&traces);
        assert_eq!(encoded.len() as u32, len);
    }

    #[test]
    fn test_remote_parent_root_span_top_level() {
        // A span with a non-zero parent_id but _dd.top_level=1.0 is a root in its chunk.
        let mut metrics = HashMap::new();
        metrics.insert(BytesString::from_static("_dd.top_level"), 1.0f64);
        metrics.insert(BytesString::from_static("_sampling_priority_v1"), 2.0f64);

        let root = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 123,
            span_id: 42,
            parent_id: 999, // remote parent — not in this chunk
            start: 1000,
            duration: 100,
            metrics,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![root]]);
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_payload_promoted_fields() {
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("env"),
            BytesString::from_static("prod"),
        );
        meta.insert(
            BytesString::from_static("version"),
            BytesString::from_static("1.2.3"),
        );
        meta.insert(
            BytesString::from_static("_dd.hostname"),
            BytesString::from_static("my-host"),
        );

        let span = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![span]]);
        let prod_bytes = b"prod";
        assert!(
            encoded.windows(prod_bytes.len()).any(|w| w == prod_bytes),
            "env 'prod' should appear in payload"
        );
        let host_bytes = b"my-host";
        assert!(
            encoded.windows(host_bytes.len()).any(|w| w == host_bytes),
            "hostname 'my-host' should appear in payload"
        );
    }
}
