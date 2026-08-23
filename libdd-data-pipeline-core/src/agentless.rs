// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{fmt, time::Duration};

use http::{HeaderMap, StatusCode};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    CompressionStrategy, PreparedRequest, RetryBackoffType, RetryStrategy,
};
use libdd_trace_utils::span::{trace_utils::compute_top_level_span, TraceData};
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use thiserror::Error;

const AGENTLESS_MAX_RETRIES: u32 = 2;
const AGENTLESS_RETRY_DELAY_MS: u64 = 1000;

/// Default timeout for one agentless request attempt.
pub const DEFAULT_AGENTLESS_TIMEOUT: Duration = Duration::from_secs(15);

/// Agentless trace request configuration.
#[derive(Clone)]
pub struct AgentlessTraceConfig {
    /// Full URL to POST traces to.
    pub endpoint_url: String,
    /// Datadog API key used for the `dd-api-key` header.
    pub api_key: String,
    /// Timeout for one request attempt.
    pub timeout: Duration,
}

impl fmt::Debug for AgentlessTraceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentlessTraceConfig")
            .field("endpoint_url", &self.endpoint_url)
            .field("api_key", &"<redacted>")
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// An error encountered while preparing an agentless request.
#[derive(Debug, Error)]
pub enum PrepareAgentlessError {
    /// The v0.4 MessagePack input could not be decoded.
    #[error("failed to decode v0.4 traces: {0}")]
    Deserialization(libdd_trace_utils::msgpack_decoder::decode::error::DecodeError),
    /// The decoded traces could not be serialized as agentless JSON.
    #[error("failed to encode agentless JSON: {0}")]
    Serialization(#[source] serde_json::Error),
    /// The configured endpoint was not a valid URI.
    #[error("invalid agentless endpoint URL: {0}")]
    InvalidEndpoint(String),
    /// The API key cannot be represented as an HTTP header.
    #[error("invalid Datadog API key header value")]
    InvalidApiKey,
}

/// A fully encoded agentless request for transport by the host runtime.
#[derive(Clone, Debug)]
pub struct PreparedAgentlessRequest {
    request: PreparedRequest,
    retry_strategy: RetryStrategy,
}

impl PreparedAgentlessRequest {
    /// Returns the reusable request plan.
    pub fn request_plan(&self) -> &PreparedRequest {
        &self.request
    }

    /// Returns the retry strategy associated with the request.
    pub fn retry_strategy(&self) -> &RetryStrategy {
        &self.retry_strategy
    }

    /// Builds one HTTP request attempt.
    pub fn request(&self) -> Result<http::Request<bytes::Bytes>, http::Error> {
        self.request.request()
    }

    /// Returns the timeout for one HTTP request attempt.
    pub fn timeout(&self) -> Duration {
        self.request.timeout()
    }

    /// Returns the maximum number of retries after the initial attempt.
    pub fn max_retries(&self) -> u32 {
        self.retry_strategy.max_retries()
    }

    /// Returns whether an HTTP response is retryable.
    pub fn is_retryable_status(status: StatusCode) -> bool {
        RetryStrategy::is_retryable_status(status)
    }

    /// Returns the delay before retrying after the one-indexed `attempt`.
    pub fn retry_delay(&self, attempt: u32) -> Option<Duration> {
        self.retry_strategy.retry_delay(attempt)
    }
}

/// Converts v0.4 MessagePack traces into a host-transported request.
pub fn prepare_agentless_v04_request(
    data: &[u8],
    metadata: &TracerMetadata,
    config: &AgentlessTraceConfig,
) -> Result<PreparedAgentlessRequest, PrepareAgentlessError> {
    let (traces, _) = libdd_trace_utils::msgpack_decoder::v04::from_slice(data)
        .map_err(PrepareAgentlessError::Deserialization)?;
    prepare_agentless_traces_request(traces, metadata, config)
}

/// Prepares already-decoded v0.4 traces for host transport.
pub fn prepare_agentless_traces_request<T: TraceData>(
    mut traces: Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    metadata: &TracerMetadata,
    config: &AgentlessTraceConfig,
) -> Result<PreparedAgentlessRequest, PrepareAgentlessError> {
    if !metadata.client_computed_top_level {
        for chunk in &mut traces {
            compute_top_level_span(chunk);
        }
    }
    let trace_count = traces.len();
    let json_body = libdd_trace_utils::agentless_encoder::encode_payload(&traces, metadata)
        .map_err(PrepareAgentlessError::Serialization)?;
    let headers = build_agentless_headers(metadata, trace_count);
    prepare_agentless_json_request(config, headers, json_body)
}

/// Prepares an encoded agentless JSON request for host transport.
///
/// The configured API key replaces any `dd-api-key` value in `headers`.
pub fn prepare_agentless_json_request(
    config: &AgentlessTraceConfig,
    mut headers: HeaderMap,
    json_body: Vec<u8>,
) -> Result<PreparedAgentlessRequest, PrepareAgentlessError> {
    let api_key = http::HeaderValue::from_str(&config.api_key)
        .map_err(|_| PrepareAgentlessError::InvalidApiKey)?;
    headers.insert(http::HeaderName::from_static("dd-api-key"), api_key);

    let url = libdd_common::parse_uri(&config.endpoint_url)
        .map_err(|error| PrepareAgentlessError::InvalidEndpoint(error.to_string()))?;
    let target = Endpoint {
        url,
        timeout_ms: u64::try_from(config.timeout.as_millis()).unwrap_or(u64::MAX),
        ..Endpoint::default()
    };
    let retry_strategy = RetryStrategy::new(
        AGENTLESS_MAX_RETRIES,
        AGENTLESS_RETRY_DELAY_MS,
        RetryBackoffType::Exponential,
        None,
    );

    #[cfg(feature = "compression")]
    let compression_strategy = CompressionStrategy::Zstd { level: 1 };
    #[cfg(not(feature = "compression"))]
    let compression_strategy = CompressionStrategy::None;

    Ok(PreparedAgentlessRequest {
        request: PreparedRequest::new(target, json_body, headers, compression_strategy),
        retry_strategy,
    })
}

fn build_agentless_headers(metadata: &TracerMetadata, trace_count: usize) -> HeaderMap {
    let mut headers: HeaderMap = metadata.into();
    headers.insert(
        http::header::CONTENT_TYPE,
        libdd_common::header::APPLICATION_JSON,
    );
    headers.insert(
        http::HeaderName::from_static("x-datadog-trace-count"),
        http::HeaderValue::from(trace_count),
    );
    for (name, value) in libdd_common::entity_id::get_entity_headers() {
        if let (Ok(name), Ok(value)) = (
            http::HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::msgpack_encoder;
    use libdd_trace_utils::span::v04::SpanBytes;

    fn metadata() -> TracerMetadata {
        TracerMetadata {
            hostname: "host-1".to_string(),
            service: "service-1".to_string(),
            tracer_version: "1.2.3".to_string(),
            language: "nodejs".to_string(),
            language_version: "v24".to_string(),
            language_interpreter: "v8".to_string(),
            ..Default::default()
        }
    }

    fn config() -> AgentlessTraceConfig {
        AgentlessTraceConfig {
            endpoint_url: "https://example.test/v1/input".to_string(),
            api_key: "test-api-key".to_string(),
            timeout: Duration::from_millis(1_234),
        }
    }

    fn v04_traces() -> Vec<Vec<SpanBytes>> {
        vec![vec![SpanBytes {
            name: BytesString::from_static("operation"),
            service: BytesString::from_static("service-1"),
            resource: BytesString::from_static("resource-1"),
            trace_id: 1,
            span_id: 2,
            start: 1,
            duration: 2,
            ..Default::default()
        }]]
    }

    fn v04_payload() -> Vec<u8> {
        msgpack_encoder::v04::to_vec_from_v04(&v04_traces())
    }

    fn request_body(prepared: &PreparedAgentlessRequest) -> Vec<u8> {
        let request = prepared.request().unwrap();
        #[cfg(feature = "compression")]
        let body = zstd::decode_all(request.body().as_ref()).unwrap();
        #[cfg(not(feature = "compression"))]
        let body = request.body().to_vec();
        body
    }

    #[test]
    fn prepares_v04_request_without_runtime() {
        let prepared =
            prepare_agentless_v04_request(&v04_payload(), &metadata(), &config()).unwrap();
        let request = prepared.request().unwrap();

        assert_eq!(request.uri(), "https://example.test/v1/input");
        assert_eq!(request.headers()["dd-api-key"], "test-api-key");
        assert_eq!(request.headers()["x-datadog-trace-count"], "1");
        assert_eq!(prepared.timeout(), Duration::from_millis(1_234));
        assert_eq!(prepared.retry_delay(1), Some(Duration::from_secs(1)));
        assert_eq!(prepared.retry_delay(2), Some(Duration::from_secs(2)));
        assert_eq!(prepared.retry_delay(3), None);

        assert!(String::from_utf8(request_body(&prepared))
            .unwrap()
            .contains("\"_top_level\":1"));
    }

    #[test]
    fn prepares_decoded_traces_with_top_level_tags() {
        let mut metadata = metadata();
        metadata.client_computed_top_level = false;
        let prepared =
            prepare_agentless_traces_request(v04_traces(), &metadata, &config()).unwrap();

        assert!(String::from_utf8(request_body(&prepared))
            .unwrap()
            .contains("\"_top_level\":1"));
    }

    #[test]
    fn json_request_uses_configured_api_key() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static("dd-api-key"),
            http::HeaderValue::from_static("caller-supplied-key"),
        );

        let prepared = prepare_agentless_json_request(&config(), headers, b"[]".to_vec()).unwrap();
        assert_eq!(
            prepared.request().unwrap().headers()["dd-api-key"],
            "test-api-key"
        );
    }

    #[test]
    fn json_request_rejects_invalid_api_key() {
        let mut invalid_config = config();
        invalid_config.api_key = "invalid\nkey".to_string();

        let error =
            prepare_agentless_json_request(&invalid_config, HeaderMap::new(), b"[]".to_vec())
                .unwrap_err();
        assert!(matches!(error, PrepareAgentlessError::InvalidApiKey));
    }

    #[test]
    fn debug_output_redacts_agentless_secrets() {
        let config = config();
        assert!(!format!("{config:?}").contains("test-api-key"));

        let prepared = prepare_agentless_v04_request(&v04_payload(), &metadata(), &config).unwrap();
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("test-api-key"));
        assert!(!debug.contains("operation"));
        assert!(!debug.contains("resource-1"));
    }
}
