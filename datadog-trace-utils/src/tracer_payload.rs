// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::{v05, Span, SpanBytes, SpanText};
use crate::trace_utils::collect_trace_chunks;
use crate::{msgpack_decoder, trace_utils::cmp_send_data_payloads};
use libdd_trace_protobuf::pb;
use std::cmp::Ordering;
use std::iter::Iterator;
use tinybytes::{self, BytesString};

pub type TracerPayloadV04 = Vec<SpanBytes>;
pub type TracerPayloadV05 = Vec<v05::Span>;

#[derive(Debug, Clone)]
/// Enumerates the different encoding types.
pub enum TraceEncoding {
    /// v0.4 encoding (TracerPayloadV04).
    V04,
    /// v054 encoding (TracerPayloadV04).
    V05,
}

#[derive(Debug, Clone)]
pub enum TraceChunks<T: SpanText> {
    /// Collection of TraceChunkSpan.
    V04(Vec<Vec<Span<T>>>),
    /// Collection of TraceChunkSpan with de-duplicated strings.
    V05((Vec<T>, Vec<Vec<v05::Span>>)),
}

impl TraceChunks<BytesString> {
    pub fn into_tracer_payload_collection(self) -> TracerPayloadCollection {
        match self {
            TraceChunks::V04(traces) => TracerPayloadCollection::V04(traces),
            TraceChunks::V05(traces) => TracerPayloadCollection::V05(traces),
        }
    }
}

impl<T: SpanText> TraceChunks<T> {
    /// Returns the number of traces in the chunk
    pub fn size(&self) -> usize {
        match self {
            TraceChunks::V04(traces) => traces.len(),
            TraceChunks::V05((_, traces)) => traces.len(),
        }
    }
}

#[derive(Debug, Clone)]
/// Enum representing a general abstraction for a collection of tracer payloads.
pub enum TracerPayloadCollection {
    /// Collection of TracerPayloads.
    V07(Vec<pb::TracerPayload>),
    /// Collection of TraceChunkSpan.
    V04(Vec<Vec<SpanBytes>>),
    /// Collection of TraceChunkSpan with de-duplicated strings.
    V05((Vec<tinybytes::BytesString>, Vec<Vec<v05::Span>>)),
}

impl TracerPayloadCollection {
    /// Appends `other` collection of the same type to the current collection.
    ///
    /// #Arguments
    ///
    /// * `other`: collection of the same type.
    ///
    /// # Examples:
    ///
    /// ```rust
    /// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
    /// use libdd_trace_protobuf::pb::TracerPayload;
    /// let mut col1 = TracerPayloadCollection::V07(vec![TracerPayload::default()]);
    /// let mut col2 = TracerPayloadCollection::V07(vec![TracerPayload::default()]);
    /// col1.append(&mut col2);
    /// ```
    pub fn append(&mut self, other: &mut Self) {
        match self {
            TracerPayloadCollection::V07(dest) => {
                if let TracerPayloadCollection::V07(src) = other {
                    dest.append(src)
                }
            }
            TracerPayloadCollection::V04(dest) => {
                if let TracerPayloadCollection::V04(src) = other {
                    dest.append(src)
                }
            }
            // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
            #[allow(clippy::unimplemented)]
            TracerPayloadCollection::V05(_) => unimplemented!("Append for V05 not implemented"),
        }
    }

    /// Merges traces that came from the same origin together to reduce the payload size.
    ///
    /// # Examples:
    ///
    /// ```rust
    /// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
    /// use libdd_trace_protobuf::pb::TracerPayload;
    /// let mut col1 =
    ///     TracerPayloadCollection::V07(vec![TracerPayload::default(), TracerPayload::default()]);
    /// col1.merge();
    /// ```
    pub fn merge(&mut self) {
        if let TracerPayloadCollection::V07(collection) = self {
            collection.sort_unstable_by(cmp_send_data_payloads);
            collection.dedup_by(|a, b| {
                if cmp_send_data_payloads(a, b) == Ordering::Equal {
                    // Note: dedup_by drops a, and retains b.
                    b.chunks.append(&mut a.chunks);
                    return true;
                }
                false
            })
        }
    }

    /// Computes the size of the collection.
    ///
    /// # Returns
    ///
    /// The number of traces contained in the collection.
    ///
    /// # Examples:
    ///
    /// ```rust
    /// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
    /// use libdd_trace_protobuf::pb::TracerPayload;
    /// let col1 = TracerPayloadCollection::V07(vec![TracerPayload::default()]);
    /// col1.size();
    /// ```
    pub fn size(&self) -> usize {
        match self {
            TracerPayloadCollection::V07(collection) => {
                collection.iter().map(|s| s.chunks.len()).sum()
            }
            TracerPayloadCollection::V04(collection) => collection.len(),
            TracerPayloadCollection::V05((_, collection)) => collection.len(),
        }
    }
}

/// A trait defining custom processing to be applied to `TraceChunks`.
///
/// TraceChunks are part of the v07 Trace payloads. Implementors of this trait can define specific
/// logic to modify or enrich trace chunks and pass it to the `TracerPayloadCollection` via
/// `TracerPayloadParams`.
///
/// # Examples
///
/// Implementing `TraceChunkProcessor` to add a custom tag to each span in a chunk:
///
/// ```rust
/// use datadog_trace_utils::tracer_payload::TraceChunkProcessor;
/// use libdd_trace_protobuf::pb::{Span, TraceChunk};
/// use std::collections::HashMap;
///
/// struct CustomTagProcessor {
///     tag_key: String,
///     tag_value: String,
/// }
///
/// impl TraceChunkProcessor for CustomTagProcessor {
///     fn process(&mut self, chunk: &mut TraceChunk, index: usize) {
///         for span in &mut chunk.spans {
///             span.meta
///                 .insert(self.tag_key.clone(), self.tag_value.clone());
///         }
///     }
/// }
/// ```
pub trait TraceChunkProcessor {
    fn process(&mut self, chunk: &mut pb::TraceChunk, index: usize);
}

#[derive(Default)]
/// Default implementation of `TraceChunkProcessor` that does nothing.
///
/// If used, the compiler should optimize away calls to it.
pub struct DefaultTraceChunkProcessor;

impl TraceChunkProcessor for DefaultTraceChunkProcessor {
    fn process(&mut self, _chunk: &mut pb::TraceChunk, _index: usize) {
        // Default implementation does nothing.
    }
}

/// This method processes the msgpack data contained within `data` based on
/// the specified `encoding_type`, converting it into a collection of tracer payloads.
///
/// Note: Currently only the `TraceEncoding::V04` and `TraceEncoding::V05` encoding types are
/// supported.
///
/// # Returns
///
/// A `Result` containing either the successfully converted `TraceChunks` and the length consummed
/// from the data  or an error if the conversion fails. Possible errors include issues with
/// deserializing the msgpack data or if the data does not conform to the expected format.
///
/// # Examples
///
/// ```rust
/// use datadog_trace_utils::trace_utils::TracerHeaderTags;
/// use datadog_trace_utils::tracer_payload::{decode_to_trace_chunks, TraceEncoding};
/// use libdd_trace_protobuf::pb;
/// use std::convert::TryInto;
/// use tinybytes;
/// // This will likely be a &[u8] slice in practice.
/// let data: Vec<u8> = Vec::new();
/// let data_as_bytes = tinybytes::Bytes::from(data);
/// let result = decode_to_trace_chunks(data_as_bytes, TraceEncoding::V04)
///     .map(|(chunks, _size)| chunks.into_tracer_payload_collection());
///
/// match result {
///     Ok(collection) => println!("Successfully converted to TracerPayloadCollection."),
///     Err(e) => println!("Failed to convert: {:?}", e),
/// }
/// ```
pub fn decode_to_trace_chunks(
    data: tinybytes::Bytes,
    encoding_type: TraceEncoding,
) -> Result<(TraceChunks<BytesString>, usize), anyhow::Error> {
    let (data, size) = match encoding_type {
        TraceEncoding::V04 => msgpack_decoder::v04::from_bytes(data),
        TraceEncoding::V05 => msgpack_decoder::v05::from_bytes(data),
    }
    .map_err(|e| anyhow::format_err!("Error deserializing trace from request body: {e}"))?;

    Ok((
        collect_trace_chunks(data, matches!(encoding_type, TraceEncoding::V05))?,
        size,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanBytes;
    use crate::test_utils::create_test_no_alloc_span;
    use libdd_trace_protobuf::pb;
    use serde_json::json;
    use std::collections::HashMap;
    use tinybytes::BytesString;

    fn create_dummy_collection_v07() -> TracerPayloadCollection {
        TracerPayloadCollection::V07(vec![pb::TracerPayload {
            container_id: "".to_string(),
            language_name: "".to_string(),
            language_version: "".to_string(),
            tracer_version: "".to_string(),
            runtime_id: "".to_string(),
            chunks: vec![pb::TraceChunk {
                priority: 0,
                origin: "".to_string(),
                spans: vec![],
                tags: Default::default(),
                dropped_trace: false,
            }],
            tags: Default::default(),
            env: "".to_string(),
            hostname: "".to_string(),
            app_version: "".to_string(),
        }])
    }

    fn create_trace() -> Vec<SpanBytes> {
        vec![
            // create a root span with metrics
            create_test_no_alloc_span(1234, 12341, 0, 1, true),
            create_test_no_alloc_span(1234, 12342, 12341, 1, false),
            create_test_no_alloc_span(1234, 12343, 12342, 1, false),
        ]
    }

    #[test]
    fn test_append_traces_v07() {
        let mut trace = create_dummy_collection_v07();

        let empty = TracerPayloadCollection::V07(vec![]);

        trace.append(&mut trace.clone());
        assert_eq!(2, trace.size());

        trace.append(&mut trace.clone());
        assert_eq!(4, trace.size());

        trace.append(&mut empty.clone());
        assert_eq!(4, trace.size());
    }

    #[test]
    fn test_append_traces_v04() {
        let mut trace =
            TracerPayloadCollection::V04(vec![vec![create_test_no_alloc_span(0, 1, 0, 2, true)]]);

        let empty = TracerPayloadCollection::V04(vec![]);

        trace.append(&mut trace.clone());
        assert_eq!(2, trace.size());

        trace.append(&mut trace.clone());
        assert_eq!(4, trace.size());

        trace.append(&mut empty.clone());
        assert_eq!(4, trace.size());
    }

    #[test]
    fn test_merge_traces() {
        let mut trace = create_dummy_collection_v07();

        trace.append(&mut trace.clone());
        assert_eq!(2, trace.size());

        trace.merge();
        assert_eq!(2, trace.size());
        if let TracerPayloadCollection::V07(collection) = trace {
            assert_eq!(1, collection.len());
        } else {
            panic!("Unexpected type");
        }
    }

    #[test]
    fn test_try_into_success() {
        let span_data1 = json!([{
            "service": "test-service",
            "name": "test-service-name",
            "resource": "test-service-resource",
            "trace_id": 111,
            "span_id": 222,
            "parent_id": 100,
            "start": 1,
            "duration": 5,
            "error": 0,
            "meta": {},
            "metrics": {},
            "type": "serverless",
        }]);

        let expected_serialized_span_data1 = vec![SpanBytes {
            service: BytesString::from_slice("test-service".as_ref()).unwrap(),
            name: BytesString::from_slice("test-service-name".as_ref()).unwrap(),
            resource: BytesString::from_slice("test-service-resource".as_ref()).unwrap(),
            trace_id: 111,
            span_id: 222,
            parent_id: 100,
            start: 1,
            duration: 5,
            error: 0,
            meta: HashMap::new(),
            metrics: HashMap::new(),
            meta_struct: HashMap::new(),
            r#type: BytesString::from_slice("serverless".as_ref()).unwrap(),
            span_links: vec![],
            span_events: vec![],
        }];

        let span_data2 = json!([{
            "service": "test-service",
            "name": "test-service-name",
            "resource": "test-service-resource",
            "trace_id": 111,
            "span_id": 333,
            "parent_id": 100,
            "start": 1,
            "duration": 5,
            "error": 1,
            "meta": {},
            "metrics": {},
            "type": "",
        }]);

        let expected_serialized_span_data2 = vec![SpanBytes {
            service: BytesString::from_slice("test-service".as_ref()).unwrap(),
            name: BytesString::from_slice("test-service-name".as_ref()).unwrap(),
            resource: BytesString::from_slice("test-service-resource".as_ref()).unwrap(),
            trace_id: 111,
            span_id: 333,
            parent_id: 100,
            start: 1,
            duration: 5,
            error: 1,
            meta: HashMap::new(),
            metrics: HashMap::new(),
            meta_struct: HashMap::new(),
            r#type: BytesString::default(),
            span_links: vec![],
            span_events: vec![],
        }];

        let data = rmp_serde::to_vec(&vec![span_data1, span_data2])
            .expect("Failed to serialize test span.");
        let data = tinybytes::Bytes::from(data);

        let result = decode_to_trace_chunks(data, TraceEncoding::V04);

        assert!(result.is_ok());

        let (chunks, _) = result.unwrap();
        assert_eq!(2, chunks.size());

        if let TraceChunks::V04(traces) = chunks {
            assert_eq!(expected_serialized_span_data1, traces[0]);
            assert_eq!(expected_serialized_span_data2, traces[1]);
        } else {
            panic!("Invalid collection type returned for try_into");
        }
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_try_into_empty() {
        let empty_data = vec![0x90];
        let data = tinybytes::Bytes::from(empty_data);

        let result = decode_to_trace_chunks(data, TraceEncoding::V04);

        assert!(result.is_ok());

        let (collection, _) = result.unwrap();
        assert_eq!(0, collection.size());
    }

    #[test]
    fn test_try_into_meta_metrics_success() {
        let dummy_trace = create_trace();
        let expected = vec![dummy_trace.clone()];
        let payload = rmp_serde::to_vec_named(&expected).unwrap();
        let payload = tinybytes::Bytes::from(payload);

        let result = decode_to_trace_chunks(payload, TraceEncoding::V04);

        assert!(result.is_ok());

        let (collection, _size) = result.unwrap();
        assert_eq!(1, collection.size());
        if let TraceChunks::V04(traces) = collection {
            assert_eq!(dummy_trace, traces[0]);
        } else {
            panic!("Invalid collection type returned for try_into");
        }
    }
}
