// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP capability trait and types.

use crate::maybe_send::MaybeSend;
use core::fmt;
use core::future::Future;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Look up a response header by name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum HttpError {
    Network(String),
    Timeout,
    InvalidRequest(String),
    Other(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Network(msg) => write!(f, "Network error: {}", msg),
            HttpError::Timeout => write!(f, "Request timed out"),
            HttpError::InvalidRequest(msg) => write!(f, "Invalid request: {}", msg),
            HttpError::Other(msg) => write!(f, "HTTP error: {}", msg),
        }
    }
}

impl std::error::Error for HttpError {}

/// Request without body (GET, HEAD, DELETE, OPTIONS).
#[derive(Debug, Clone, Default)]
pub struct RequestHead {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl RequestHead {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Request with body (POST, PUT, PATCH).
#[derive(Debug, Clone)]
pub struct RequestWithBody {
    pub head: RequestHead,
    pub body: Vec<u8>,
}

impl RequestWithBody {
    pub fn new(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            head: RequestHead::new(url),
            body,
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.head = self.head.with_header(name, value);
        self
    }
}

#[derive(Debug, Clone)]
pub enum HttpRequest {
    Get(RequestHead),
    Head(RequestHead),
    Delete(RequestHead),
    Options(RequestHead),
    Post(RequestWithBody),
    Put(RequestWithBody),
    Patch(RequestWithBody),
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self::Get(RequestHead::new(url))
    }

    pub fn head(url: impl Into<String>) -> Self {
        Self::Head(RequestHead::new(url))
    }

    pub fn delete(url: impl Into<String>) -> Self {
        Self::Delete(RequestHead::new(url))
    }

    pub fn options(url: impl Into<String>) -> Self {
        Self::Options(RequestHead::new(url))
    }

    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::Post(RequestWithBody::new(url, body))
    }

    pub fn put(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::Put(RequestWithBody::new(url, body))
    }

    pub fn patch(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::Patch(RequestWithBody::new(url, body))
    }

    pub fn with_header(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        match self {
            Self::Get(head) => Self::Get(head.with_header(name, value)),
            Self::Head(head) => Self::Head(head.with_header(name, value)),
            Self::Delete(head) => Self::Delete(head.with_header(name, value)),
            Self::Options(head) => Self::Options(head.with_header(name, value)),
            Self::Post(req) => Self::Post(req.with_header(name, value)),
            Self::Put(req) => Self::Put(req.with_header(name, value)),
            Self::Patch(req) => Self::Patch(req.with_header(name, value)),
        }
    }

    pub fn url(&self) -> &str {
        match self {
            Self::Get(h) | Self::Head(h) | Self::Delete(h) | Self::Options(h) => &h.url,
            Self::Post(r) | Self::Put(r) | Self::Patch(r) => &r.head.url,
        }
    }

    pub fn headers(&self) -> &[(String, String)] {
        match self {
            Self::Get(h) | Self::Head(h) | Self::Delete(h) | Self::Options(h) => &h.headers,
            Self::Post(r) | Self::Put(r) | Self::Patch(r) => &r.head.headers,
        }
    }

    pub fn body(&self) -> &[u8] {
        match self {
            Self::Get(_) | Self::Head(_) | Self::Delete(_) | Self::Options(_) => &[],
            Self::Post(r) | Self::Put(r) | Self::Patch(r) => &r.body,
        }
    }

    pub fn into_body(self) -> Vec<u8> {
        match self {
            Self::Get(_) | Self::Head(_) | Self::Delete(_) | Self::Options(_) => Vec::new(),
            Self::Post(r) | Self::Put(r) | Self::Patch(r) => r.body,
        }
    }

    pub fn method_str(&self) -> &'static str {
        match self {
            Self::Get(_) => "GET",
            Self::Head(_) => "HEAD",
            Self::Delete(_) => "DELETE",
            Self::Options(_) => "OPTIONS",
            Self::Post(_) => "POST",
            Self::Put(_) => "PUT",
            Self::Patch(_) => "PATCH",
        }
    }
}

pub trait HttpClientTrait {
    fn request(
        req: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + MaybeSend;
}
