// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::health_metrics::{self, HealthMetric};
use crate::trace_exporter::agent_response::AgentResponse;
use crate::trace_exporter::error::{RequestError, TraceExporterError};
use crate::trace_exporter::metrics::MetricsEmitter;
use crate::trace_exporter::TracerMetadata;
use bytes::Bytes;
use ddcommon::hyper_migration;
use ddcommon::{tag, tag::Tag};
use http_body_util::BodyExt;
use hyper::{Method, Uri};
use std::collections::HashMap;
use tracing::{error, info, warn};

/// Transport client for trace exporter operations
pub(super) struct TransportClient<'a> {
    metadata: &'a TracerMetadata,
    health_metrics_enabled: bool,
    dogstatsd: Option<&'a dogstatsd_client::Client>,
    common_stats_tags: &'a [Tag],
}

impl<'a> TransportClient<'a> {
    /// Create a new transport client
    pub(super) fn new(
        metadata: &'a TracerMetadata,
        health_metrics_enabled: bool,
        dogstatsd: Option<&'a dogstatsd_client::Client>,
        common_stats_tags: &'a [Tag],
    ) -> Self {
        Self {
            metadata,
            health_metrics_enabled,
            dogstatsd,
            common_stats_tags,
        }
    }

    /// Build HTTP request for sending trace data to agent
    pub(super) fn build_trace_request(
        &self,
        data: &[u8],
        trace_count: usize,
        uri: Uri,
    ) -> hyper::Request<hyper_migration::Body> {
        let mut req_builder = self.create_base_request_builder(uri);
        req_builder = self.add_metadata_headers(req_builder);
        req_builder = self.add_trace_headers(req_builder, trace_count);
        self.build_request_with_body(req_builder, data)
    }

    /// Handle HTTP error response and emit appropriate metrics
    pub(super) async fn handle_http_error_response(
        &self,
        response: hyper::Response<hyper_migration::Body>,
        payload_size: usize,
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let response_status = response.status();
        let response_body = self.extract_response_body(response).await;
        self.log_and_emit_error_metrics(response_status, payload_size, trace_count);
        Err(TraceExporterError::Request(RequestError::new(
            response_status,
            &response_body,
        )))
    }

    /// Handle successful HTTP response
    pub(super) async fn handle_http_success_response(
        &self,
        response: hyper::Response<hyper_migration::Body>,
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        match response.into_body().collect().await {
            Ok(body) => {
                info!(trace_count, "Traces sent successfully to agent");
                self.emit_metric(
                    HealthMetric::Count(
                        health_metrics::TRANSPORT_TRACES_SUCCESSFUL,
                        trace_count as i64,
                    ),
                    None,
                );
                Ok(AgentResponse::Changed {
                    body: String::from_utf8_lossy(&body.to_bytes()).to_string(),
                })
            }
            Err(err) => {
                error!(
                    error = %err,
                    "Failed to read agent response body"
                );
                let type_tag = tag!("type", "response_body");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                    Some(vec![&type_tag]),
                );
                Err(TraceExporterError::from(err))
            }
        }
    }

    /// Process HTTP response based on status code
    pub(super) async fn process_http_response(
        &self,
        response: hyper::Response<hyper_migration::Body>,
        trace_count: usize,
        payload_size: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        if !response.status().is_success() {
            self.handle_http_error_response(response, payload_size, trace_count)
                .await
        } else {
            self.handle_http_success_response(response, trace_count)
                .await
        }
    }

    /// Create base HTTP request builder with URI, user agent, and method
    fn create_base_request_builder(&self, uri: Uri) -> hyper::http::request::Builder {
        hyper::Request::builder()
            .uri(uri)
            .header(
                hyper::header::USER_AGENT,
                concat!("Tracer/", env!("CARGO_PKG_VERSION")),
            )
            .method(Method::POST)
    }

    /// Add metadata headers to the request builder
    fn add_metadata_headers(
        &self,
        mut req_builder: hyper::http::request::Builder,
    ) -> hyper::http::request::Builder {
        let headers: HashMap<&'static str, String> = self.metadata.into();
        for (key, value) in &headers {
            req_builder = req_builder.header(*key, value);
        }
        req_builder
    }

    /// Add trace-specific headers to the request builder
    fn add_trace_headers(
        &self,
        req_builder: hyper::http::request::Builder,
        trace_count: usize,
    ) -> hyper::http::request::Builder {
        req_builder
            .header("Content-type", "application/msgpack")
            .header("X-Datadog-Trace-Count", trace_count.to_string().as_str())
    }

    /// Build the final request with body
    fn build_request_with_body(
        &self,
        req_builder: hyper::http::request::Builder,
        data: &[u8],
    ) -> hyper::Request<hyper_migration::Body> {
        #[allow(clippy::unwrap_used)]
        req_builder
            .body(hyper_migration::Body::from_bytes(Bytes::copy_from_slice(
                data,
            )))
            // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
            .unwrap()
    }

    /// Extract response body from HTTP response
    async fn extract_response_body(
        &self,
        response: hyper::Response<hyper_migration::Body>,
    ) -> String {
        // TODO: Properly handle non-OK states to prevent possible panics
        // (APMSP-18190).
        #[allow(clippy::unwrap_used)]
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body_bytes.to_vec()).unwrap_or_default()
    }

    /// Log error and emit metrics for HTTP error response
    fn log_and_emit_error_metrics(
        &self,
        response_status: hyper::StatusCode,
        payload_size: usize,
        trace_count: usize,
    ) {
        let resp_tag_res = &Tag::new("response_code", response_status.as_str());
        match resp_tag_res {
            Ok(resp_tag) => {
                warn!(
                    response_code = response_status.as_u16(),
                    "HTTP error response received from agent"
                );
                let type_tag = Tag::new("type", response_status.as_str())
                    .unwrap_or_else(|_| tag!("type", "unknown"));
                self.emit_metric(
                    HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                    Some(vec![&resp_tag, &type_tag]),
                );
                if response_status.as_u16() != 404 && response_status.as_u16() != 415 {
                    self.emit_metric(
                        HealthMetric::Distribution(
                            health_metrics::TRANSPORT_DROPPED_BYTES,
                            payload_size as i64,
                        ),
                        None,
                    );
                    self.emit_metric(
                        HealthMetric::Distribution(
                            health_metrics::TRANSPORT_TRACES_DROPPED,
                            trace_count as i64,
                        ),
                        None,
                    );
                }
            }
            Err(tag_err) => {
                error!(?tag_err, "Failed to serialize response_code to tag")
            }
        }
    }

    /// Emit a health metric to dogstatsd
    fn emit_metric(&self, metric: HealthMetric, custom_tags: Option<Vec<&Tag>>) {
        if self.health_metrics_enabled {
            let emitter = MetricsEmitter::new(self.dogstatsd, self.common_stats_tags);
            emitter.emit(metric, custom_tags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::TracerMetadata;
    use bytes::Bytes;
    use ddcommon::tag;
    use hyper::{Response, StatusCode};

    fn create_test_metadata() -> TracerMetadata {
        TracerMetadata {
            hostname: "test-host".to_string(),
            env: "test".to_string(),
            app_version: "1.0.0".to_string(),
            runtime_id: "test-runtime".to_string(),
            service: "test-service".to_string(),
            tracer_version: "1.0.0".to_string(),
            language: "rust".to_string(),
            language_version: "1.70.0".to_string(),
            language_interpreter: "rustc".to_string(),
            language_interpreter_vendor: "rust-lang".to_string(),
            git_commit_sha: "abc123".to_string(),
            client_computed_stats: true,
            client_computed_top_level: false,
        }
    }

    #[test]
    fn test_transport_client_new() {
        let metadata = create_test_metadata();
        let tags = vec![tag!("env", "test")];
        let client = TransportClient::new(&metadata, true, None, &tags);

        assert!(client.health_metrics_enabled);
        assert!(client.dogstatsd.is_none());
        assert_eq!(client.common_stats_tags.len(), 1);
        assert_eq!(client.metadata.service, "test-service");
    }

    #[test]
    fn test_build_trace_request() {
        let metadata = create_test_metadata();
        let tags = vec![tag!("test", "value")];
        let client = TransportClient::new(&metadata, false, None, &tags);
        let uri = "http://localhost:8126/v0.4/traces".parse().unwrap();
        let data = b"test payload";
        let trace_count = 5;

        let request = client.build_trace_request(data, trace_count, uri);

        assert_eq!(request.method(), hyper::Method::POST);
        assert_eq!(request.uri().path(), "/v0.4/traces");

        let headers = request.headers();
        assert_eq!(headers.get("content-type").unwrap(), "application/msgpack");
        assert_eq!(headers.get("x-datadog-trace-count").unwrap(), "5");
        assert!(headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("Tracer/"));

        assert!(headers.contains_key("datadog-meta-lang"));
        assert_eq!(headers.get("datadog-meta-lang").unwrap(), "rust");
        assert!(headers.contains_key("datadog-meta-tracer-version"));
        assert_eq!(headers.get("datadog-meta-tracer-version").unwrap(), "1.0.0");
    }

    #[tokio::test]
    async fn test_handle_http_success_response() {
        let metadata = create_test_metadata();
        let tags = vec![tag!("test", "value")];
        let client = TransportClient::new(&metadata, false, None, &tags);
        let body = r#"{"rate_by_service": {"service:test": 1.0}}"#;
        let response =
            hyper_migration::mock_response(Response::builder().status(200), Bytes::from(body))
                .unwrap();

        let result = client.handle_http_success_response(response, 10).await;

        assert!(result.is_ok());
        match result.unwrap() {
            AgentResponse::Changed {
                body: response_body,
            } => {
                assert_eq!(response_body, body);
            }
            _ => panic!("Expected Changed response"),
        }
    }

    #[tokio::test]
    async fn test_handle_http_error_response() {
        let metadata = create_test_metadata();
        let tags = vec![tag!("test", "value")];
        let client = TransportClient::new(&metadata, false, None, &tags);
        let error_body = r#"{"error": "Bad Request"}"#;
        let response = hyper_migration::mock_response(
            Response::builder().status(400),
            Bytes::from(error_body),
        )
        .unwrap();

        let result = client.handle_http_error_response(response, 1024, 5).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            TraceExporterError::Request(req_err) => {
                assert_eq!(req_err.status(), StatusCode::BAD_REQUEST);
            }
            _ => panic!("Expected Request error"),
        }
    }

    #[tokio::test]
    async fn test_process_http_response_success() {
        let metadata = create_test_metadata();
        let tags = vec![tag!("test", "value")];
        let client = TransportClient::new(&metadata, false, None, &tags);
        let body = r#"{"success": true}"#;
        let response =
            hyper_migration::mock_response(Response::builder().status(200), Bytes::from(body))
                .unwrap();

        let result = client.process_http_response(response, 3, 512).await;

        assert!(result.is_ok());
        match result.unwrap() {
            AgentResponse::Changed {
                body: response_body,
            } => {
                assert_eq!(response_body, body);
            }
            _ => panic!("Expected Changed response"),
        }
    }

    #[tokio::test]
    async fn test_process_http_response_error() {
        let metadata = create_test_metadata();
        let tags = vec![tag!("test", "value")];
        let client = TransportClient::new(&metadata, false, None, &tags);
        let error_body = r#"{"error": "Internal Server Error"}"#;
        let response = hyper_migration::mock_response(
            Response::builder().status(500),
            Bytes::from(error_body),
        )
        .unwrap();

        let result = client.process_http_response(response, 2, 256).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            TraceExporterError::Request(req_err) => {
                assert_eq!(req_err.status(), StatusCode::INTERNAL_SERVER_ERROR);
            }
            _ => panic!("Expected Request error"),
        }
    }

    #[test]
    fn test_request_headers_metadata_integration() {
        let mut metadata = create_test_metadata();
        metadata.language = "python".to_string();
        metadata.tracer_version = "2.0.0".to_string();

        let tags = vec![tag!("region", "us-east-1")];
        let client = TransportClient::new(&metadata, false, None, &tags);
        let uri = "http://localhost:8126/v0.4/traces".parse().unwrap();
        let data = b"test";

        let request = client.build_trace_request(data, 1, uri);
        let headers = request.headers();

        assert_eq!(headers.get("datadog-meta-lang").unwrap(), "python");
        assert_eq!(headers.get("datadog-meta-tracer-version").unwrap(), "2.0.0");
    }
}
