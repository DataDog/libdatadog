// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export configuration.

use std::time::Duration;

/// OTLP trace export protocol. HTTP/JSON is currently supported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OtlpProtocol {
    /// HTTP with JSON body (Content-Type: application/json). Default for HTTP.
    #[default]
    HttpJson,
    /// HTTP with protobuf body. (Not supported yet)
    HttpProtobuf,
    /// gRPC. (Not supported yet)
    Grpc,
}

/// Default timeout for OTLP export requests.
pub const DEFAULT_OTLP_TIMEOUT: Duration = Duration::from_secs(10);

/// Parsed OTLP trace exporter configuration.
#[derive(Clone, Debug)]
pub struct OtlpTraceConfig {
    /// Full URL to POST traces to (e.g. `http://localhost:4318/v1/traces`).
    pub endpoint_url: String,
    /// Optional HTTP headers (key-value pairs).
    pub headers: Vec<(String, String)>,
    /// Request timeout.
    pub timeout: Duration,
    /// Protocol (for future use; currently only HttpJson is supported).
    #[allow(dead_code)]
    pub(crate) protocol: OtlpProtocol,
}
