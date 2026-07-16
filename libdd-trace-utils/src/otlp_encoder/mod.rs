// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP encoder: maps Datadog spans to the prost OTLP types (the IR), then to the HTTP/protobuf
//! or HTTP/JSON wire format.

pub(crate) mod json_serializer;
pub mod mapper;

pub use mapper::map_traces_to_otlp;

pub use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoExportTraceServiceRequest;
use prost::Message;

/// Serialize the prost OTLP request to the HTTP/protobuf wire format.
pub fn encode_otlp_protobuf(req: &ProtoExportTraceServiceRequest) -> Vec<u8> {
    req.encode_to_vec()
}

/// Serialize the prost OTLP request to the HTTP/JSON wire format (OTLP/JSON spec).
pub fn encode_otlp_json(req: &ProtoExportTraceServiceRequest) -> serde_json::Result<Vec<u8>> {
    json_serializer::to_otlp_json_vec(req)
}

/// Tracer-level attributes used to populate the OTLP Resource on export.
///
/// These are the fields from the tracer's configuration that map to OTLP Resource attributes
/// (service.name, deployment.environment.name, service.version, telemetry.sdk.*, runtime-id).
/// Callers should build this from their own tracer metadata struct.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct OtlpResourceInfo {
    pub service: String,
    pub env: String,
    pub app_version: String,
    pub language: String,
    pub tracer_version: String,
    pub runtime_id: String,
    pub hostname: String,
    pub process_tags: String,
    pub instrumentation_scope_name: String,
    pub instrumentation_scope_version: String,
    /// When true, emits `_dd.stats_computed: "true"` on the OTLP resource to prevent
    /// double-counted APM metrics in Datadog Agent OTLP receivers (backwards compatible).
    pub client_computed_stats: bool,
}

#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::span::v04::Span;
    use crate::span::BytesData;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as ProtoValue;
    use prost::Message;

    fn sample_native() -> (Vec<Vec<Span<BytesData>>>, OtlpResourceInfo) {
        let resource_info = OtlpResourceInfo {
            service: "svc".to_string(),
            ..Default::default()
        };
        let mut span: Span<BytesData> = Span {
            trace_id: 0x5b8efff798038103_d269b633813fc60c_u128,
            span_id: 0xEEE19B7EC3C1B174,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            start: 1,
            duration: 2,
            error: 1,
            ..Default::default()
        };
        span.meta.insert(
            "error.msg".into(),
            libdd_tinybytes::BytesString::from_static("boom"),
        );
        span.meta.insert(
            "http.method".into(),
            libdd_tinybytes::BytesString::from_static("GET"),
        );
        (vec![vec![span]], resource_info)
    }

    #[test]
    fn json_and_protobuf_carry_same_span() {
        // Decisive guard: JSON and protobuf are encoded from the *same* prost IR, so the two
        // wire formats cannot drift.
        let (chunks, info) = sample_native();
        let req = map_traces_to_otlp(chunks, &info, false);
        let json = encode_otlp_json(&req).unwrap();
        let pb = encode_otlp_protobuf(&req);

        let jv: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let jspan = &jv["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        let proto = ProtoReq::decode(pb.as_slice()).unwrap();
        let pspan = &proto.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(jspan["name"].as_str().unwrap(), pspan.name);
        assert_eq!(
            jspan["spanId"].as_str().unwrap(),
            hex::encode(&pspan.span_id)
        );
        assert_eq!(
            jspan["traceId"].as_str().unwrap(),
            hex::encode(&pspan.trace_id)
        );
        let pst = pspan.status.as_ref().unwrap();
        assert_eq!(jspan["status"]["code"].as_i64().unwrap() as i32, pst.code);
        assert_eq!(jspan["status"]["message"].as_str().unwrap(), pst.message);
        let jattr = jspan["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "http.method")
            .unwrap();
        let pattr = pspan
            .attributes
            .iter()
            .find(|a| a.key == "http.method")
            .unwrap();
        let pval = match pattr.value.as_ref().unwrap().value.as_ref().unwrap() {
            ProtoValue::StringValue(v) => v.as_str(),
            other => panic!("expected string, got {other:?}"),
        };
        assert_eq!(jattr["value"]["stringValue"].as_str().unwrap(), pval);
        assert_eq!(jattr["value"]["stringValue"].as_str().unwrap(), "GET");
    }

    #[test]
    fn protobuf_round_trips_through_prost() {
        // Round-trip the IR through the protobuf wire format: decoding the encoded bytes
        // reproduces the original prost request, i.e. the encoding is lossless. (A JSON round-trip
        // would need a deserializer mirroring `json_serializer`, which this crate doesn't ship;
        // `json_and_protobuf_carry_same_span` guards that the JSON matches this same IR.)
        let (chunks, info) = sample_native();
        let req = map_traces_to_otlp(chunks, &info, false);
        let decoded = ProtoReq::decode(encode_otlp_protobuf(&req).as_slice()).unwrap();
        assert_eq!(decoded, req);
    }
}
