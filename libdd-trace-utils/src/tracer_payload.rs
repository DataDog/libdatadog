// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v05::dict::SharedDict;
use crate::span::{v04, v05, v1, BytesData, SharedDictBytes, TraceData};
use crate::trace_utils::convert_trace_chunks_v04_to_v05;
use crate::{msgpack_decoder, trace_utils::cmp_send_data_payloads};
use anyhow::Ok;
use libdd_trace_protobuf::pb;
use std::cmp::Ordering;
use std::iter::Iterator;
use tracing::warn;

pub type TracerPayloadV04 = Vec<v04::SpanBytes>;
pub type TracerPayloadV05 = Vec<v05::Span>;

#[derive(Debug, Clone, Copy)]
/// Enumerates the different encoding types.
pub enum TraceEncoding {
    /// v0.4 encoding (TracerPayloadV04).
    V04,
    /// v0.5 encoding (TracerPayloadV05).
    V05,
    /// v1 encoding (TracerPayloadV1).
    V1,
}

#[derive(Debug)]
pub enum TraceChunks<T: TraceData> {
    /// Collection of TraceChunkSpan.
    V04(Vec<Vec<v04::Span<T>>>),
    /// Collection of TraceChunkSpan with de-duplicated strings.
    V05((SharedDict<T::Text>, Vec<Vec<v05::Span>>)),
    /// Collection of v0.4 spans to be serialized as a V1 msgpack payload.
    V1(Box<v1::TracerPayload<BytesData>>),
}

impl TraceChunks<BytesData> {
    pub fn into_tracer_payload_collection(self) -> TracerPayloadCollection {
        match self {
            TraceChunks::V04(traces) => TracerPayloadCollection::V04(traces),
            TraceChunks::V05(traces) => TracerPayloadCollection::V05(traces),
            TraceChunks::V1(traces) => TracerPayloadCollection::V1(traces),
        }
    }
}

impl<T: TraceData> TraceChunks<T> {
    /// Returns the number of traces in the chunk
    pub fn size(&self) -> usize {
        match self {
            TraceChunks::V04(traces) => traces.len(),
            TraceChunks::V05((_, traces)) => traces.len(),
            TraceChunks::V1(trace) => trace.chunks.len(),
        }
    }
}

#[derive(Debug)]
/// Enum representing a general abstraction for a collection of tracer payloads.
pub enum TracerPayloadCollection {
    /// Collection of TracerPayloads.
    V07(Vec<pb::TracerPayload>),
    /// Collection of TraceChunkSpan.
    V04(Vec<Vec<v04::SpanBytes>>),
    /// Collection of TraceChunkSpan with de-duplicated strings.
    V05((SharedDictBytes, Vec<Vec<v05::Span>>)),
    // /// V0.4-shaped spans that must be serialized as a V1 msgpack payload on send.
    V1(Box<v1::TracerPayload<BytesData>>),
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
    /// use libdd_trace_protobuf::pb::TracerPayload;
    /// use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
    /// let mut col1 = TracerPayloadCollection::V07(vec![TracerPayload::default()]);
    /// let mut col2 = TracerPayloadCollection::V07(vec![TracerPayload::default()]);
    /// col1.append(&mut col2);
    /// ```
    ///
    /// # Returns
    ///
    /// `true` if `other`'s data was merged into `self`, `false` if the append was skipped (e.g.
    /// diverging V1 tracer metadata). Callers that rely on `other` being fully drained must check
    /// this return value rather than assuming success.
    pub fn append(&mut self, other: &mut Self) -> bool {
        match self {
            TracerPayloadCollection::V07(dest) => {
                if let TracerPayloadCollection::V07(src) = other {
                    dest.append(src);
                    return true;
                }
                false
            }
            TracerPayloadCollection::V04(dest) => {
                if let TracerPayloadCollection::V04(src) = other {
                    dest.append(src);
                    return true;
                }
                false
            }
            TracerPayloadCollection::V1(dest) => {
                if let TracerPayloadCollection::V1(src) = other {
                    // Same-target SendData entries are coalesced by
                    // trace_utils::coalesce_send_data, so both V1 payloads
                    // typically share tracer-level metadata. If all metadata
                    // fields match we append `src`'s chunks into `dest`; if any diverge we no-op
                    // (logging a warning) rather than silently dropping `src`'s metadata.
                    if metadata_matches_v1(dest, src) {
                        dest.chunks.append(&mut src.chunks);
                        return true;
                    }
                }
                false
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
    /// use libdd_trace_protobuf::pb::TracerPayload;
    /// use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
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
    /// use libdd_trace_protobuf::pb::TracerPayload;
    /// use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
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
            TracerPayloadCollection::V1(collection) => collection.chunks.len(),
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
/// use libdd_trace_protobuf::pb::{Span, TraceChunk};
/// use libdd_trace_utils::tracer_payload::TraceChunkProcessor;
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
/// use libdd_tinybytes;
/// use libdd_trace_protobuf::pb;
/// use libdd_trace_utils::trace_utils::TracerHeaderTags;
/// use libdd_trace_utils::tracer_payload::{decode_to_trace_chunks, TraceEncoding};
/// use std::convert::TryInto;
/// // This will likely be a &[u8] slice in practice.
/// let data: Vec<u8> = Vec::new();
/// let data_as_bytes = libdd_tinybytes::Bytes::from(data);
/// let result = decode_to_trace_chunks(data_as_bytes, TraceEncoding::V04)
///     .map(|(chunks, _size)| chunks.into_tracer_payload_collection());
///
/// match result {
///     Ok(collection) => println!("Successfully converted to TracerPayloadCollection."),
///     Err(e) => println!("Failed to convert: {:?}", e),
/// }
/// ```
pub fn decode_to_trace_chunks(
    data: libdd_tinybytes::Bytes,
    encoding_type: TraceEncoding,
) -> Result<(TraceChunks<BytesData>, usize), anyhow::Error> {
    match encoding_type {
        TraceEncoding::V04 => {
            let (data, size) = msgpack_decoder::v04::from_bytes(data).map_err(|e| {
                anyhow::format_err!("Error deserializing trace from request body: {e}")
            })?;
            Ok((TraceChunks::V04(data), size))
        }
        TraceEncoding::V05 => {
            let (data, size) = msgpack_decoder::v05::from_bytes(data).map_err(|e| {
                anyhow::format_err!("Error deserializing trace from request body: {e}")
            })?;
            Ok((convert_trace_chunks_v04_to_v05(data)?, size))
        }
        TraceEncoding::V1 => {
            let (data, size) = msgpack_decoder::v1::from_bytes(data).map_err(|e| {
                anyhow::format_err!("Error deserializing trace from request body: {e}")
            })?;
            Ok((TraceChunks::V1(Box::new(data)), size))
        }
    }
}

/// Returns `true` iff every tracer-level metadata field (string fields and attributes) of `src`
/// matches `dest`.
///
/// V1 payloads carry tracer metadata (env, hostname, language, …) inside the payload itself, so
/// merging two payloads whose metadata diverges would silently drop one set of values. Callers
/// use this to gate the merge: on a `false` return, append is skipped (no-op) and the two
/// payloads stay separate. A warning is logged listing the diverging fields so the situation
/// is observable rather than silent.
fn metadata_matches_v1(
    dest: &v1::TracerPayload<BytesData>,
    src: &v1::TracerPayload<BytesData>,
) -> bool {
    let differing: Vec<&'static str> = [
        ("container_id", dest.container_id == src.container_id),
        ("language_name", dest.language_name == src.language_name),
        (
            "language_version",
            dest.language_version == src.language_version,
        ),
        ("tracer_version", dest.tracer_version == src.tracer_version),
        ("runtime_id", dest.runtime_id == src.runtime_id),
        ("env", dest.env == src.env),
        ("hostname", dest.hostname == src.hostname),
        ("app_version", dest.app_version == src.app_version),
        ("attributes", dest.attributes == src.attributes),
    ]
    .into_iter()
    .filter_map(|(label, eq)| (!eq).then_some(label))
    .collect();

    if !differing.is_empty() {
        warn!(
            "Skipping V1 TracerPayload append: diverging metadata fields {:?}",
            differing
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::{SpanBytes, VecMap};
    use crate::test_utils::create_test_no_alloc_span;
    use libdd_tinybytes::BytesString;
    use libdd_trace_protobuf::pb;
    use serde_json::json;

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
            container_debug: None,
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
        let mut two_traces = create_dummy_collection_v07();
        two_traces.append(&mut create_dummy_collection_v07());

        let mut trace = create_dummy_collection_v07();

        let mut empty = TracerPayloadCollection::V07(vec![]);

        trace.append(&mut create_dummy_collection_v07());
        assert_eq!(2, trace.size());

        trace.append(&mut two_traces);
        assert_eq!(4, trace.size());

        trace.append(&mut empty);
        assert_eq!(4, trace.size());
    }

    #[test]
    fn test_append_traces_v04() {
        fn create_trace() -> TracerPayloadCollection {
            TracerPayloadCollection::V04(vec![vec![create_test_no_alloc_span(0, 1, 0, 2, true)]])
        }

        let mut two_traces = create_trace();
        two_traces.append(&mut create_trace());

        let mut trace = create_trace();

        let mut empty = TracerPayloadCollection::V04(vec![]);

        trace.append(&mut create_trace());
        assert_eq!(2, trace.size());

        trace.append(&mut two_traces);
        assert_eq!(4, trace.size());

        trace.append(&mut empty);
        assert_eq!(4, trace.size());
    }

    #[test]
    fn test_merge_traces() {
        let mut trace = create_dummy_collection_v07();

        trace.append(&mut create_dummy_collection_v07());
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
            meta: VecMap::new(),
            metrics: VecMap::new(),
            meta_struct: VecMap::new(),
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
            meta: VecMap::new(),
            metrics: VecMap::new(),
            meta_struct: VecMap::new(),
            r#type: BytesString::default(),
            span_links: vec![],
            span_events: vec![],
        }];

        let data = rmp_serde::to_vec(&vec![span_data1, span_data2])
            .expect("Failed to serialize test span.");
        let data = libdd_tinybytes::Bytes::from(data);

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
        let data = libdd_tinybytes::Bytes::from(empty_data);

        let result = decode_to_trace_chunks(data, TraceEncoding::V04);

        assert!(result.is_ok());

        let (collection, _) = result.unwrap();
        assert_eq!(0, collection.size());
    }

    #[test]
    fn test_try_into_meta_metrics_success() {
        let dummy_trace = create_trace();
        let expected = vec![create_trace()];
        let payload = rmp_serde::to_vec_named(&expected).unwrap();
        let payload = libdd_tinybytes::Bytes::from(payload);

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
