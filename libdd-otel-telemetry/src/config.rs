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
}

/// Aggregation temporality preference for metrics export.
///
/// Re-exported as a crate-local type (rather than requiring consumers to depend on
/// `opentelemetry_sdk` directly) so no upstream SDK type ever needs to appear in a consumer's
/// public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Temporality {
    #[default]
    Delta,
    Cumulative,
}

impl From<Temporality> for opentelemetry_sdk::metrics::Temporality {
    fn from(value: Temporality) -> Self {
        match value {
            Temporality::Delta => opentelemetry_sdk::metrics::Temporality::Delta,
            Temporality::Cumulative => opentelemetry_sdk::metrics::Temporality::Cumulative,
        }
    }
}

/// Configuration for a single OTLP exporter (metrics or logs).
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
