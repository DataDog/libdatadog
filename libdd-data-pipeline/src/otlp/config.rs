// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export configuration.

use http::HeaderMap;
use std::time::Duration;

/// OTLP trace export protocol: selects the wire transport and body encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// HTTP with a JSON body (`Content-Type: application/json`). The default.
    #[default]
    HttpJson,
    /// HTTP with a protobuf body (`Content-Type: application/x-protobuf`).
    HttpProtobuf,
    /// gRPC over HTTP/2.
    Grpc,
}

impl std::str::FromStr for OtlpProtocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http/json" => Ok(OtlpProtocol::HttpJson),
            "http/protobuf" => Ok(OtlpProtocol::HttpProtobuf),
            "grpc" => Ok(OtlpProtocol::Grpc),
            other => Err(format!("unknown OTLP protocol: {other}")),
        }
    }
}

impl OtlpProtocol {
    /// The HTTP `Content-Type` for this protocol's body encoding, or `None` for [`Self::Grpc`].
    pub(crate) fn content_type(&self) -> Option<http::HeaderValue> {
        match self {
            OtlpProtocol::HttpJson => Some(libdd_common::header::APPLICATION_JSON),
            OtlpProtocol::HttpProtobuf => Some(libdd_common::header::APPLICATION_PROTOBUF),
            OtlpProtocol::Grpc => None,
        }
    }

    /// Encode the prost OTLP request to this protocol's wire format, or `None` for
    /// [`Self::Grpc`].
    pub(crate) fn encode(
        &self,
        req: &libdd_trace_utils::otlp_encoder::ProtoExportTraceServiceRequest,
    ) -> Option<Result<Vec<u8>, serde_json::Error>> {
        match self {
            OtlpProtocol::HttpJson => Some(libdd_trace_utils::otlp_encoder::encode_otlp_json(req)),
            OtlpProtocol::HttpProtobuf => Some(Ok(
                libdd_trace_utils::otlp_encoder::encode_otlp_protobuf(req),
            )),
            OtlpProtocol::Grpc => None,
        }
    }
}

/// Default timeout for OTLP export requests.
pub const DEFAULT_OTLP_TIMEOUT: Duration = Duration::from_secs(10);

/// Parsed OTLP trace exporter configuration.
#[derive(Clone, Debug)]
pub struct OtlpTraceConfig {
    /// Full URL to POST traces to (e.g. `http://localhost:4318/v1/traces`).
    pub endpoint_url: String,
    /// Pre-validated HTTP headers to include in each request.
    pub headers: HeaderMap,
    /// Request timeout.
    pub timeout: Duration,
    /// OTLP export protocol (selects body encoding and content-type).
    pub protocol: OtlpProtocol,
    /// OTLP instrumentation scope name for exported traces.
    pub instrumentation_scope_name: String,
    /// OTLP instrumentation scope version for exported traces.
    pub instrumentation_scope_version: String,
    /// When `true`, omit DD-specific per-span attributes (`service.name`, `operation.name`,
    /// `resource.name`, `span.type`, `error.*`, `span.kind`) from the OTLP payload.
    pub otel_trace_semantics_enabled: bool,
}

/// Per-request OTLP gRPC trace exporter configuration.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct OtlpGrpcTraceConfig {
    /// Custom key-value pairs forwarded as gRPC request metadata.
    pub headers: Vec<(String, String)>,
    /// Per-request timeout.
    pub timeout: Duration,
    /// When `true`, omit DD-specific per-span attributes from the payload.
    pub otel_trace_semantics_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    #[test]
    fn protocol_from_str() {
        assert_eq!(
            OtlpProtocol::from_str("http/json").unwrap(),
            OtlpProtocol::HttpJson
        );
        assert_eq!(
            OtlpProtocol::from_str("http/protobuf").unwrap(),
            OtlpProtocol::HttpProtobuf
        );
        assert_eq!(OtlpProtocol::from_str("grpc").unwrap(), OtlpProtocol::Grpc);
        assert!(OtlpProtocol::from_str("nonsense").is_err());
    }

    #[test]
    fn protocol_content_types() {
        assert_eq!(
            OtlpProtocol::HttpJson.content_type(),
            Some(libdd_common::header::APPLICATION_JSON)
        );
        assert_eq!(
            OtlpProtocol::HttpProtobuf.content_type(),
            Some(libdd_common::header::APPLICATION_PROTOBUF)
        );
        assert_eq!(OtlpProtocol::Grpc.content_type(), None);
    }
}

/// Parsed OTLP trace-metrics exporter configuration.
#[derive(Clone, Debug)]
pub struct OtlpMetricsConfig {
    /// Full URL to POST metrics to (e.g. `http://localhost:4318/v1/metrics`).
    pub endpoint_url: String,
    /// Pre-validated HTTP headers to include in each request.
    pub headers: HeaderMap,
    /// Request timeout.
    pub timeout: Duration,
    /// Protocol (for future use; currently only HttpJson is supported).
    #[allow(dead_code)]
    pub(crate) protocol: OtlpProtocol,
    /// Retained for downstream compatibility; OTLP trace metrics ignore this value.
    #[allow(dead_code)]
    pub otel_trace_semantics_enabled: bool,
}
