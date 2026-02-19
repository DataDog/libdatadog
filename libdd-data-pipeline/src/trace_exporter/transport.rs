// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::trace_exporter::TracerMetadata;
use bytes::Bytes;
use hyper::Uri;
use std::collections::HashMap;

/// Transport client for trace exporter operations
///
/// This struct is responsible for building HTTP requests for trace data.
/// Response handling and metric emission are handled by TraceExporter.
pub(super) struct TransportClient<'a> {
    metadata: &'a TracerMetadata,
}

impl<'a> TransportClient<'a> {
    /// Create a new transport client
    pub(super) fn new(metadata: &'a TracerMetadata) -> Self {
        Self { metadata }
    }

    /// Build HTTP request for sending trace data to agent
    pub(super) fn build_trace_request(
        &self,
        data: &[u8],
        trace_count: usize,
        uri: Uri,
    ) -> Result<http::Request<Bytes>, http::Error> {
        let headers: HashMap<&'static str, String> = self.metadata.into();

        let mut builder = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header("user-agent", concat!("Tracer/", env!("CARGO_PKG_VERSION")))
            .header("content-type", "application/msgpack")
            .header("X-Datadog-Trace-Count", trace_count.to_string());

        for (key, value) in &headers {
            builder = builder.header(*key, value.as_str());
        }

        builder.body(Bytes::from(data.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::TracerMetadata;

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
        let client = TransportClient::new(&metadata);

        assert_eq!(client.metadata.service, "test-service");
    }

    #[test]
    fn test_build_trace_request() {
        let metadata = create_test_metadata();
        let client = TransportClient::new(&metadata);
        let uri = "http://localhost:8126/v0.4/traces".parse().unwrap();
        let data = b"test payload";
        let trace_count = 5;

        let request = client.build_trace_request(data, trace_count, uri).unwrap();

        assert_eq!(request.method(), http::Method::POST);
        assert!(request.uri().to_string().contains("/v0.4/traces"));

        let headers = request.headers();
        let find_header = |name: &str| -> Option<&str> {
            headers.get(name).and_then(|v| v.to_str().ok())
        };

        assert_eq!(find_header("content-type"), Some("application/msgpack"));
        assert_eq!(find_header("x-datadog-trace-count"), Some("5"));
        assert!(find_header("user-agent").unwrap().starts_with("Tracer/"));

        assert!(find_header("datadog-meta-lang").is_some());
        assert_eq!(find_header("datadog-meta-lang"), Some("rust"));
        assert!(find_header("datadog-meta-tracer-version").is_some());
        assert_eq!(find_header("datadog-meta-tracer-version"), Some("1.0.0"));
    }

    #[test]
    fn test_request_headers_metadata_integration() {
        let mut metadata = create_test_metadata();
        metadata.language = "python".to_string();
        metadata.tracer_version = "2.0.0".to_string();

        let client = TransportClient::new(&metadata);
        let uri = "http://localhost:8126/v0.4/traces".parse().unwrap();
        let data = b"test";

        let request = client.build_trace_request(data, 1, uri).unwrap();

        let headers = request.headers();
        let find_header = |name: &str| -> Option<&str> {
            headers.get(name).and_then(|v| v.to_str().ok())
        };

        assert_eq!(find_header("datadog-meta-lang"), Some("python"));
        assert_eq!(find_header("datadog-meta-tracer-version"), Some("2.0.0"));
    }
}
