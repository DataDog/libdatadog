// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON encoder: maps Datadog spans to ExportTraceServiceRequest.

pub mod json_types;
pub mod mapper;
pub mod proto_mapper;

pub use json_types::ExportTraceServiceRequest;
pub use mapper::map_traces_to_otlp;
pub use proto_mapper::map_traces_to_otlp_proto;

use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoExportTraceServiceRequest;
use prost::Message;

/// Serialize an OTLP request to the HTTP/JSON wire format.
pub fn encode_otlp_json(req: &ExportTraceServiceRequest) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(req)
}

/// Serialize a prost OTLP request to the HTTP/protobuf wire format.
pub fn encode_otlp_protobuf(req: &ProtoExportTraceServiceRequest) -> Vec<u8> {
    req.encode_to_vec()
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
        // Build the JSON request and the prost request from the same native spans.
        let (chunks, resource_info) = sample_native();
        let json = encode_otlp_json(&map_traces_to_otlp(chunks.clone(), &resource_info)).unwrap();
        let pb = encode_otlp_protobuf(&map_traces_to_otlp_proto(chunks, &resource_info));

        let json_v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let jspan = &json_v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        let proto = ProtoReq::decode(pb.as_slice()).unwrap();
        let pspan = &proto.resource_spans[0].scope_spans[0].spans[0];

        // name
        assert_eq!(jspan["name"].as_str().unwrap(), pspan.name);
        // span_id: JSON hex string == prost raw bytes, hex-encoded
        assert_eq!(
            jspan["spanId"].as_str().unwrap(),
            hex::encode(&pspan.span_id)
        );
        // trace_id: same, full 128 bits
        assert_eq!(
            jspan["traceId"].as_str().unwrap(),
            hex::encode(&pspan.trace_id)
        );
        // status: code + message
        let pstatus = pspan.status.as_ref().expect("proto status");
        assert_eq!(
            jspan["status"]["code"].as_i64().unwrap() as i32,
            pstatus.code
        );
        assert_eq!(
            jspan["status"]["message"].as_str().unwrap_or(""),
            pstatus.message
        );
        // one attribute: http.method == "GET" in both encodings
        let jattr = jspan["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "http.method")
            .expect("json http.method");
        assert_eq!(jattr["value"]["stringValue"].as_str().unwrap(), "GET");
        let pattr = pspan
            .attributes
            .iter()
            .find(|a| a.key == "http.method")
            .expect("proto http.method");
        let pval = match pattr.value.as_ref().unwrap().value.as_ref().unwrap() {
            ProtoValue::StringValue(s) => s.as_str(),
            other => panic!("expected string value, got {other:?}"),
        };
        assert_eq!(pval, "GET");
    }
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
}
