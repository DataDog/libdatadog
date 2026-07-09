// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use core::{
    fmt,
    future::Future,
    ptr,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use embedded_io_async::Write;
use heapless::Vec as HeaplessVec;
use reqwless::{
    headers::ContentType,
    request::{RequestBody, RequestBuilder},
};

use crate::{
    header::validate_header_value, BuildError, Header, HttpSink, Method, SendError,
    TelemetryMetricsRequest,
};

/// Maximum number of extra headers this wrapper can pass to reqwless without allocation.
pub const MAX_HEADER_COUNT: usize = 16;

const CONNECTION_CLOSE: Header<'static> = Header::new_unchecked("Connection", "close");

type HeaderPairs<'a> = HeaplessVec<(&'a str, &'a str), MAX_HEADER_COUNT>;

/// A borrowed HTTP/1.1 request.
///
/// Header and body emission is delegated to `reqwless`' low-level request API. This wrapper only
/// validates borrowed inputs, keeps a caller-friendly synchronous sink API for signal-handler
/// paths, and supplies fixed-size `heapless` header storage.
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
        Self::new(Method::POST, host, path)
    }

    /// Sets the request body.
    pub const fn with_body(mut self, body: &'a [u8]) -> Self {
        self.body = body;
        self
    }

    /// Sets the `Content-Type` header value emitted by reqwless.
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
        let headers = self.reqwless_headers()?;
        let reqwless_request = self.reqwless_request(headers.as_slice());
        let mut counter = CountingWriter::default();

        block_on_ready(reqwless_request.write_header(&mut counter))
            .map_err(|_| BuildError::LengthOverflow)?
            .map_err(|error| map_reqwless_build_error(error, &counter))?;
        block_on_ready(self.body.write(&mut counter))
            .map_err(|_| BuildError::LengthOverflow)?
            .map_err(|_| BuildError::LengthOverflow)?;

        if counter.overflowed {
            Err(BuildError::LengthOverflow)
        } else {
            Ok(counter.len)
        }
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
        let headers = self.reqwless_headers()?;
        let reqwless_request = self.reqwless_request(headers.as_slice());
        let mut writer = SinkWriter::new(sink);

        match block_on_ready(reqwless_request.write_header(&mut writer)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return match writer.take_error() {
                    Some(error) => Err(SendError::Sink(error)),
                    None => Err(SendError::Reqwless(error)),
                };
            }
            Err(()) => return Err(SendError::Pending),
        }

        match block_on_ready(self.body.write(&mut writer)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_error)) => match writer.take_error() {
                Some(error) => Err(SendError::Sink(error)),
                None => Err(SendError::Pending),
            },
            Err(()) => Err(SendError::Pending),
        }
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
            Err(SendError::Reqwless(error)) => {
                Err(map_reqwless_build_error(error, &CountingWriter::default()))
            }
            Err(SendError::Pending) => Err(BuildError::LengthOverflow),
            Err(SendError::Sink(error)) => match error {},
        }
    }

    fn reqwless_request<'request>(
        &'request self,
        headers: &'request [(&'request str, &'request str)],
    ) -> reqwless::request::Request<'request, &'request [u8]>
    where
        'a: 'request,
    {
        let builder = reqwless::request::Request::new(self.method, self.path)
            .host(self.host)
            .headers(headers);

        let builder = match self.content_type.and_then(content_type) {
            Some(content_type) => builder.content_type(content_type),
            None => builder,
        };

        builder.body(self.body).build()
    }

    fn reqwless_headers(&self) -> Result<HeaderPairs<'a>, BuildError> {
        let mut headers = HeaderPairs::new();

        push_header(&mut headers, CONNECTION_CLOSE)?;
        if let Some(value) = self.content_type {
            if content_type(value).is_none() {
                push_header(&mut headers, Header::new_unchecked("Content-Type", value))?;
            }
        }
        for header in self
            .headers
            .iter()
            .chain(self.extra_headers.iter())
            .copied()
        {
            push_header(&mut headers, header)?;
        }

        Ok(headers)
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

fn content_type(value: &str) -> Option<ContentType> {
    match value {
        "application/json" => Some(ContentType::ApplicationJson),
        "application/cbor" => Some(ContentType::ApplicationCbor),
        "application/octet-stream" => Some(ContentType::ApplicationOctetStream),
        "text/plain" => Some(ContentType::TextPlain),
        _ => None,
    }
}

fn push_header<'a>(headers: &mut HeaderPairs<'a>, header: Header<'a>) -> Result<(), BuildError> {
    headers
        .push((header.name(), header.value()))
        .map_err(|_| BuildError::TooManyHeaders {
            max: MAX_HEADER_COUNT,
        })
}

fn map_reqwless_build_error(error: reqwless::Error, counter: &CountingWriter) -> BuildError {
    let _ = error;
    let _ = counter;
    BuildError::LengthOverflow
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SinkWriteError {
    kind: embedded_io::ErrorKind,
}

impl embedded_io::Error for SinkWriteError {
    fn kind(&self) -> embedded_io::ErrorKind {
        self.kind
    }
}

impl fmt::Display for SinkWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "embedded I/O write failed: {:?}", self.kind)
    }
}

struct SinkWriter<'a, S: HttpSink> {
    sink: &'a mut S,
    error: Option<S::Error>,
}

impl<'a, S: HttpSink> SinkWriter<'a, S> {
    fn new(sink: &'a mut S) -> Self {
        Self { sink, error: None }
    }

    fn take_error(&mut self) -> Option<S::Error> {
        self.error.take()
    }
}

impl<S: HttpSink> embedded_io::ErrorType for SinkWriter<'_, S> {
    type Error = SinkWriteError;
}

impl<S: HttpSink> Write for SinkWriter<'_, S> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.sink.write_all(buf).map_or_else(
            |error| {
                let kind = S::error_kind(&error);
                self.error = Some(error);
                Err(SinkWriteError { kind })
            },
            |()| Ok(buf.len()),
        )
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct CountingWriter {
    len: usize,
    overflowed: bool,
}

impl embedded_io::ErrorType for CountingWriter {
    type Error = SinkWriteError;
}

impl Write for CountingWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if let Some(len) = self.len.checked_add(buf.len()) {
            self.len = len;
        } else {
            self.overflowed = true;
        }
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn block_on_ready<F: Future>(future: F) -> Result<F::Output, ()> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut future = core::pin::pin!(future);

    match future.as_mut().poll(&mut cx) {
        Poll::Ready(output) => Ok(output),
        Poll::Pending => Err(()),
    }
}

fn noop_waker() -> Waker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        noop_waker_clone,
        noop_waker_wake,
        noop_waker_wake_by_ref,
        noop_waker_drop,
    );

    // SAFETY: The raw waker uses a null data pointer that is never dereferenced. All vtable
    // operations are no-ops and cloning returns another equivalent raw waker.
    unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &VTABLE)) }
}

unsafe fn noop_waker_clone(_data: *const ()) -> RawWaker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        noop_waker_clone,
        noop_waker_wake,
        noop_waker_wake_by_ref,
        noop_waker_drop,
    );
    RawWaker::new(ptr::null(), &VTABLE)
}

unsafe fn noop_waker_wake(_data: *const ()) {}

unsafe fn noop_waker_wake_by_ref(_data: *const ()) {}

unsafe fn noop_waker_drop(_data: *const ()) {}

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
            "Content-Type: text/plain\r\n",
            "Content-Length: 4\r\n",
            "Connection: close\r\n",
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

        assert!(matches!(result, Err(SendError::Sink(BufferTooSmall))));
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

    #[test]
    fn too_many_headers_are_rejected_before_writing() {
        let headers = [Header::new_unchecked("X-Test", "yes"); MAX_HEADER_COUNT];
        let request = Request::post("localhost:8126", "/v1/input").with_headers(&headers);
        let mut storage = [0_u8; 1024];
        let mut buffer = FixedBuffer::new(&mut storage);

        assert!(matches!(
            request.write_to(&mut buffer),
            Err(SendError::Build(BuildError::TooManyHeaders {
                max: MAX_HEADER_COUNT
            }))
        ));
        assert!(buffer.is_empty());
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
