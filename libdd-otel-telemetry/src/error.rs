// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

/// A non-fatal problem encountered while building a [`crate::TelemetryAggregator`].
///
/// The aggregator is always usable after `build()` — on any of these conditions it falls back to
/// a no-op internal state (recorded values are dropped) rather than failing construction, so a
/// misconfigured exporter never prevents the host tracer from starting up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildWarning {
    /// The configured OTLP endpoint could not be parsed as a valid URL.
    InvalidEndpoint(String),
    /// The requested wire protocol is not supported (e.g. `http/json`).
    UnsupportedProtocol(String),
    /// The underlying `opentelemetry-otlp` exporter failed to build.
    ExporterInitFailed(String),
}

impl fmt::Display for BuildWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildWarning::InvalidEndpoint(msg) => write!(f, "invalid OTLP endpoint: {msg}"),
            BuildWarning::UnsupportedProtocol(msg) => write!(f, "unsupported OTLP protocol: {msg}"),
            BuildWarning::ExporterInitFailed(msg) => {
                write!(f, "failed to initialize OTLP exporter: {msg}")
            }
        }
    }
}

/// Error returned from lifecycle operations (`force_flush`/`shutdown`) on a
/// [`crate::TelemetryAggregator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryAggregatorError(pub(crate) String);

impl fmt::Display for TelemetryAggregatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TelemetryAggregatorError {}
