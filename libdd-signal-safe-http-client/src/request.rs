// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::{
    header::validate_header_value, BuildError, Header, HttpSink, SendError, TelemetryMetricsRequest,
};

const CRLF: &[u8] = b"\r\n";
const HTTP_VERSION: &str = "HTTP/1.1";
const CONNECTION_CLOSE: Header<'static> = Header::new_unchecked("Connection", "close");

/// HTTP methods supported by the allocation-free request writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
}

impl Method {
    /// Returns the wire representation of the method.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// A borrowed HTTP/1.1 request.
///
/// The writer emits `Host`, `Content-Length`, and `Connection: close` automatically. Optional
/// content type and caller-provided headers are emitted before the body.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    method: Method,
    host: &'a str,
    path: &'a str,
    body: &'a [u8],
    content_type: Option<&'a str>,
    headers: &'a [Header<'a>],
    extra_headers: &'a [Header<'a>],
}

impl<'a> Request<'a> {
    /// Creates a request with no body and no optional headers.
    pub const fn new(method: Method, host: &'a str, path: &'a str) -> Self {
        Self {
            method,
            host,
            path,
            body: &[],
            content_type: None,
            headers: &[],
            extra_headers: &[],
        }
    }

    /// Creates a `POST` request with no body and no optional headers.
    pub const fn post(host: &'a str, path: &'a str) -> Self {
        Self::new(Method::Post, host, path)
    }

    /// Sets the request body.
    pub const fn with_body(mut self, body: &'a [u8]) -> Self {
        self.body = body;
        self
    }

    /// Sets the `Content-Type` header value emitted by the writer.
    pub const fn with_content_type(mut self, content_type: &'a str) -> Self {
        self.content_type = Some(content_type);
        self
    }

    /// Sets the primary caller-provided header slice.
    pub const fn with_headers(mut self, headers: &'a [Header<'a>]) -> Self {
        self.headers = headers;
        self
    }

    /// Sets an additional caller-provided header slice.
    ///
    /// This is mainly useful for helpers that have their own static headers but still need to
    /// append caller-supplied endpoint headers without allocating.
    pub const fn with_extra_headers(mut self, headers: &'a [Header<'a>]) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Returns the request method.
    pub const fn method(&self) -> Method {
        self.method
    }

    /// Returns the `Host` header value.
    pub const fn host(&self) -> &'a str {
        self.host
    }

    /// Returns the request path.
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// Returns the request body.
    pub const fn body(&self) -> &'a [u8] {
        self.body
    }

    /// Returns the number of bytes needed to encode the full request.
    pub fn encoded_len(&self) -> Result<usize, BuildError> {
        self.validate()?;

        let mut len = 0;
        len = checked_add(len, self.method.as_str().len())?;
        len = checked_add(len, 1)?;
        len = checked_add(len, self.path.len())?;
        len = checked_add(len, 1)?;
        len = checked_add(len, HTTP_VERSION.len())?;
        len = checked_add(len, CRLF.len())?;

        len = checked_add_header_len(len, "Host", self.host)?;
        len = checked_add_header_len(len, "Content-Length", decimal_len(self.body.len()))?;
        len = checked_add_header_len(len, CONNECTION_CLOSE.name(), CONNECTION_CLOSE.value())?;

        if let Some(content_type) = self.content_type {
            len = checked_add_header_len(len, "Content-Type", content_type)?;
        }

        for header in self.headers.iter().chain(self.extra_headers.iter()) {
            len = checked_add_header_len(len, header.name(), header.value())?;
        }

        len = checked_add(len, CRLF.len())?;
        checked_add(len, self.body.len())
    }

    /// Validates all request fields without emitting bytes.
    pub fn validate(&self) -> Result<(), BuildError> {
        validate_host(self.host)?;
        validate_path(self.path)?;
        if let Some(content_type) = self.content_type {
            validate_header_value(content_type)?;
        }
        for header in self.headers.iter().chain(self.extra_headers.iter()) {
            header.validate()?;
        }
        Ok(())
    }

    /// Writes the full HTTP request into the supplied sink.
    pub fn write_to<S: HttpSink>(&self, sink: &mut S) -> Result<(), SendError<S::Error>> {
        self.validate()?;

        write_str(sink, self.method.as_str())?;
        write_all(sink, b" ")?;
        write_str(sink, self.path)?;
        write_all(sink, b" ")?;
        write_str(sink, HTTP_VERSION)?;
        write_all(sink, CRLF)?;

        write_header(sink, "Host", self.host)?;
        write_str(sink, "Content-Length: ")?;
        write_decimal(sink, self.body.len())?;
        write_all(sink, CRLF)?;
        write_header(sink, CONNECTION_CLOSE.name(), CONNECTION_CLOSE.value())?;

        if let Some(content_type) = self.content_type {
            write_header(sink, "Content-Type", content_type)?;
        }

        for header in self.headers.iter().chain(self.extra_headers.iter()) {
            write_header(sink, header.name(), header.value())?;
        }

        write_all(sink, CRLF)?;
        write_all(sink, self.body)
    }

    /// Encodes the full HTTP request into an owned buffer.
    #[cfg(feature = "alloc")]
    pub fn to_vec(&self) -> Result<Vec<u8>, BuildError> {
        let len = self.encoded_len()?;
        let mut out = Vec::new();
        if out.try_reserve_exact(len).is_err() {
            return Err(BuildError::AllocationFailed);
        }

        match self.write_to(&mut out) {
            Ok(()) => Ok(out),
            Err(SendError::Build(error)) => Err(error),
            Err(SendError::Sink(error)) => match error {},
        }
    }
}

/// A tiny client that carries default endpoint headers for repeated submissions.
#[derive(Debug, Clone, Copy)]
pub struct HttpClient<'a> {
    host: &'a str,
    default_headers: &'a [Header<'a>],
}

impl<'a> HttpClient<'a> {
    /// Creates a client for the supplied HTTP `Host` header value.
    pub const fn new(host: &'a str) -> Self {
        Self {
            host,
            default_headers: &[],
        }
    }

    /// Sets endpoint headers appended to every client helper request.
    ///
    /// Typical examples are `dd-api-key` for direct intake or
    /// `x-datadog-test-session-token` for tests.
    pub const fn with_default_headers(mut self, headers: &'a [Header<'a>]) -> Self {
        self.default_headers = headers;
        self
    }

    /// Returns the HTTP `Host` header value used by this client.
    pub const fn host(&self) -> &'a str {
        self.host
    }

    /// Returns the default endpoint headers appended by helper methods.
    pub const fn default_headers(&self) -> &'a [Header<'a>] {
        self.default_headers
    }

    /// Writes a Datadog telemetry `generate-metrics` request into the supplied sink.
    pub fn submit_telemetry_metrics<S: HttpSink>(
        &self,
        payload: &[u8],
        sink: &mut S,
    ) -> Result<(), SendError<S::Error>> {
        TelemetryMetricsRequest::new_agent(self.host, payload)
            .with_headers(self.default_headers)
            .submit(sink)
    }
}

fn validate_host(host: &str) -> Result<(), BuildError> {
    if host.is_empty() || !host.bytes().all(is_host_byte) {
        return Err(BuildError::InvalidHost);
    }
    Ok(())
}

fn is_host_byte(byte: u8) -> bool {
    matches!(byte, b'!'..=b'~') && byte != b'/'
}

fn validate_path(path: &str) -> Result<(), BuildError> {
    if path.as_bytes().first().copied() != Some(b'/') || !path.bytes().all(is_path_byte) {
        return Err(BuildError::InvalidPath);
    }
    Ok(())
}

fn is_path_byte(byte: u8) -> bool {
    matches!(byte, b'!'..=b'~')
}

fn checked_add(lhs: usize, rhs: usize) -> Result<usize, BuildError> {
    lhs.checked_add(rhs).ok_or(BuildError::LengthOverflow)
}

fn checked_add_header_len(
    len: usize,
    name: &str,
    value_or_value_len: impl HeaderLen,
) -> Result<usize, BuildError> {
    let len = checked_add(len, name.len())?;
    let len = checked_add(len, 2)?;
    let len = checked_add(len, value_or_value_len.header_len())?;
    checked_add(len, CRLF.len())
}

trait HeaderLen {
    fn header_len(self) -> usize;
}

impl HeaderLen for &str {
    fn header_len(self) -> usize {
        self.len()
    }
}

impl HeaderLen for usize {
    fn header_len(self) -> usize {
        self
    }
}

fn decimal_len(mut value: usize) -> usize {
    let mut len = 1;
    while value >= 10 {
        value /= 10;
        len += 1;
    }
    len
}

fn write_header<S: HttpSink>(
    sink: &mut S,
    name: &str,
    value: &str,
) -> Result<(), SendError<S::Error>> {
    write_str(sink, name)?;
    write_all(sink, b": ")?;
    write_str(sink, value)?;
    write_all(sink, CRLF)
}

fn write_decimal<S: HttpSink>(sink: &mut S, value: usize) -> Result<(), SendError<S::Error>> {
    let mut buffer = [0_u8; 39];
    let mut index = buffer.len();
    let mut remaining = value;

    loop {
        index -= 1;
        buffer[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }

    write_all(sink, &buffer[index..])
}

fn write_str<S: HttpSink>(sink: &mut S, value: &str) -> Result<(), SendError<S::Error>> {
    write_all(sink, value.as_bytes())
}

fn write_all<S: HttpSink>(sink: &mut S, chunk: &[u8]) -> Result<(), SendError<S::Error>> {
    sink.write_all(chunk).map_err(SendError::Sink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BufferTooSmall, FixedBuffer};

    #[test]
    fn request_writer_emits_http_11_request() -> Result<(), SendError<BufferTooSmall>> {
        let headers = [Header::new_unchecked("X-Test", "yes")];
        let request = Request::post("localhost:8126", "/v1/input")
            .with_body(b"body")
            .with_content_type("text/plain")
            .with_headers(&headers);
        let mut storage = [0_u8; 256];
        let mut buffer = FixedBuffer::new(&mut storage);

        request.write_to(&mut buffer)?;

        let expected = concat!(
            "POST /v1/input HTTP/1.1\r\n",
            "Host: localhost:8126\r\n",
            "Content-Length: 4\r\n",
            "Connection: close\r\n",
            "Content-Type: text/plain\r\n",
            "X-Test: yes\r\n",
            "\r\n",
            "body"
        )
        .as_bytes();
        assert_eq!(buffer.as_slice(), expected);
        assert_eq!(request.encoded_len(), Ok(expected.len()));
        Ok(())
    }

    #[test]
    fn fixed_buffer_reports_capacity_errors() {
        let request = Request::post("localhost:8126", "/v1/input").with_body(b"body");
        let mut storage = [0_u8; 4];
        let mut buffer = FixedBuffer::new(&mut storage);

        let result = request.write_to(&mut buffer);

        assert_eq!(result, Err(SendError::Sink(BufferTooSmall)));
    }

    #[test]
    fn header_validation_rejects_injection() {
        assert_eq!(
            Header::new("X-Test", "ok\r\nInjected: yes"),
            Err(BuildError::InvalidHeaderValue)
        );
        assert_eq!(
            Header::new("Bad Header", "value"),
            Err(BuildError::InvalidHeaderName)
        );
    }

    #[test]
    fn request_validation_rejects_bad_host_and_path() {
        assert_eq!(
            Request::post("bad\r\nhost", "/ok").validate(),
            Err(BuildError::InvalidHost)
        );
        assert_eq!(
            Request::post("localhost", "relative").validate(),
            Err(BuildError::InvalidPath)
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn alloc_to_vec_matches_encoded_request() -> Result<(), BuildError> {
        let request = Request::post("localhost:8126", "/v1/input").with_body(b"body");

        let bytes = request.to_vec()?;

        assert_eq!(bytes.len(), request.encoded_len()?);
        assert!(bytes.starts_with(b"POST /v1/input HTTP/1.1\r\n"));
        assert!(bytes.ends_with(b"\r\n\r\nbody"));
        Ok(())
    }
}
