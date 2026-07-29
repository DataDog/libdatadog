// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export configuration.

use http::HeaderMap;
use std::time::Duration;

/// OTLP trace export protocol — selects the HTTP body encoding and `Content-Type`.
///
/// Only the HTTP encodings libdatadog actually supports are representable. A `grpc` value (e.g.
/// resolved from the OTel-default `OTEL_EXPORTER_OTLP_PROTOCOL`) is rejected by
/// [`FromStr`](std::str::FromStr) rather than represented here, so an unsupported protocol can
/// never be constructed and silently mishandled downstream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// HTTP with a JSON body (`Content-Type: application/json`). The default.
    #[default]
    HttpJson,
    /// HTTP with a protobuf body (`Content-Type: application/x-protobuf`).
    HttpProtobuf,
}

impl std::str::FromStr for OtlpProtocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http/json" => Ok(OtlpProtocol::HttpJson),
            "http/protobuf" => Ok(OtlpProtocol::HttpProtobuf),
            // gRPC is a valid OTLP protocol in the OTel spec but is not implemented in
            // libdatadog. Reject it explicitly so callers get a clean error at the parse
            // boundary, rather than constructing an unsupported value that has to be guarded
            // against everywhere downstream.
            "grpc" => Err("OTLP gRPC export is not supported".to_string()),
            other => Err(format!("unknown OTLP protocol: {other}")),
        }
    }
}

impl OtlpProtocol {
    /// The HTTP `Content-Type` for this protocol's body encoding. Crate-internal: the public type
    /// is only constructed/selected by callers; encoding is the exporter's job.
    pub(crate) fn content_type(&self) -> http::HeaderValue {
        match self {
            OtlpProtocol::HttpJson => libdd_common::header::APPLICATION_JSON,
            OtlpProtocol::HttpProtobuf => libdd_common::header::APPLICATION_PROTOBUF,
        }
    }

    /// Encode the prost OTLP request to this protocol's wire format. Crate-internal so the
    /// third-party `serde_json::Error` does not leak into the public API.
    pub(crate) fn encode(
        &self,
        req: &libdd_trace_utils::otlp_encoder::ProtoExportTraceServiceRequest,
    ) -> Result<Vec<u8>, serde_json::Error> {
        match self {
            OtlpProtocol::HttpJson => libdd_trace_utils::otlp_encoder::encode_otlp_json(req),
            OtlpProtocol::HttpProtobuf => {
                Ok(libdd_trace_utils::otlp_encoder::encode_otlp_protobuf(req))
            }
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
// Not yet wired to the trace exporter's send loop; exercised by tests only.
#[allow(dead_code)]
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
        assert!(OtlpProtocol::from_str("nonsense").is_err());
    }

    #[test]
    fn grpc_is_rejected_at_parse() {
        // gRPC is unsupported, so it must not parse into a protocol: an unsupported value can
        // never be constructed.
        assert!(OtlpProtocol::from_str("grpc").is_err());
    }

    #[test]
    fn protocol_content_types() {
        assert_eq!(
            OtlpProtocol::HttpJson.content_type(),
            libdd_common::header::APPLICATION_JSON
        );
        assert_eq!(
            OtlpProtocol::HttpProtobuf.content_type(),
            libdd_common::header::APPLICATION_PROTOBUF
        );
    }

    #[test]
    fn grpc_config_constructs_and_clones() {
        let cfg = OtlpGrpcTraceConfig {
            headers: vec![("k".to_string(), "v".to_string())],
            timeout: Duration::from_secs(3),
            otel_trace_semantics_enabled: true,
        };
        let clone = cfg.clone();
        assert_eq!(clone.headers, cfg.headers);
        assert_eq!(clone.timeout, Duration::from_secs(3));
        assert!(clone.otel_trace_semantics_enabled);
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
    /// When `true`, emit only OTel attributes; omit `dd.*`/`_dd.*` ones
    /// (`DD_TRACE_OTEL_SEMANTICS_ENABLED`).
    pub otel_trace_semantics_enabled: bool,
}
