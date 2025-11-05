// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::trace_exporter::agent_response::{
    AgentResponsePayloadVersion, DATADOG_RATES_PAYLOAD_VERSION_HEADER,
};
use crate::trace_exporter::error::TraceExporterError;
use crate::trace_exporter::TraceExporterOutputFormat;
use datadog_trace_utils::msgpack_decoder::decode::error::DecodeError;
use datadog_trace_utils::msgpack_encoder;
use datadog_trace_utils::span::{Span, SpanText};
use datadog_trace_utils::trace_utils::{self, TracerHeaderTags};
use datadog_trace_utils::tracer_payload;
use ddcommon::header::{
    APPLICATION_MSGPACK_STR, DATADOG_SEND_REAL_HTTP_STATUS_STR, DATADOG_TRACE_COUNT_STR,
};
use hyper::header::CONTENT_TYPE;
use std::collections::HashMap;

/// Prepared traces payload ready for sending to the agent
pub(super) struct PreparedTracesPayload {
    /// Serialized msgpack payload
    pub data: Vec<u8>,
    /// HTTP headers for the request
    pub headers: HashMap<&'static str, String>,
    /// Number of trace chunks
    pub chunk_count: usize,
}

/// Trace serialization client for handling payload preparation
pub(super) struct TraceSerializer<'a> {
    output_format: TraceExporterOutputFormat,
    agent_payload_response_version: Option<&'a AgentResponsePayloadVersion>,
}

impl<'a> TraceSerializer<'a> {
    /// Create a new trace serializer
    pub(super) fn new(
        output_format: TraceExporterOutputFormat,
        agent_payload_response_version: Option<&'a AgentResponsePayloadVersion>,
    ) -> Self {
        Self {
            output_format,
            agent_payload_response_version,
        }
    }

    /// Prepare traces payload and HTTP headers for sending to agent
    pub(super) fn prepare_traces_payload<T: SpanText>(
        &self,
        traces: Vec<Vec<Span<T>>>,
        header_tags: TracerHeaderTags,
    ) -> Result<PreparedTracesPayload, TraceExporterError> {
        let payload = self.collect_and_process_traces(traces)?;
        let chunks = payload.size();
        let headers = self.build_traces_headers(header_tags, chunks);
        let mp_payload = self.serialize_payload(&payload)?;

        Ok(PreparedTracesPayload {
            data: mp_payload,
            headers,
            chunk_count: chunks,
        })
    }

    /// Collect trace chunks based on output format
    fn collect_and_process_traces<T: SpanText>(
        &self,
        traces: Vec<Vec<Span<T>>>,
    ) -> Result<tracer_payload::TraceChunks<T>, TraceExporterError> {
        let use_v05_format = match self.output_format {
            TraceExporterOutputFormat::V05 => true,
            TraceExporterOutputFormat::V04 => false,
        };
        trace_utils::collect_trace_chunks(traces, use_v05_format).map_err(|e| {
            TraceExporterError::Deserialization(DecodeError::InvalidFormat(e.to_string()))
        })
    }

    /// Build HTTP headers for traces request
    fn build_traces_headers(
        &self,
        header_tags: TracerHeaderTags,
        chunk_count: usize,
    ) -> HashMap<&'static str, String> {
        let mut headers: HashMap<&'static str, String> = header_tags.into();
        headers.insert(DATADOG_SEND_REAL_HTTP_STATUS_STR, "1".to_string());
        headers.insert(DATADOG_TRACE_COUNT_STR, chunk_count.to_string());
        headers.insert(CONTENT_TYPE.as_str(), APPLICATION_MSGPACK_STR.to_string());
        if let Some(agent_payload_response_version) = &self.agent_payload_response_version {
            headers.insert(
                DATADOG_RATES_PAYLOAD_VERSION_HEADER,
                agent_payload_response_version.header_value(),
            );
        }
        headers
    }

    /// Serialize payload to msgpack format
    fn serialize_payload<T: SpanText>(
        &self,
        payload: &tracer_payload::TraceChunks<T>,
    ) -> Result<Vec<u8>, TraceExporterError> {
        match payload {
            tracer_payload::TraceChunks::V04(p) => Ok(msgpack_encoder::v04::to_vec(p)),
            tracer_payload::TraceChunks::V05(p) => {
                rmp_serde::to_vec(p).map_err(TraceExporterError::Serialization)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::agent_response::AgentResponsePayloadVersion;
    use datadog_trace_utils::span::SpanBytes;
    use datadog_trace_utils::trace_utils::TracerHeaderTags;
    use ddcommon::header::{
        APPLICATION_MSGPACK_STR, DATADOG_SEND_REAL_HTTP_STATUS_STR, DATADOG_TRACE_COUNT_STR,
    };
    use hyper::header::CONTENT_TYPE;
    use libdd_tinybytes::BytesString;

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
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        assert!(matches!(
            serializer.output_format,
            TraceExporterOutputFormat::V04
        ));
        assert!(serializer.agent_payload_response_version.is_none());
    }

    #[test]
    fn test_trace_serializer_new_with_agent_version() {
        let agent_version = AgentResponsePayloadVersion::new();
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V05, Some(&agent_version));
        assert!(matches!(
            serializer.output_format,
            TraceExporterOutputFormat::V05
        ));
        assert!(serializer.agent_payload_response_version.is_some());
    }

    #[test]
    fn test_build_traces_headers() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let header_tags = create_test_header_tags();
        let headers = serializer.build_traces_headers(header_tags, 3);

        // Check basic headers are present
        assert_eq!(headers.get(DATADOG_SEND_REAL_HTTP_STATUS_STR).unwrap(), "1");
        assert_eq!(headers.get(DATADOG_TRACE_COUNT_STR).unwrap(), "3");
        assert_eq!(
            headers.get(CONTENT_TYPE.as_str()).unwrap(),
            APPLICATION_MSGPACK_STR
        );

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
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, Some(&agent_version));
        let header_tags = create_test_header_tags();
        let headers = serializer.build_traces_headers(header_tags, 2);

        // Check that agent payload version header is included
        assert!(headers.contains_key(DATADOG_RATES_PAYLOAD_VERSION_HEADER));
        assert_eq!(headers.get(DATADOG_TRACE_COUNT_STR).unwrap(), "2");
    }

    #[test]
    fn test_collect_and_process_traces_v04() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let traces = vec![vec![create_test_span()]];

        let result = serializer.collect_and_process_traces(traces);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(matches!(payload, tracer_payload::TraceChunks::V04(_)));
        assert_eq!(payload.size(), 1);
    }

    #[test]
    fn test_collect_and_process_traces_v05() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V05, None);
        let traces = vec![vec![create_test_span()]];

        let result = serializer.collect_and_process_traces(traces);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(matches!(payload, tracer_payload::TraceChunks::V05(_)));
        assert_eq!(payload.size(), 1);
    }

    #[test]
    fn test_collect_and_process_traces_multiple_chunks() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let traces = vec![
            vec![create_test_span()],
            vec![create_test_span(), create_test_span()],
            vec![create_test_span()],
        ];

        let result = serializer.collect_and_process_traces(traces);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.size(), 3);
    }

    #[test]
    fn test_serialize_payload_v04() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let original_traces = vec![vec![create_test_span()]];
        let payload = serializer
            .collect_and_process_traces(original_traces.clone())
            .unwrap();

        let result = serializer.serialize_payload(&payload);
        assert!(result.is_ok());

        let serialized = result.unwrap();
        assert!(!serialized.is_empty());

        // Verify we can deserialize it back and data integrity is preserved
        let (deserialized_traces, _) =
            datadog_trace_utils::msgpack_decoder::v04::from_slice(&serialized).unwrap();
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
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V05, None);
        let original_traces = vec![vec![create_test_span()]];
        let payload = serializer
            .collect_and_process_traces(original_traces.clone())
            .unwrap();

        let result = serializer.serialize_payload(&payload);
        assert!(result.is_ok());

        let serialized = result.unwrap();
        assert!(!serialized.is_empty());

        // Verify we can deserialize it back and data integrity is preserved
        let (deserialized_traces, _) =
            datadog_trace_utils::msgpack_decoder::v05::from_slice(&serialized).unwrap();
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
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let traces = vec![
            vec![create_test_span()],
            vec![create_test_span(), create_test_span()],
        ];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(traces, header_tags);
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 2);
        assert!(!prepared.data.is_empty());
        assert!(!prepared.headers.is_empty());

        // Check headers
        assert_eq!(prepared.headers.get(DATADOG_TRACE_COUNT_STR).unwrap(), "2");
        assert_eq!(prepared.headers.get("datadog-meta-lang").unwrap(), "rust");
    }

    #[test]
    fn test_prepare_traces_payload_v05() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V05, None);
        let traces = vec![vec![create_test_span()]];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(traces, header_tags);
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 1);
        assert!(!prepared.data.is_empty());
        assert!(!prepared.headers.is_empty());
    }

    #[test]
    fn test_prepare_traces_payload_with_agent_version() {
        let agent_version = AgentResponsePayloadVersion::new();
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, Some(&agent_version));
        let traces = vec![vec![create_test_span()]];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(traces, header_tags);
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 1);
        assert!(prepared
            .headers
            .contains_key(DATADOG_RATES_PAYLOAD_VERSION_HEADER));
    }

    #[test]
    fn test_prepare_traces_payload_empty_traces() {
        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let traces: Vec<Vec<SpanBytes>> = vec![];
        let header_tags = create_test_header_tags();

        let result = serializer.prepare_traces_payload(traces, header_tags);
        assert!(result.is_ok());

        let prepared = result.unwrap();
        assert_eq!(prepared.chunk_count, 0);
        assert!(!prepared.data.is_empty()); // Even empty traces result in some serialized data
        assert_eq!(prepared.headers.get(DATADOG_TRACE_COUNT_STR).unwrap(), "0");
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

        let serializer = TraceSerializer::new(TraceExporterOutputFormat::V04, None);
        let headers = serializer.build_traces_headers(header_tags, 1);

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
