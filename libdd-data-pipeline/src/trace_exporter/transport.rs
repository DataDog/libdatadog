// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::trace_exporter::TracerMetadata;
use bytes::Bytes;
use hyper::{Method, Uri};
use libdd_common::hyper_migration;
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
    ) -> hyper::Request<hyper_migration::Body> {
        let mut req_builder = self.create_base_request_builder(uri);
        req_builder = self.add_metadata_headers(req_builder);
        req_builder = self.add_trace_headers(req_builder, trace_count);
        self.build_request_with_body(req_builder, data)
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

    #[test]
    fn test_request_headers_metadata_integration() {
        let mut metadata = create_test_metadata();
        metadata.language = "python".to_string();
        metadata.tracer_version = "2.0.0".to_string();

        let client = TransportClient::new(&metadata);
        let uri = "http://localhost:8126/v0.4/traces".parse().unwrap();
        let data = b"test";

        let request = client.build_trace_request(data, 1, uri);
        let headers = request.headers();

        assert_eq!(headers.get("datadog-meta-lang").unwrap(), "python");
        assert_eq!(headers.get("datadog-meta-tracer-version").unwrap(), "2.0.0");
    }
}
