// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON encoder: maps Datadog spans to ExportTraceServiceRequest.

pub mod json_types;
pub mod mapper;
pub mod proto_convert;

pub use json_types::ExportTraceServiceRequest;
pub use mapper::map_traces_to_otlp;

use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoExportTraceServiceRequest;
use prost::Message;

/// Serialize an OTLP request to the HTTP/JSON wire format.
pub fn encode_otlp_json(req: &ExportTraceServiceRequest) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(req)
}

/// Serialize an OTLP request to the HTTP/protobuf wire format.
pub fn encode_otlp_protobuf(req: &ExportTraceServiceRequest) -> Vec<u8> {
    let proto: ProtoExportTraceServiceRequest = req.into();
    proto.encode_to_vec()
}

#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::span::v04::Span;
    use crate::span::BytesData;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
    use prost::Message;

    fn sample() -> ExportTraceServiceRequest {
        let resource_info = OtlpResourceInfo {
            service: "svc".to_string(),
            ..Default::default()
        };
        let span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 0xEEE19B7EC3C1B174,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            start: 1,
            duration: 2,
            ..Default::default()
        };
        map_traces_to_otlp(vec![vec![span]], &resource_info)
    }

    #[test]
    fn json_and_protobuf_carry_same_span() {
        let req = sample();
        let json = encode_otlp_json(&req).unwrap();
        let pb = encode_otlp_protobuf(&req);

        let json_v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let json_name = json_v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();

        let proto = ProtoReq::decode(pb.as_slice()).unwrap();
        let proto_name = proto.resource_spans[0].scope_spans[0].spans[0].name.clone();

        assert_eq!(json_name, "res");
        assert_eq!(proto_name, "res");
        let json_sid = json_v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["spanId"]
            .as_str()
            .unwrap()
            .to_string();
        let proto_sid = &proto.resource_spans[0].scope_spans[0].spans[0].span_id;
        assert_eq!(json_sid, hex::encode(proto_sid));
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
