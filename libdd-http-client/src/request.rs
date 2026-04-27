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
    pub(crate) name: String,
    /// The part's data.
    pub(crate) data: bytes::Bytes,
    /// Optional filename for this part.
    pub(crate) filename: Option<String>,
    /// Optional MIME content type (e.g. `"application/json"`).
    pub(crate) content_type: Option<String>,
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
    #[inline]
    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the MIME content type for this part.
    #[inline]
    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// Returns the field name for this part.
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the raw bytes of this part.
    #[inline]
    pub fn data(&self) -> &bytes::Bytes {
        &self.data
    }

    /// Returns the filename, if any.
    #[inline]
    pub fn filename(&self) -> Option<&str> {
        self.filename.as_deref()
    }

    /// Returns the MIME content type, if any.
    #[inline]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
}

/// An outgoing HTTP request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP method.
    pub(crate) method: HttpMethod,

    /// Absolute URL string (e.g. `"http://localhost:8080/v0.4/traces"`).
    pub(crate) url: String,

    /// Request headers as a list of (name, value) pairs.
    ///
    /// Vec preserves insertion order and allows duplicate header names,
    /// both of which are valid in HTTP.
    pub(crate) headers: Vec<(String, String)>,

    /// Request body bytes. Empty for requests with no body.
    pub(crate) body: bytes::Bytes,

    /// Per-request timeout. Overrides the client-level timeout if set.
    pub(crate) timeout: Option<Duration>,

    /// Multipart form-data parts. When non-empty, the request is sent as
    /// multipart/form-data and `body` is ignored.
    pub(crate) multipart_parts: Vec<MultipartPart>,
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

    /// Append a header to this request.
    #[inline]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Append headers to this request.
    #[inline]
    pub fn with_headers<'a, K, V>(mut self, it: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.headers
            .extend(it.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Set the request body.
    #[inline]
    pub fn with_body(mut self, body: impl Into<bytes::Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Set the per-request timeout, overriding the client-level timeout.
    #[inline]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Append a multipart part to this request.
    #[inline]
    pub fn with_multipart_part(mut self, part: MultipartPart) -> Self {
        self.multipart_parts.push(part);
        self
    }

    /// Returns the HTTP method.
    #[inline]
    pub fn method(&self) -> HttpMethod {
        self.method
    }

    /// Returns the request URL.
    #[inline]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the request headers.
    #[inline]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Returns the request body.
    #[inline]
    pub fn body(&self) -> &bytes::Bytes {
        &self.body
    }

    /// Returns the per-request timeout override, if set.
    #[inline]
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Returns the multipart parts.
    #[inline]
    pub fn multipart_parts(&self) -> &[MultipartPart] {
        &self.multipart_parts
    }

    /// Returns a mutable reference to the headers list.
    #[inline]
    pub fn headers_mut(&mut self) -> &mut Vec<(String, String)> {
        &mut self.headers
    }

    /// Returns a mutable reference to the request body.
    #[inline]
    pub fn body_mut(&mut self) -> &mut bytes::Bytes {
        &mut self.body
    }

    /// Returns a mutable reference to the multipart parts list.
    #[inline]
    pub fn multipart_parts_mut(&mut self) -> &mut Vec<MultipartPart> {
        &mut self.multipart_parts
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
            .push(("Content-Type".to_owned(), "application/json".to_owned()));
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

    #[test]
    fn test_multipart_part_builder() {
        let part = MultipartPart::new("name", bytes::Bytes::from_static(b"data"))
            .with_filename("test.txt")
            .with_content_type("text/plain");

        assert_eq!(part.name, "name");
        assert_eq!(part.data.as_ref(), b"data");
        assert_eq!(part.filename.as_deref(), Some("test.txt"));
        assert_eq!(part.content_type.as_deref(), Some("text/plain"));
    }
}
