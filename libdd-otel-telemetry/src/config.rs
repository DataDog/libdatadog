// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

/// Wire protocol used to speak OTLP to the configured endpoint.
///
/// Mirrors the protocol choice already exposed by dd-trace-rs's in-house OTel pipeline
/// (`datadog-opentelemetry::configuration::OtlpProtocol`) so consumers can translate their
/// existing `OTEL_EXPORTER_OTLP_*_PROTOCOL` config directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
    /// HTTP with JSON encoding. Recognized for config-parsing parity with the OTel spec and
    /// dd-trace-rs, but not exportable today — the exporter build reports it as unsupported.
    HttpJson,
}

impl OtlpProtocol {
    /// Parse an `OTEL_EXPORTER_OTLP_*_PROTOCOL` value case-insensitively.
    ///
    /// Accepts `grpc`, `http/protobuf`, and `http/json` (any case, surrounding whitespace
    /// trimmed). Returns `None` for empty or unrecognized values so the caller decides how to
    /// surface the error.
    pub fn from_config_str(s: &str) -> Option<OtlpProtocol> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "grpc" => Some(OtlpProtocol::Grpc),
            "http/protobuf" => Some(OtlpProtocol::HttpProtobuf),
            "http/json" => Some(OtlpProtocol::HttpJson),
            _ => None,
        }
    }
}

/// Aggregation temporality preference for metrics export.
///
/// Kept as a crate-local type so no third-party SDK type appears in a consumer's public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Temporality {
    #[default]
    Delta,
    Cumulative,
}

impl Temporality {
    /// Parse an `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` value case-insensitively.
    ///
    /// Accepts `delta` and `cumulative` (any case, surrounding whitespace trimmed). Empty or
    /// unrecognized values default to [`Temporality::Delta`], matching Datadog's preference.
    pub fn from_config_str(s: &str) -> Temporality {
        match s.trim().to_ascii_lowercase().as_str() {
            "cumulative" => Temporality::Cumulative,
            _ => Temporality::Delta,
        }
    }
}

/// Parse an `OTEL_EXPORTER_OTLP_*_HEADERS` string (`k1=v1,k2=v2`) into key/value pairs.
///
/// Entries without an `=` (or with an empty key) are skipped; keys and values are trimmed.
pub fn parse_otlp_headers(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|item| {
            let (key, value) = item.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Configuration for the OTLP metrics exporter.
#[derive(Debug, Clone)]
pub struct OtlpExporterConfig {
    pub endpoint: String,
    pub protocol: OtlpProtocol,
    pub timeout: Duration,
    pub headers: Vec<(String, String)>,
}

impl OtlpExporterConfig {
    pub fn new(endpoint: impl Into<String>, protocol: OtlpProtocol) -> Self {
        Self {
            endpoint: endpoint.into(),
            protocol,
            timeout: Duration::from_secs(10),
            headers: Vec::new(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }
}
