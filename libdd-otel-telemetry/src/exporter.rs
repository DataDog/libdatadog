// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::Ordering;

use http::HeaderMap;
use libdd_capabilities::HttpClientCapability;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    CompressionStrategy, RetryBackoffType, RetryStrategy, send_with_retry,
};

use crate::ExportCounters;
use crate::aggregator::Counters;
use crate::config::{OtlpExporterConfig, OtlpProtocol, Temporality};
use crate::error::{BuildWarning, OtelMetricsError};

const MAX_RETRIES: u32 = 4;
const RETRY_DELAY_MS: u64 = 100;

/// OTLP/HTTP protobuf exporter built entirely from libdatadog transport components.
#[derive(Debug)]
pub struct DatadogMetricExporter {
    endpoint: Endpoint,
    headers: HeaderMap,
    capabilities: NativeCapabilities,
    counters: Arc<Counters>,
}

impl DatadogMetricExporter {
    pub fn counters(&self) -> ExportCounters {
        self.counters.snapshot()
    }

    pub(crate) fn counters_handle(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }

    pub(crate) async fn export(&self, payload: Vec<u8>) -> Result<(), OtelMetricsError> {
        self.counters.attempts.fetch_add(1, Ordering::Relaxed);
        let retry_strategy = RetryStrategy::new(
            MAX_RETRIES,
            RETRY_DELAY_MS,
            RetryBackoffType::Exponential,
            None,
        );
        let result = send_with_retry(
            &self.capabilities,
            &self.endpoint,
            payload,
            &self.headers,
            &retry_strategy,
            CompressionStrategy::None,
        )
        .await
        .map(|_| ())
        .map_err(|error| OtelMetricsError(error.to_string()));
        match result {
            Ok(()) => self.counters.successes.fetch_add(1, Ordering::Relaxed),
            Err(_) => self.counters.failures.fetch_add(1, Ordering::Relaxed),
        };
        result
    }
}

/// Builds the native OTLP metrics exporter.
///
/// The temporality argument is retained for source compatibility. Aggregation temporality is
/// applied before encoding rather than by the transport.
pub fn build_datadog_metric_exporter(
    config: &OtlpExporterConfig,
    _temporality: Temporality,
) -> Result<DatadogMetricExporter, BuildWarning> {
    match config.protocol {
        OtlpProtocol::HttpProtobuf => {}
        OtlpProtocol::Grpc => {
            return Err(BuildWarning::UnsupportedProtocol(
                "the native gRPC transport has not been generalized beyond traces yet".to_owned(),
            ));
        }
        OtlpProtocol::HttpJson => {
            return Err(BuildWarning::UnsupportedProtocol(
                "http/json protocol is not supported for OTLP metrics export".to_owned(),
            ));
        }
    }

    #[cfg(not(feature = "http"))]
    return Err(BuildWarning::UnsupportedProtocol(
        "http/protobuf protocol requires the 'http' feature".to_owned(),
    ));

    #[cfg(feature = "http")]
    {
        let url = libdd_common::parse_uri(&config.endpoint)
            .map_err(|error| BuildWarning::InvalidEndpoint(error.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            libdd_common::header::APPLICATION_PROTOBUF,
        );
        for (key, value) in &config.headers {
            let name = http::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|error| BuildWarning::InvalidHeader(error.to_string()))?;
            let value = http::header::HeaderValue::from_str(value)
                .map_err(|error| BuildWarning::InvalidHeader(error.to_string()))?;
            headers.insert(name, value);
        }
        Ok(DatadogMetricExporter {
            endpoint: Endpoint {
                url,
                timeout_ms: u64::try_from(config.timeout.as_millis()).unwrap_or(u64::MAX),
                ..Endpoint::default()
            },
            headers,
            capabilities: NativeCapabilities::new_periodic(),
            counters: Arc::new(Counters::default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_reports_the_remaining_transport_gap() {
        let result = build_datadog_metric_exporter(
            &OtlpExporterConfig::new("http://127.0.0.1:4317", OtlpProtocol::Grpc),
            Temporality::Delta,
        );
        assert!(matches!(result, Err(BuildWarning::UnsupportedProtocol(_))));
    }

    #[test]
    fn invalid_http_headers_are_rejected() {
        let result = build_datadog_metric_exporter(
            &OtlpExporterConfig::new(
                "http://127.0.0.1:4318/v1/metrics",
                OtlpProtocol::HttpProtobuf,
            )
            .with_header("invalid header", "value"),
            Temporality::Delta,
        );
        assert!(matches!(result, Err(BuildWarning::InvalidHeader(_))));
    }
}
