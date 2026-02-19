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

    /// Returns true if the status code is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Returns true if the status code is in the 4xx range.
    pub fn is_client_error(&self) -> bool {
        (400..500).contains(&self.status)
    }

    /// Returns true if the status code is in the 5xx range.
    pub fn is_server_error(&self) -> bool {
        (500..600).contains(&self.status)
    }
}

#[derive(Debug, Clone)]
pub enum HttpError {
    Network(String),
    Timeout,
    ResponseBody(String),
    InvalidRequest(String),
    Other(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Network(msg) => write!(f, "Network error: {}", msg),
            HttpError::Timeout => write!(f, "Request timed out"),
            HttpError::ResponseBody(msg) => write!(f, "Response body error: {}", msg),
            HttpError::InvalidRequest(msg) => write!(f, "Invalid request: {}", msg),
            HttpError::Other(msg) => write!(f, "HTTP error: {}", msg),
        }
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Delete,
    Options,
    Post,
    Put,
    Patch,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
        }
    }

    pub fn accepts_body(self) -> bool {
        matches!(self, Self::Post | Self::Put | Self::Patch)
    }
}

pub type Body = Vec<u8>;

#[derive(Debug, Clone)]
pub struct HttpRequest {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Body>,
}

impl HttpRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }

    pub fn head(url: impl Into<String>) -> Self {
        Self::new(Method::Head, url)
    }

    pub fn delete(url: impl Into<String>) -> Self {
        Self::new(Method::Delete, url)
    }

    pub fn options(url: impl Into<String>) -> Self {
        Self::new(Method::Options, url)
    }

    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::new(Method::Post, url)
            .with_body(body)
            .expect("POST must accept body")
    }

    pub fn put(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::new(Method::Put, url)
            .with_body(body)
            .expect("PUT must accept body")
    }

    pub fn patch(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::new(Method::Patch, url)
            .with_body(body)
            .expect("PATCH must accept body")
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_body(mut self, body: Body) -> Result<Self, HttpError> {
        self.set_body(body)?;
        Ok(self)
    }

    pub fn set_body(&mut self, body: Body) -> Result<(), HttpError> {
        if !self.method.accepts_body() {
            return Err(HttpError::InvalidRequest(format!(
                "method {} does not accept a request body",
                self.method.as_str()
            )));
        }
        self.body = Some(body);
        Ok(())
    }

    pub fn clear_body(&mut self) {
        self.body = None;
    }

    pub fn method(&self) -> Method {
        self.method
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    pub fn body(&self) -> Option<&Body> {
        self.body.as_ref()
    }

    pub fn into_body(self) -> Option<Body> {
        self.body
    }

    pub fn method_str(&self) -> &'static str {
        self.method.as_str()
    }
}

pub trait HttpClientTrait {
    fn new_client() -> Self;

    fn request(
        &self,
        req: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + MaybeSend;
}
