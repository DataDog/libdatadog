// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::trace_exporter::agent_response::{
    AgentResponsePayloadVersion, DATADOG_RATES_PAYLOAD_VERSION,
};
use crate::trace_exporter::error::TraceExporterError;
use crate::trace_exporter::TraceExporterOutputFormat;
use http::{header::CONTENT_TYPE, HeaderMap, HeaderValue};
use libdd_common::header::{
    APPLICATION_MSGPACK, DATADOG_SEND_REAL_HTTP_STATUS, DATADOG_TRACE_COUNT,
};
use libdd_trace_utils::msgpack_decoder::decode::error::DecodeError;
use libdd_trace_utils::msgpack_encoder;
use libdd_trace_utils::span::{v04::Span, TraceData};
use libdd_trace_utils::trace_utils::{self, TracerHeaderTags};
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use libdd_trace_utils::tracer_payload::{self};

/// Minimal capacity of fresh buffers allocated to encode traces, in bytes.
const MIN_BUFFER_CAPACITY: usize = 1024;

/// Prepared traces payload ready for sending to the agent
pub(super) struct PreparedTracesPayload {
    /// Serialized msgpack payload
    pub data: Vec<u8>,
    /// HTTP headers for the request
    pub headers: HeaderMap,
    /// Number of trace chunks
    pub chunk_count: usize,
}

/// Trace serialization client for handling payload preparation
#[derive(Debug)]
pub(super) struct TraceSerializer {
    previous_serialised_len: AtomicUsize,
}

impl TraceSerializer {
    /// Create a new trace serializer
    pub(super) fn new() -> Self {
        Self {
            previous_serialised_len: AtomicUsize::new(MIN_BUFFER_CAPACITY),
        }
    }

    /// Prepare traces payload and HTTP headers for sending to agent
    pub(super) fn prepare_traces_payload<T: TraceData>(
        &self,
        traces: Vec<Vec<Span<T>>>,
        header_tags: TracerHeaderTags,
        metadata: &TracerMetadata,
        agent_payload_response_version: Option<&AgentResponsePayloadVersion>,
        output_format: TraceExporterOutputFormat,
    ) -> Result<PreparedTracesPayload, TraceExporterError> {
        let payload = self.collect_and_process_traces(traces, output_format)?;
        let chunks = payload.size();
        let headers =
            self.build_traces_headers(header_tags, chunks, agent_payload_response_version);
        let mp_payload = self.serialize_payload(&payload, metadata, output_format)?;

        Ok(PreparedTracesPayload {
            data: mp_payload,
            headers,
            chunk_count: chunks,
        })
    }

    /// Collect trace chunks based on output format
    fn collect_and_process_traces<T: TraceData>(
        &self,
        traces: Vec<Vec<Span<T>>>,
        output_format: TraceExporterOutputFormat,
    ) -> Result<tracer_payload::TraceChunks<T>, TraceExporterError> {
        let map_err = |e: anyhow::Error| {
            TraceExporterError::Deserialization(DecodeError::InvalidFormat(e.to_string()))
        };
        match output_format {
            // v0.4 input spans are kept as-is in `TraceChunks::V04`. Whether they go out as v0.4
            // or are cross-encoded into V1 on the wire is decided in `serialize_payload`.
            //
            // APMSP-2812 - TODO: when the data-pipeline gains a V1-native input model (its own
            // `v1::Span`-shaped builder), route `OutputFormat::V1` to
            // `TraceChunks::V1(v1::TracerPayload)` instead and serialize via
            // `to_vec_from_payload_v1`. A `StatSpan` impl on `v1::Span<T>` will also be needed
            // if client-side stats are enabled on the V1-native path.
            TraceExporterOutputFormat::V04 | TraceExporterOutputFormat::V1 => {
                Ok(tracer_payload::TraceChunks::V04(traces))
            }
            TraceExporterOutputFormat::V05 => {
                trace_utils::convert_trace_chunks_v04_to_v05(traces).map_err(map_err)
            }
        }
    }

    /// Build HTTP headers for traces request
    fn build_traces_headers(
        &self,
        header_tags: TracerHeaderTags,
        chunk_count: usize,
        agent_payload_response_version: Option<&AgentResponsePayloadVersion>,
    ) -> HeaderMap {
        let mut headers: HeaderMap = header_tags.into();
        headers.reserve(4);
        headers.insert(DATADOG_SEND_REAL_HTTP_STATUS, HeaderValue::from_static("1"));
        headers.insert(DATADOG_TRACE_COUNT, chunk_count.into());
        headers.insert(CONTENT_TYPE, APPLICATION_MSGPACK);
        if let Some(agent_payload_response_version) = agent_payload_response_version {
            // should never fail, as the version should only contain visible ascii chars
            let _ = HeaderValue::try_from(agent_payload_response_version.header_value())
                .map(|v| headers.insert(DATADOG_RATES_PAYLOAD_VERSION, v));
        }
        headers
    }

    /// Serialize payload to msgpack format
    fn serialize_payload<T: TraceData>(
        &self,
        payload: &tracer_payload::TraceChunks<T>,
        metadata: &TracerMetadata,
        output_format: TraceExporterOutputFormat,
    ) -> Result<Vec<u8>, TraceExporterError> {
        let capacity = self
            .previous_serialised_len
            .load(Ordering::Relaxed)
            .max(MIN_BUFFER_CAPACITY);
        let buff = match (payload, output_format) {
            (tracer_payload::TraceChunks::V04(p), TraceExporterOutputFormat::V04) => {
                msgpack_encoder::v04::to_vec_with_capacity(p, capacity as u32)
            }
            // v0.4 spans cross-encoded as V1 on the wire — used when the agent advertises
            // /v1.0/traces. Same in-memory shape as the v0.4 native path, different encoder.
            (tracer_payload::TraceChunks::V04(p), TraceExporterOutputFormat::V1) => {
                msgpack_encoder::v1::to_vec_with_capacity(p, capacity as u32, metadata)
            }
            (tracer_payload::TraceChunks::V05(p), TraceExporterOutputFormat::V05) => {
                let mut buff = Vec::with_capacity(capacity);
                rmp_serde::encode::write(&mut buff, p)
                    .map_err(TraceExporterError::Serialization)?;
                buff
            }
            // APMSP-2812 - TODO: native V1 input — call `msgpack_encoder::v1::to_vec_from_payload_v1`
            // on the carried `v1::TracerPayload`. Not yet reachable: `collect_and_process_traces`
            // never produces `TraceChunks::V1` in the current data-pipeline path.
            (tracer_payload::TraceChunks::V1(_), TraceExporterOutputFormat::V1) => {
                todo!("Native V1 input serialization not yet implemented (APMSP-2812)")
            }
            // `collect_and_process_traces` only produces (V04, V04|V1), (V05, V05),
            // or (V1, V1) — any other combination here is a programming error.
            _ => {
                return Err(TraceExporterError::Deserialization(
                    DecodeError::InvalidFormat(
                        "Unsupported (TraceChunks, OutputFormat) combination for serialization"
                            .to_owned(),
                    ),
                ));
            }
        };
        self.previous_serialised_len
            .store(buff.len(), Ordering::Relaxed);
        Ok(buff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::agent_response::AgentResponsePayloadVersion;
    use http::header::CONTENT_TYPE;
    use libdd_common::header::APPLICATION_MSGPACK_STR;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::span::v04::SpanBytes;
    use libdd_trace_utils::trace_utils::TracerHeaderTags;

    fn create_test_span() -> SpanBytes {
        SpanBytes {
            name: BytesString::from_slice(b"test_span").unwrap(),
            service: BytesString::from_slice(b"test_service").unwrap(),
            resource: BytesString::from_slice(b"test_resource").unwrap(),
            r#type: BytesString::from_slice(b"http").unwrap(),
            start: 1234567890,
            duration: 1000,
            span_id: 123,
            trace_id: 456,
            parent_id: 789,
            error: 0,
            ..Default::default()
        }
    }

    fn create_test_header_tags() -> TracerHeaderTags<'static> {
        TracerHeaderTags {
            lang: "rust",
            lang_version: "1.70.0",
            tracer_version: "1.0.0",
            lang_interpreter: "rustc",
            lang_vendor: "rust-lang",
            client_computed_stats: true,
            client_computed_top_level: false,
            ..Default::default()
        }
    }

    #[test]
    fn test_trace_serializer_new() {
        let serializer = TraceSerializer::new();
        assert_eq!(
            serializer.previous_serialised_len.load(Ordering::Relaxed),
            MIN_BUFFER_CAPACITY
        );
    }

    #[test]
    fn test_build_traces_headers() {
        let serializer = TraceSerializer::new();
        let header_tags = create_test_header_tags();
        let headers = serializer.build_traces_headers(header_tags, 3, None);

        // Check basic headers are present
        assert_eq!(headers.get(DATADOG_SEND_REAL_HTTP_STATUS).unwrap(), "1");
        assert_eq!(headers.get(DATADOG_TRACE_COUNT).unwrap(), "3");
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), APPLICATION_MSGPACK_STR);

        // Check tracer metadata headers are present
        assert_eq!(headers.get("datadog-meta-lang").unwrap(), "rust");
        assert_eq!(headers.get("datadog-meta-lang-version").unwrap(), "1.70.0");
        assert_eq!(headers.get("datadog-meta-tracer-version").unwrap(), "1.0.0");
        assert_eq!(
            headers.get("datadog-meta-lang-interpreter").unwrap(),
            "rustc"
        );
        assert_eq!(
            headers.get("datadog-meta-lang-interpreter-vendor").unwrap(),
            "rust-lang"
        );

        // Check computed stats headers
        assert!(headers.contains_key("datadog-client-computed-stats"));
        assert!(!headers.contains_key("datadog-client-computed-top-level"));
    }

    #[test]
    fn test_build_traces_headers_with_agent_version() {
        let agent_version = AgentResponsePayloadVersion::new();
        let serializer = TraceSerializer::new();
        let header_tags = create_test_header_tags();
        let headers = serializer.build_traces_headers(header_tags, 2, Some(&agent_version));

        // Check that agent payload version header is included
        assert!(headers.contains_key(DATADOG_RATES_PAYLOAD_VERSION));
        assert_eq!(headers.get(DATADOG_TRACE_COUNT).unwrap(), "2");
    }

    #[test]
    fn test_collect_and_process_traces_v04() {
        let serializer = TraceSerializer::new();
        let traces = vec![vec![create_test_span()]];

        let result = serializer.collect_and_process_traces(traces, TraceExporterOutputFormat::V04);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(matches!(payload, tracer_payload::TraceChunks::V04(_)));
        assert_eq!(payload.size(), 1);
    }

    #[test]
    fn test_collect_and_process_traces_v05() {
        let serializer = TraceSerializer::new();
        let traces = vec![vec![create_test_span()]];

        let result = serializer.collect_and_process_traces(traces, TraceExporterOutputFormat::V05);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(matches!(payload, tracer_payload::TraceChunks::V05(_)));
        assert_eq!(payload.size(), 1);
    }

    #[test]
    fn test_collect_and_process_traces_multiple_chunks() {
        let serializer = TraceSerializer::new();
        let traces = vec![
            vec![create_test_span()],
            vec![create_test_span(), create_test_span()],
            vec![create_test_span()],
        ];

        let result = serializer.collect_and_process_traces(traces, TraceExporterOutputFormat::V04);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.size(), 3);
    }

    #[test]
    fn test_serialize_payload_v04() {
        let serializer = TraceSerializer::new();
        let original_traces = vec![vec![create_test_span()]];
        let payload = serializer
            .collect_and_process_traces(original_traces.clone(), TraceExporterOutputFormat::V04)
            .unwrap();

        let result = serializer.serialize_payload(
            &payload,
            &TracerMetadata::default(),
            TraceExporterOutputFormat::V04,
        );
        assert!(result.is_ok());

        let serialized = result.unwrap();
        assert!(!serialized.is_empty());

        // Verify we can deserialize it back and data integrity is preserved
        let (deserialized_traces, _) =
            libdd_trace_utils::msgpack_decoder::v04::from_slice(&serialized).unwrap();
        assert_eq!(deserialized_traces.len(), 1);
        assert_eq!(deserialized_traces[0].len(), 1);

        let original_span = &original_traces[0][0];
        let deserialized_span = &deserialized_traces[0][0];

        assert_eq!(original_span.name, deserialized_span.name);
        assert_eq!(original_span.service, deserialized_span.service);
        assert_eq!(original_span.resource, deserialized_span.resource);
        assert_eq!(original_span.r#type, deserialized_span.r#type);
        assert_eq!(original_span.start, deserialized_span.start);
        assert_eq!(original_span.duration, deserialized_span.duration);
        assert_eq!(original_span.span_id, deserialized_span.span_id);
        assert_eq!(original_span.trace_id, deserialized_span.trace_id);
        assert_eq!(original_span.parent_id, deserialized_span.parent_id);
        assert_eq!(original_span.error, deserialized_span.error);
    }

    #[test]
    fn test_serialize_payload_v05() {
        let serializer = TraceSerializer::new();
        let original_traces = vec![vec![create_test_span()]];
        let payload = serializer
            .collect_and_process_traces(original_traces.clone(), TraceExporterOutputFormat::V05)
            .unwrap();

        let result = serializer.serialize_payload(
            &payload,
            &TracerMetadata::default(),
            TraceExporterOutputFormat::V05,
        );
        assert!(result.is_ok());

        let serialized = result.unwrap();
        assert!(!serialized.is_empty());

        // Verify we can deserialize it back and data integrity is preserved
        let (deserialized_traces, _) =
            libdd_trace_utils::msgpack_decoder::v05::from_slice(&serialized).unwrap();
        assert_eq!(deserialized_traces.len(), 1);
        assert_eq!(deserialized_traces[0].len(), 1);

        let original_span = &original_traces[0][0];
        let deserialized_span = &deserialized_traces[0][0];

        assert_eq!(original_span.name, deserialized_span.name);
        assert_eq!(original_span.service, deserialized_span.service);
        assert_eq!(original_span.resource, deserialized_span.resource);
        assert_eq!(original_span.r#type, deserialized_span.r#type);
        assert_eq!(original_span.start, deserialized_span.start);
        assert_eq!(original_span.duration, deserialized_span.duration);
        assert_eq!(original_span.span_id, deserialized_span.span_id);
        assert_eq!(original_span.trace_id, deserialized_span.trace_id);
        assert_eq!(original_span.parent_id, deserialized_span.parent_id);
        assert_eq!(original_span.error, deserialized_span.error);
    }

    #[test]
    fn test_prepare_traces_payload_v04() {
        let serializer = TraceSerializer::new();
        let traces = vec![
            vec![create_test_span()],
            vec![create_test_span(), create_test_span()],
        ];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(
            traces,
            header_tags,
            &TracerMetadata::default(),
            None,
            TraceExporterOutputFormat::V04,
        );
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 2);
        assert!(!prepared.data.is_empty());
        assert!(!prepared.headers.is_empty());

        // Check headers
        assert_eq!(prepared.headers.get(DATADOG_TRACE_COUNT).unwrap(), "2");
        assert_eq!(prepared.headers.get("datadog-meta-lang").unwrap(), "rust");
    }

    #[test]
    fn test_prepare_traces_payload_v05() {
        let serializer = TraceSerializer::new();
        let traces = vec![vec![create_test_span()]];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(
            traces,
            header_tags,
            &TracerMetadata::default(),
            None,
            TraceExporterOutputFormat::V05,
        );
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 1);
        assert!(!prepared.data.is_empty());
        assert!(!prepared.headers.is_empty());
    }

    #[test]
    fn test_prepare_traces_payload_with_agent_version() {
        let agent_version = AgentResponsePayloadVersion::new();
        let serializer = TraceSerializer::new();
        let traces = vec![vec![create_test_span()]];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(
            traces,
            header_tags,
            &TracerMetadata::default(),
            Some(&agent_version),
            TraceExporterOutputFormat::V04,
        );
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 1);
        assert!(prepared.headers.contains_key(DATADOG_RATES_PAYLOAD_VERSION));
    }

    #[test]
    fn test_prepare_traces_payload_empty_traces() {
        let serializer = TraceSerializer::new();
        let traces: Vec<Vec<SpanBytes>> = vec![];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(
            traces,
            header_tags,
            &TracerMetadata::default(),
            None,
            TraceExporterOutputFormat::V04,
        );
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 0);
        assert!(!prepared.data.is_empty()); // Even empty traces result in some serialized data
        assert_eq!(prepared.headers.get(DATADOG_TRACE_COUNT).unwrap(), "0");
    }

    #[test]
    fn test_header_tags_conversion() {
        let header_tags = TracerHeaderTags {
            lang: "python",
            lang_version: "3.9.0",
            tracer_version: "2.0.0",
            lang_interpreter: "cpython",
            lang_vendor: "python.org",
            client_computed_stats: false,
            client_computed_top_level: true,
            ..Default::default()
        };

        let serializer = TraceSerializer::new();
        let headers = serializer.build_traces_headers(header_tags, 1, None);

        assert_eq!(headers.get("datadog-meta-lang").unwrap(), "python");
        assert_eq!(headers.get("datadog-meta-lang-version").unwrap(), "3.9.0");
        assert_eq!(headers.get("datadog-meta-tracer-version").unwrap(), "2.0.0");
        assert_eq!(
            headers.get("datadog-meta-lang-interpreter").unwrap(),
            "cpython"
        );
        assert_eq!(
            headers.get("datadog-meta-lang-interpreter-vendor").unwrap(),
            "python.org"
        );
        assert!(!headers.contains_key("datadog-client-computed-stats"));
        assert!(headers.contains_key("datadog-client-computed-top-level"));
    }
}
