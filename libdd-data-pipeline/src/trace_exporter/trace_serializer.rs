// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::trace_exporter::TraceExporterOutputFormat;
use crate::trace_exporter::agent_response::{
    AgentResponsePayloadVersion, DATADOG_RATES_PAYLOAD_VERSION,
};
use crate::trace_exporter::error::TraceExporterError;
use http::{HeaderMap, HeaderValue, header::CONTENT_TYPE};
use libdd_common::header::{
    APPLICATION_MSGPACK, DATADOG_SEND_REAL_HTTP_STATUS, DATADOG_TRACE_COUNT,
};
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::msgpack_decoder::decode::error::DecodeError;
use libdd_trace_utils::msgpack_encoder;
use libdd_trace_utils::span::TraceData;
use libdd_trace_utils::span::v1::TracerPayload;
use libdd_trace_utils::trace_utils::{self, TracerHeaderTags};
use libdd_trace_utils::tracer_payload::{self};

/// Minimal capacity of fresh buffers allocated to encode traces, in bytes.
const MIN_BUFFER_CAPACITY: usize = 1024;

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

    /// Build HTTP headers for traces request
    pub(super) fn build_traces_headers(
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

    /// Serialize the V1 `payload` to the msgpack wire format for `output_format`.
    pub(super) fn serialize_payload_v1<T: TraceData>(
        &self,
        payload: &TracerPayload<T>,
        output_format: TraceExporterOutputFormat,
    ) -> Result<Vec<u8>, TraceExporterError> {
        let capacity = self
            .previous_serialised_len
            .load(Ordering::Relaxed)
            .max(MIN_BUFFER_CAPACITY);
        // The capacity is only a pre-allocation hint, saturating is fine.
        let capacity_hint = u32::try_from(capacity).unwrap_or(u32::MAX);
        let buff = match output_format {
            TraceExporterOutputFormat::V04 => {
                msgpack_encoder::v04::to_vec_with_capacity_from_v1(payload, capacity_hint)
            }
            TraceExporterOutputFormat::V1 => {
                msgpack_encoder::v1::to_vec_with_capacity_from_v1(payload, capacity_hint)
            }
            TraceExporterOutputFormat::V05 => serialize_v05(payload, capacity)?,
        };
        self.previous_serialised_len
            .store(buff.len(), Ordering::Relaxed);
        Ok(buff)
    }
}

/// Serialize the V1 `payload` as v0.5.
///
/// There is no V1 → v0.5 encoder: the payload is downgraded to the v0.4 wire format, decoded back
/// as borrowed v0.4 spans and converted to v0.5 from there. This keeps the v0.5 output identical
/// to the v0.4 downgrade (same promoted fields, flattened attributes, ...) at the cost of an extra
/// encode/decode, which is acceptable for this legacy output format.
fn serialize_v05<T: TraceData>(
    payload: &TracerPayload<T>,
    capacity: usize,
) -> Result<Vec<u8>, TraceExporterError> {
    let v04 = msgpack_encoder::v04::to_vec_from_v1(payload);
    let (traces, _) =
        msgpack_decoder::v04::from_slice(&v04).map_err(TraceExporterError::Deserialization)?;
    let map_err = |e: anyhow::Error| {
        TraceExporterError::Deserialization(DecodeError::InvalidFormat(e.to_string()))
    };
    match trace_utils::convert_trace_chunks_v04_to_v05(&traces).map_err(map_err)? {
        tracer_payload::TraceChunks::V05(p) => {
            let mut buff = Vec::with_capacity(capacity);
            rmp_serde::encode::write(&mut buff, &p).map_err(TraceExporterError::Serialization)?;
            Ok(buff)
        }
        // `convert_trace_chunks_v04_to_v05` always returns `V05`.
        _ => Err(TraceExporterError::Deserialization(
            DecodeError::InvalidFormat(
                "v0.5 conversion produced an unexpected payload variant".to_owned(),
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::agent_response::AgentResponsePayloadVersion;
    use http::header::CONTENT_TYPE;
    use libdd_common::header::APPLICATION_MSGPACK_STR;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::span::BytesData;
    use libdd_trace_utils::span::v1::from_v04::trace_chunk_from_v04;
    use libdd_trace_utils::span::v04::{SpanBytes, SpanSlice};
    use libdd_trace_utils::trace_utils::{TracerGenericTags, TracerHeaderTags};

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
            generic: TracerGenericTags {
                client_computed_stats: true,
                client_computed_top_level: false,
                ..Default::default()
            },
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

    fn create_test_payload() -> TracerPayload<BytesData> {
        TracerPayload {
            language_name: BytesString::from_static("rust"),
            chunks: vec![trace_chunk_from_v04(vec![create_test_span()])],
            ..Default::default()
        }
    }

    fn assert_test_span_roundtrip(span: &SpanSlice<'_>) {
        let original = create_test_span();
        assert_eq!(original.name, span.name.as_ref());
        assert_eq!(original.service, span.service.as_ref());
        assert_eq!(original.resource, span.resource.as_ref());
        assert_eq!(original.r#type, span.r#type.as_ref());
        assert_eq!(original.start, span.start);
        assert_eq!(original.duration, span.duration);
        assert_eq!(original.span_id, span.span_id);
        assert_eq!(original.trace_id, span.trace_id);
        assert_eq!(original.parent_id, span.parent_id);
        assert_eq!(original.error, span.error);
    }

    #[test]
    fn test_serialize_payload_v1_as_v04() {
        let serializer = TraceSerializer::new();
        let serialized = serializer
            .serialize_payload_v1(&create_test_payload(), TraceExporterOutputFormat::V04)
            .unwrap();

        let (traces, _) = libdd_trace_utils::msgpack_decoder::v04::from_slice(&serialized).unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].len(), 1);
        assert_test_span_roundtrip(&traces[0][0]);
        assert_eq!(
            serializer.previous_serialised_len.load(Ordering::Relaxed),
            serialized.len()
        );
    }

    #[test]
    fn test_serialize_payload_v1_as_v05() {
        let serializer = TraceSerializer::new();
        let serialized = serializer
            .serialize_payload_v1(&create_test_payload(), TraceExporterOutputFormat::V05)
            .unwrap();

        let (traces, _) = libdd_trace_utils::msgpack_decoder::v05::from_slice(&serialized).unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].len(), 1);
        assert_test_span_roundtrip(&traces[0][0]);
    }

    #[test]
    fn test_serialize_payload_v1_as_v1() {
        let serializer = TraceSerializer::new();
        let serialized = serializer
            .serialize_payload_v1(&create_test_payload(), TraceExporterOutputFormat::V1)
            .unwrap();

        let (payload, _) = libdd_trace_utils::msgpack_decoder::v1::from_slice(&serialized).unwrap();
        assert_eq!(payload.language_name, "rust");
        assert_eq!(payload.chunks.len(), 1);
        let span = &payload.chunks[0].spans[0];
        assert_eq!(span.name, "test_span");
        assert_eq!(span.span_id, 123);
        assert_eq!(span.parent_id, 789);
    }

    #[test]
    fn test_serialize_payload_v1_empty() {
        let serializer = TraceSerializer::new();
        let payload = TracerPayload::<BytesData>::default();
        for format in [
            TraceExporterOutputFormat::V04,
            TraceExporterOutputFormat::V05,
            TraceExporterOutputFormat::V1,
        ] {
            // Even empty payloads result in some serialized data.
            assert!(
                !serializer
                    .serialize_payload_v1(&payload, format)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn test_header_tags_conversion() {
        let header_tags = TracerHeaderTags {
            lang: "python",
            lang_version: "3.9.0",
            tracer_version: "2.0.0",
            lang_interpreter: "cpython",
            lang_vendor: "python.org",
            generic: TracerGenericTags {
                client_computed_stats: false,
                client_computed_top_level: true,
                ..Default::default()
            },
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
