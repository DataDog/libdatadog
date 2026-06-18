// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export configuration.

use http::HeaderMap;
use std::time::Duration;

/// OTLP trace export protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OtlpProtocol {
    /// HTTP with JSON body (Content-Type: application/json). Default for HTTP.
    #[default]
    HttpJson,
    /// HTTP with protobuf body (Content-Type: application/x-protobuf).
    HttpProtobuf,
    /// gRPC. Parsed by `FromStr` so callers get a clean error, but rejected at export time
    /// (unsupported).
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

/// The wire encoding actually used to send OTLP traces over HTTP. Internal, closed set: the
/// only encodings the exporter supports. The user-facing [`OtlpProtocol`] (which also carries the
/// unsupported `Grpc`) converts into this at the send boundary via `TryFrom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtlpWireProtocol {
    Json,
    Protobuf,
}

impl std::convert::TryFrom<OtlpProtocol> for OtlpWireProtocol {
    type Error = OtlpProtocol;
    /// Maps the user-facing protocol to a supported wire encoding. `Grpc` is unsupported and
    /// returns `Err(Grpc)` so the caller surfaces a clean error instead of silently downgrading.
    fn try_from(p: OtlpProtocol) -> Result<Self, Self::Error> {
        match p {
            OtlpProtocol::HttpJson => Ok(OtlpWireProtocol::Json),
            OtlpProtocol::HttpProtobuf => Ok(OtlpWireProtocol::Protobuf),
            other => Err(other),
        }
    }
}

impl OtlpWireProtocol {
    /// The HTTP `Content-Type` for this encoding.
    pub fn content_type(&self) -> http::HeaderValue {
        match self {
            OtlpWireProtocol::Json => libdd_common::header::APPLICATION_JSON,
            OtlpWireProtocol::Protobuf => libdd_common::header::APPLICATION_PROTOBUF,
        }
    }

    /// Encode the prost OTLP request to this wire format.
    pub fn encode(
        &self,
        req: &libdd_trace_utils::otlp_encoder::ProtoExportTraceServiceRequest,
    ) -> Result<Vec<u8>, serde_json::Error> {
        match self {
            OtlpWireProtocol::Json => libdd_trace_utils::otlp_encoder::encode_otlp_json(req),
            OtlpWireProtocol::Protobuf => {
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
    fn wire_protocol_from_user_protocol() {
        use std::convert::TryFrom;
        assert_eq!(
            OtlpWireProtocol::try_from(OtlpProtocol::HttpJson).unwrap(),
            OtlpWireProtocol::Json
        );
        assert_eq!(
            OtlpWireProtocol::try_from(OtlpProtocol::HttpProtobuf).unwrap(),
            OtlpWireProtocol::Protobuf
        );
        assert!(OtlpWireProtocol::try_from(OtlpProtocol::Grpc).is_err());
    }

    #[test]
    fn wire_protocol_content_types() {
        assert_eq!(
            OtlpWireProtocol::Json.content_type(),
            libdd_common::header::APPLICATION_JSON
        );
        assert_eq!(
            OtlpWireProtocol::Protobuf.content_type(),
            libdd_common::header::APPLICATION_PROTOBUF
        );
    }
}
