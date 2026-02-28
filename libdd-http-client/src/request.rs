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

/// A single part in a multipart form-data request.
#[derive(Debug, Clone)]
pub struct MultipartPart {
    /// The field name for this part.
    pub name: String,
    /// The part's data.
    pub data: bytes::Bytes,
    /// Optional filename for this part.
    pub filename: Option<String>,
    /// Optional MIME content type (e.g. `"application/json"`).
    pub content_type: Option<String>,
}

impl MultipartPart {
    /// Create a new multipart part with the given field name and data.
    pub fn new(name: impl Into<String>, data: impl Into<bytes::Bytes>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
            filename: None,
            content_type: None,
        }
    }

    /// Set the filename for this part.
    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the MIME content type for this part.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }
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

    /// Multipart form-data parts. When non-empty, the request is sent as
    /// multipart/form-data and `body` is ignored.
    pub multipart_parts: Vec<MultipartPart>,
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
            multipart_parts: Vec::new(),
        }
    }

    /// Add a multipart part to this request. When any parts are present, the
    /// request is sent as multipart/form-data and `body` is ignored.
    pub fn add_multipart_part(&mut self, part: MultipartPart) {
        self.multipart_parts.push(part);
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
