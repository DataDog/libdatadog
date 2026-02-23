// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP request type for `libdd-http-client`.

use std::time::Duration;

/// Standard HTTP methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// GET
    Get,
    /// POST
    Post,
    /// PUT
    Put,
    /// DELETE
    Delete,
    /// HEAD
    Head,
    /// PATCH
    Patch,
    /// OPTIONS
    Options,
}

/// An outgoing HTTP request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: HttpMethod,

    /// Absolute URL string (e.g. `"http://localhost:8080/v0.4/traces"`).
    pub url: String,

    /// Request headers as a list of (name, value) pairs.
    ///
    /// Vec preserves insertion order and allows duplicate header names,
    /// both of which are valid in HTTP.
    pub headers: Vec<(String, String)>,

    /// Request body bytes. Empty for requests with no body.
    pub body: bytes::Bytes,

    /// Per-request timeout. Overrides the client-level timeout if set.
    pub timeout: Option<Duration>,
}

impl HttpRequest {
    /// Create a new request with the given method and URL, no headers, no body,
    /// and no per-request timeout override.
    pub fn new(method: HttpMethod, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: bytes::Bytes::new(),
            timeout: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_request_defaults() {
        let req = HttpRequest::new(HttpMethod::Get, "http://localhost/info".to_owned());
        assert_eq!(req.method, HttpMethod::Get);
        assert_eq!(req.url, "http://localhost/info");
        assert!(req.headers.is_empty());
        assert!(req.body.is_empty());
        assert!(req.timeout.is_none());
    }

    #[test]
    fn request_with_headers_and_body() {
        let mut req = HttpRequest::new(HttpMethod::Post, "http://localhost/data".to_owned());
        req.headers
            .push(("Content-Type".to_string(), "application/json".to_owned()));
        req.body = bytes::Bytes::from_static(b"{\"key\":\"value\"}");
        req.timeout = Some(Duration::from_secs(10));

        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.body.len(), 15);
        assert_eq!(req.timeout, Some(Duration::from_secs(10)));
    }

    #[test]
    fn http_method_equality() {
        assert_eq!(HttpMethod::Get, HttpMethod::Get);
        assert_ne!(HttpMethod::Get, HttpMethod::Post);
    }
}
