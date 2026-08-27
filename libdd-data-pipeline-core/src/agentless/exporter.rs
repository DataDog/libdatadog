// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless HTTP/JSON trace exporter.

use super::config::AgentlessTraceConfig;
use http::HeaderMap;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, CompressionStrategy, RetryBackoffType, RetryStrategy, SendWithRetryError,
};
use libdd_trace_utils::span::{trace_utils::compute_top_level_span, TraceData};
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use thiserror::Error;

const AGENTLESS_MAX_RETRIES: u32 = 2;
const AGENTLESS_RETRY_DELAY_MS: u64 = 1000;

/// An error encountered while sending agentless traces.
#[derive(Debug, Error)]
pub enum AgentlessError {
    /// The decoded traces could not be serialized as agentless JSON.
    #[error("failed to encode agentless JSON: {0}")]
    Serialization(#[source] serde_json::Error),
    /// The configured endpoint was not a valid URI.
    #[error("invalid agentless endpoint URL: {0}")]
    InvalidEndpoint(String),
    /// The API key cannot be represented as an HTTP header.
    #[error("invalid Datadog API key header value")]
    InvalidApiKey,
    /// Sending failed after applying the retry strategy.
    #[error("failed to send agentless traces: {0}")]
    Send(#[source] Box<SendWithRetryError>),
}

/// Encodes and sends already-decoded v0.4 traces.
///
/// When `suppress_compute_stats` is `true`, the encoder will **not** inject
/// `meta["_dd.compute_stats"]="1"` on the first span of each chunk. Set this when
/// the caller is already computing and exporting stats locally so that the intake
/// does not double-count the same traces.
pub async fn send_agentless_traces<C, T>(
    capabilities: &C,
    mut traces: Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    metadata: &TracerMetadata,
    config: &AgentlessTraceConfig,
    suppress_compute_stats: bool,
) -> Result<(), AgentlessError>
where
    C: HttpClientCapability + SleepCapability,
    T: TraceData,
{
    if !metadata.client_computed_top_level {
        for chunk in &mut traces {
            compute_top_level_span(chunk);
        }
    }
    for chunk in &mut traces {
        for span in chunk.iter_mut() {
            span.dedup();
        }
    }
    let trace_count = traces.len();
    let json_body = libdd_trace_utils::agentless_encoder::encode_payload(
        &traces,
        metadata,
        suppress_compute_stats,
    )
    .map_err(AgentlessError::Serialization)?;
    let headers = build_agentless_headers(metadata, trace_count);
    send_agentless_json(capabilities, config, headers, json_body).await
}

/// Sends an encoded agentless JSON request.
///
/// The configured API key replaces any `dd-api-key` value in `headers`.
async fn send_agentless_json<C>(
    capabilities: &C,
    config: &AgentlessTraceConfig,
    mut headers: HeaderMap,
    json_body: Vec<u8>,
) -> Result<(), AgentlessError>
where
    C: HttpClientCapability + SleepCapability,
{
    let api_key =
        http::HeaderValue::from_str(&config.api_key).map_err(|_| AgentlessError::InvalidApiKey)?;
    headers.insert(http::HeaderName::from_static("dd-api-key"), api_key);

    let url = libdd_common::parse_uri(&config.endpoint_url)
        .map_err(|error| AgentlessError::InvalidEndpoint(error.to_string()))?;
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

    send_with_retry(
        capabilities,
        &target,
        json_body,
        &headers,
        &retry_strategy,
        compression_strategy,
    )
    .await
    .map(|_| ())
    .map_err(|error| AgentlessError::Send(Box::new(error)))
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
    use bytes::Bytes;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::span::v04::SpanBytes;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[derive(Clone, Debug, Default)]
    struct TestCapabilities {
        requests: Arc<Mutex<Vec<http::Request<Bytes>>>>,
    }

    impl HttpClientCapability for TestCapabilities {
        fn new_client() -> Self {
            Self::default()
        }

        fn new_without_connection_pooling() -> Self {
            Self::default()
        }

        async fn request(
            &self,
            request: http::Request<Bytes>,
        ) -> Result<http::Response<Bytes>, libdd_capabilities::HttpError> {
            self.requests.lock().unwrap().push(request);
            Ok(http::Response::builder()
                .status(http::StatusCode::ACCEPTED)
                .body(Bytes::new())
                .unwrap())
        }
    }

    impl SleepCapability for TestCapabilities {
        fn new() -> Self {
            Self::default()
        }

        async fn sleep(&self, _duration: Duration) {}
    }

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

    fn request_body(request: &http::Request<Bytes>) -> Vec<u8> {
        #[cfg(feature = "compression")]
        let body = zstd::decode_all(request.body().as_ref()).unwrap();
        #[cfg(not(feature = "compression"))]
        let body = request.body().to_vec();
        body
    }

    #[test]
    fn sends_decoded_traces_with_top_level_tags() {
        let capabilities = TestCapabilities::default();
        let result = futures::executor::block_on(send_agentless_traces(
            &capabilities,
            v04_traces(),
            &metadata(),
            &config(),
            false,
        ));
        assert!(result.is_ok());

        let requests = capabilities.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.uri(), "https://example.test/v1/input");
        assert_eq!(request.headers()["dd-api-key"], "test-api-key");
        assert_eq!(request.headers()["x-datadog-trace-count"], "1");
        assert!(String::from_utf8(request_body(request))
            .unwrap()
            .contains("\"_top_level\":1"));
    }

    #[test]
    fn json_request_uses_configured_api_key() {
        let capabilities = TestCapabilities::default();
        let mut headers = HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static("dd-api-key"),
            http::HeaderValue::from_static("caller-supplied-key"),
        );

        let result = futures::executor::block_on(send_agentless_json(
            &capabilities,
            &config(),
            headers,
            b"[]".to_vec(),
        ));
        assert!(result.is_ok());
        let requests = capabilities.requests.lock().unwrap();
        assert_eq!(requests[0].headers()["dd-api-key"], "test-api-key");
    }

    #[test]
    fn json_request_rejects_invalid_api_key() {
        let capabilities = TestCapabilities::default();
        let mut invalid_config = config();
        invalid_config.api_key = "invalid\nkey".to_string();

        let error = futures::executor::block_on(send_agentless_json(
            &capabilities,
            &invalid_config,
            HeaderMap::new(),
            b"[]".to_vec(),
        ))
        .unwrap_err();
        assert!(matches!(error, AgentlessError::InvalidApiKey));
    }

    #[test]
    fn debug_output_redacts_agentless_secrets() {
        assert!(!format!("{:?}", config()).contains("test-api-key"));
    }
}
