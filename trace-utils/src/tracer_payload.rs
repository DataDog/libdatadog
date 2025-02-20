// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::{trace_utils, v05, SpanBytes};
use crate::{
    msgpack_decoder,
    trace_utils::{cmp_send_data_payloads, collect_trace_chunks, TracerHeaderTags},
};
use datadog_trace_protobuf::pb;
use std::cmp::Ordering;
use std::iter::Iterator;
use tinybytes;

pub type TracerPayloadV04 = Vec<SpanBytes>;
pub type TracerPayloadV05 = Vec<v05::Span>;

// Keys used for sampling
const SAMPLING_PRIORITY_KEY: &str = "_sampling_priority_v1";
const SAMPLING_SINGLE_SPAN_MECHANISM: &str = "_dd.span_sampling.mechanism";
const SAMPLING_ANALYTICS_RATE_KEY: &str = "_dd1.sr.eausr";

#[derive(Debug, Clone)]
/// Enumerates the different encoding types.
pub enum TraceEncoding {
    /// v0.4 encoding (TracerPayloadV04).
    V04,
    /// v054 encoding (TracerPayloadV04).
    V05,
    /// v0.7 encoding (TracerPayload).
    V07,
}

/// A collection of traces before they are turned into TraceChunks.
pub enum TraceCollection {
    V07(Vec<Vec<pb::Span>>),
    TraceChunk(Vec<Vec<SpanBytes>>),
}

impl TraceCollection {
    pub fn len(&self) -> usize {
        match self {
            TraceCollection::V07(traces) => traces.len(),
            TraceCollection::TraceChunk(traces) => traces.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            TraceCollection::V07(traces) => traces.is_empty(),
            TraceCollection::TraceChunk(traces) => traces.is_empty(),
        }
    }

    pub fn set_top_level_spans(&mut self) {
        match self {
            TraceCollection::TraceChunk(traces) => {
                for chunk in traces.iter_mut() {
                    trace_utils::compute_top_level_span(chunk);
                }
            }
            TraceCollection::V07(_) => todo!("set_top_level_spans not implemented for v07"),
        }
    }

    /// Remove spans and chunks only keeping the ones that may be sampled by the agent
    pub fn drop_chunks(&mut self) -> (usize, usize) {
        let mut dropped_p0_traces = 0;
        let mut dropped_p0_spans = 0;

        match self {
            TraceCollection::TraceChunk(traces) => {
                traces.retain_mut(|chunk| {
                    // List of spans to keep even if the chunk is dropped
                    let mut sampled_indexes = Vec::new();
                    for (index, span) in chunk.iter().enumerate() {
                        // ErrorSampler
                        if span.error == 1 {
                            // We send chunks containing an error
                            return true;
                        }
                        // PrioritySampler and NoPrioritySampler
                        let priority = span.metrics.get(SAMPLING_PRIORITY_KEY);
                        if trace_utils::has_top_level(span)
                            && (priority.is_none() || priority.is_some_and(|p| *p > 0.0))
                        {
                            // We send chunks with positive priority or no priority
                            return true;
                        }
                        // SingleSpanSampler and AnalyzedSpansSampler
                        else if span
                            .metrics
                            .get(SAMPLING_SINGLE_SPAN_MECHANISM)
                            .is_some_and(|m| *m == 8.0)
                            || span.metrics.contains_key(SAMPLING_ANALYTICS_RATE_KEY)
                        {
                            // We send spans sampled by single-span sampling or analyzed spans
                            sampled_indexes.push(index);
                        }
                    }
                    dropped_p0_spans += chunk.len() - sampled_indexes.len();
                    if sampled_indexes.is_empty() {
                        // If no spans were sampled we can drop the whole chunk
                        dropped_p0_traces += 1;
                        return false;
                    }
                    let sampled_spans = sampled_indexes
                        .iter()
                        .map(|i| std::mem::take(&mut chunk[*i]))
                        .collect();
                    *chunk = sampled_spans;
                    true
                });
            }
            TraceCollection::V07(_) => todo!("drop_chunks not implemented for v07"),
        }

        (dropped_p0_traces, dropped_p0_spans)
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
    /// use datadog_trace_protobuf::pb::TracerPayload;
    /// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
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
            TracerPayloadCollection::V05(_) => todo!("Append for V05 not implemented"),
        }
    }

    /// Merges traces that came from the same origin together to reduce the payload size.
    ///
    /// # Examples:
    ///
    /// ```rust
    /// use datadog_trace_protobuf::pb::TracerPayload;
    /// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
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
    /// use datadog_trace_protobuf::pb::TracerPayload;
    /// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
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
/// use datadog_trace_protobuf::pb::{Span, TraceChunk};
/// use datadog_trace_utils::tracer_payload::TraceChunkProcessor;
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
/// Represents the parameters required to collect trace chunks from msgpack data.
///
/// This struct encapsulates all the necessary parameters for converting msgpack data into
/// a `TracerPayloadCollection`. It is designed to work with the `TryInto` trait to facilitate
/// the conversion process, handling different encoding types and ensuring that all required
/// data is available for the conversion.
pub struct TracerPayloadParams<'a, T: TraceChunkProcessor + 'a> {
    /// A tinybytes::Bytes slice containing the serialized msgpack data.
    data: tinybytes::Bytes,
    /// Reference to `TracerHeaderTags` containing metadata for the trace.
    tracer_header_tags: &'a TracerHeaderTags<'a>,
    /// Amount of data consumed from buffer
    size: Option<&'a mut usize>,
    /// A mutable reference to an implementation of `TraceChunkProcessor` that processes each
    /// `TraceChunk` after it is constructed but before it is added to the TracerPayloadCollection.
    /// TraceChunks are only available for v07 traces.
    chunk_processor: &'a mut T,
    /// A boolean indicating whether the agent is running in an agentless mode. This is used to
    /// determine what protocol trace chunks should be serialized into when being sent.
    is_agentless: bool,
    /// The encoding type of the trace data, determining how the data should be deserialized and
    /// processed.
    encoding_type: TraceEncoding,
}

impl<'a, T: TraceChunkProcessor + 'a> TracerPayloadParams<'a, T> {
    pub fn new(
        data: tinybytes::Bytes,
        tracer_header_tags: &'a TracerHeaderTags,
        chunk_processor: &'a mut T,
        is_agentless: bool,
        encoding_type: TraceEncoding,
    ) -> TracerPayloadParams<'a, T> {
        TracerPayloadParams {
            data,
            tracer_header_tags,
            size: None,
            chunk_processor,
            is_agentless,
            encoding_type,
        }
    }

    pub fn measure_size(&mut self, size: &'a mut usize) {
        self.size = Some(size);
    }
}
// TODO: APMSP-1282 - Implement TryInto for other encoding types. Supporting TraceChunkProcessor but
// not supporting v07 is a bit pointless for now.
impl<'a, T: TraceChunkProcessor + 'a> TryInto<TracerPayloadCollection>
    for TracerPayloadParams<'a, T>
{
    type Error = anyhow::Error;
    /// Attempts to convert `TracerPayloadParams` into a `TracerPayloadCollection`.
    ///
    /// This method processes the msgpack data contained within `TracerPayloadParams` based on
    /// the specified `encoding_type`, converting it into a collection of tracer payloads.
    /// The conversion process involves deserializing the msgpack data, applying any necessary
    /// processing through `process_chunk`, and assembling the resulting data into
    /// a `TracerPayloadCollection`.
    ///
    /// Note: Currently only the `TraceEncoding::V04` encoding type is supported.
    ///
    /// # Returns
    ///
    /// A `Result` containing either the successfully converted `TracerPayloadCollection` or
    /// an error if the conversion fails. Possible errors include issues with deserializing the
    /// msgpack data or if the data does not conform to the expected format.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use datadog_trace_protobuf::pb;
    /// use datadog_trace_utils::trace_utils::TracerHeaderTags;
    /// use datadog_trace_utils::tracer_payload::{
    ///     DefaultTraceChunkProcessor, TraceEncoding, TracerPayloadCollection, TracerPayloadParams,
    /// };
    /// use std::convert::TryInto;
    /// use tinybytes;
    /// // This will likely be a &[u8] slice in practice.
    /// let data: Vec<u8> = Vec::new();
    /// let data_as_bytes = tinybytes::Bytes::from(data);
    /// let tracer_header_tags = &TracerHeaderTags::default();
    /// let result: Result<TracerPayloadCollection, _> = TracerPayloadParams::new(
    ///     data_as_bytes,
    ///     tracer_header_tags,
    ///     &mut DefaultTraceChunkProcessor,
    ///     false,
    ///     TraceEncoding::V04,
    /// )
    /// .try_into();
    ///
    /// match result {
    ///     Ok(collection) => println!("Successfully converted to TracerPayloadCollection."),
    ///     Err(e) => println!("Failed to convert: {:?}", e),
    /// }
    /// ```
    fn try_into(self) -> Result<TracerPayloadCollection, Self::Error> {
        match self.encoding_type {
            TraceEncoding::V04 => {
                let (traces, size) = match msgpack_decoder::v04::from_slice(self.data) {
                    Ok(res) => res,
                    Err(e) => {
                        anyhow::bail!("Error deserializing trace from request body: {e}")
                    }
                };

                if let Some(size_ref) = self.size {
                    *size_ref = size;
                }

                Ok(collect_trace_chunks(
                    TraceCollection::TraceChunk(traces),
                    self.tracer_header_tags,
                    self.chunk_processor,
                    self.is_agentless,
                    false,
                ))
            },
            TraceEncoding::V05 => {
                let (traces, size) = match msgpack_decoder::v05::from_slice(self.data) {
                    Ok(res) => res,
                    Err(e) => {
                        anyhow::bail!("Error deserializing trace from request body: {e}")
                    }
                };

                if let Some(size_ref) = self.size {
                    *size_ref = size;
                }

                Ok(collect_trace_chunks(
                    TraceCollection::TraceChunk(traces),
                    self.tracer_header_tags,
                    self.chunk_processor,
                    self.is_agentless,
                    true,
                ))
            },
            _ => todo!("Encodings other than TraceEncoding::V04 and TraceEncoding::V05 not implemented yet."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanBytes;
    use crate::test_utils::create_test_no_alloc_span;
    use datadog_trace_protobuf::pb;
    use serde_json::json;
    use std::collections::HashMap;
    use tinybytes::BytesString;

    const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";

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
        }];

        let data = rmp_serde::to_vec(&vec![span_data1, span_data2])
            .expect("Failed to serialize test span.");
        let data = tinybytes::Bytes::from(data);

        let tracer_header_tags = &TracerHeaderTags::default();

        let result: anyhow::Result<TracerPayloadCollection> = TracerPayloadParams::new(
            data,
            tracer_header_tags,
            &mut DefaultTraceChunkProcessor,
            false,
            TraceEncoding::V04,
        )
        .try_into();

        assert!(result.is_ok());

        let collection = result.unwrap();
        assert_eq!(2, collection.size());

        if let TracerPayloadCollection::V04(traces) = collection {
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

        let tracer_header_tags = &TracerHeaderTags::default();

        let result: anyhow::Result<TracerPayloadCollection> = TracerPayloadParams::new(
            data,
            tracer_header_tags,
            &mut DefaultTraceChunkProcessor,
            false,
            TraceEncoding::V04,
        )
        .try_into();

        assert!(result.is_ok());

        let collection = result.unwrap();
        assert_eq!(0, collection.size());
    }

    #[test]
    fn test_try_into_meta_metrics_success() {
        let dummy_trace = create_trace();
        let expected = vec![dummy_trace.clone()];
        let payload = rmp_serde::to_vec_named(&expected).unwrap();
        let payload = tinybytes::Bytes::from(payload);
        let tracer_header_tags = &TracerHeaderTags::default();

        let result: anyhow::Result<TracerPayloadCollection> = TracerPayloadParams::new(
            payload,
            tracer_header_tags,
            &mut DefaultTraceChunkProcessor,
            false,
            TraceEncoding::V04,
        )
        .try_into();

        assert!(result.is_ok());

        let collection = result.unwrap();
        assert_eq!(1, collection.size());
        if let TracerPayloadCollection::V04(traces) = collection {
            assert_eq!(dummy_trace, traces[0]);
        } else {
            panic!("Invalid collection type returned for try_into");
        }
    }

    #[test]
    fn test_drop_chunks() {
        let chunk_with_priority = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 1.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_null_priority = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_without_priority = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([(TRACER_TOP_LEVEL_KEY.into(), 1.0)]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_error = vec![
            SpanBytes {
                span_id: 1,
                error: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_a_single_span = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                metrics: HashMap::from([(SAMPLING_SINGLE_SPAN_MECHANISM.into(), 8.0)]),
                ..Default::default()
            },
        ];
        let chunk_with_analyzed_span = vec![
            SpanBytes {
                span_id: 1,
                metrics: HashMap::from([
                    (SAMPLING_PRIORITY_KEY.into(), 0.0),
                    (TRACER_TOP_LEVEL_KEY.into(), 1.0),
                ]),
                ..Default::default()
            },
            SpanBytes {
                span_id: 2,
                parent_id: 1,
                metrics: HashMap::from([(SAMPLING_ANALYTICS_RATE_KEY.into(), 1.0)]),
                ..Default::default()
            },
        ];

        let chunks_and_expected_sampled_spans = vec![
            (chunk_with_priority, 2),
            (chunk_with_null_priority, 0),
            (chunk_without_priority, 2),
            (chunk_with_error, 2),
            (chunk_with_a_single_span, 1),
            (chunk_with_analyzed_span, 1),
        ];

        for (chunk, expected_count) in chunks_and_expected_sampled_spans.into_iter() {
            let mut collection = TraceCollection::TraceChunk(vec![chunk]);
            collection.drop_chunks();

            let traces = match collection {
                TraceCollection::TraceChunk(t) => t,
                _ => panic!("Collection must contain TraceChunkSpan"),
            };

            if expected_count == 0 {
                assert!(traces.is_empty());
            } else {
                assert_eq!(traces[0].len(), expected_count);
            }
        }
    }
}
