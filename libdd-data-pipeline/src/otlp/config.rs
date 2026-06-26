// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export configuration.

use http::HeaderMap;
use std::time::Duration;

/// OTLP trace export protocol — selects the wire transport and body encoding.
///
/// All three OTel-standard protocol strings parse successfully; the selection
/// controls which send path the exporter uses:
/// - `http/json` and `http/protobuf` → OTLP over HTTP/1.1 via
///   [`HttpClientCapability`](libdd_capabilities::HttpClientCapability).
/// - `grpc` → OTLP over HTTP/2 via a tonic [`Channel`](tonic::transport::Channel).
///
/// Plaintext gRPC (`http://` scheme, port 4317) is supported. TLS gRPC
/// (`https://` scheme) is not yet implemented — use a TLS-terminating sidecar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// HTTP with a JSON body (`Content-Type: application/json`). The default.
    #[default]
    HttpJson,
    /// HTTP with a protobuf body (`Content-Type: application/x-protobuf`).
    HttpProtobuf,
    /// gRPC over HTTP/2. Protobuf-encoded body with 5-byte gRPC framing.
    /// Default port is 4317. Only plaintext (`http://`) is supported.
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
    /// The HTTP `Content-Type` for this protocol's body encoding. Crate-internal: the public type
    /// is only constructed/selected by callers; encoding is the exporter's job.
    /// Only called on the HTTP path; the gRPC path uses tonic's ProstCodec.
    pub(crate) fn content_type(&self) -> http::HeaderValue {
        #[allow(clippy::unreachable)]
        match self {
            OtlpProtocol::HttpJson => libdd_common::header::APPLICATION_JSON,
            OtlpProtocol::HttpProtobuf => libdd_common::header::APPLICATION_PROTOBUF,
            OtlpProtocol::Grpc => unreachable!("gRPC path does not call content_type()"),
        }
    }

    /// Encode the prost OTLP request to this protocol's wire format. Crate-internal so the
    /// third-party `serde_json::Error` does not leak into the public API.
    /// Only called on the HTTP path; the gRPC path uses tonic's ProstCodec.
    pub(crate) fn encode(
        &self,
        req: &libdd_trace_utils::otlp_encoder::ProtoExportTraceServiceRequest,
    ) -> Result<Vec<u8>, serde_json::Error> {
        #[allow(clippy::unreachable)]
        match self {
            OtlpProtocol::HttpJson => libdd_trace_utils::otlp_encoder::encode_otlp_json(req),
            OtlpProtocol::HttpProtobuf => {
                Ok(libdd_trace_utils::otlp_encoder::encode_otlp_protobuf(req))
            }
            OtlpProtocol::Grpc => unreachable!("gRPC path does not call encode()"),
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
    /// When `true`, omit DD-specific per-span attributes (`service.name`, `operation.name`,
    /// `resource.name`, `span.type`, `error.*`, `span.kind`) from the OTLP payload.
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
    fn grpc_parses_successfully() {
        // gRPC is now a supported protocol — it must parse without error.
        assert_eq!(OtlpProtocol::from_str("grpc").unwrap(), OtlpProtocol::Grpc);
    }

    #[test]
    fn grpc_config_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OtlpGrpcTraceConfig>();
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
}

/// Parsed OTLP gRPC trace exporter configuration.
///
/// The gRPC endpoint URL contains only scheme + host + port (e.g.
/// `http://localhost:4317`). The service path
/// `/opentelemetry.proto.collector.trace.v1.TraceService/Export` is
/// appended by the exporter.
#[derive(Clone, Debug)]
pub struct OtlpGrpcTraceConfig {
    /// Full gRPC base URL, e.g. `http://localhost:4317`.
    /// Must use `http://` scheme; `https://` (TLS) is not yet supported.
    #[allow(dead_code)]
    pub endpoint_url: String,
    /// Custom key-value pairs forwarded as gRPC request metadata.
    pub headers: Vec<(String, String)>,
    /// Per-request timeout (applied via [`tokio::time::timeout`]).
    pub timeout: Duration,
    /// When `true`, omit DD-specific per-span attributes from the payload.
    pub otel_trace_semantics_enabled: bool,
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
