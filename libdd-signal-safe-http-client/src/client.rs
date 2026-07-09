// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    future::Future,
    ptr,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use embedded_io::{Error as _, Read as SyncRead, Write as SyncWrite};
use embedded_io_async::{Read, Write};
use reqwless::{
    headers::ContentType,
    request::{Request, RequestBody},
    response::StatusCode,
};

/// Errors returned by the synchronous HTTP client façade.
#[derive(Debug, thiserror::Error)]
pub enum ClientError<E> {
    /// The caller-provided transport failed.
    #[error("HTTP transport error: {0:?}")]
    Transport(E),
    /// reqwless rejected the request, response, or protocol state.
    #[error("reqwless HTTP error: {0:?}")]
    Reqwless(reqwless::Error),
    /// The reqwless operation could not complete synchronously.
    #[error("HTTP operation would block")]
    WouldBlock,
}

/// Summary of the HTTP response headers parsed by reqwless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: StatusCode,
    /// Parsed response content type, when present and recognized by reqwless.
    pub content_type: Option<&'static str>,
    /// Parsed response content length, when present.
    pub content_length: Option<usize>,
}

/// Synchronous façade over reqwless for preconnected transports.
///
/// The transport implements synchronous [`embedded_io::Read`] and [`embedded_io::Write`]. The
/// async reqwless boundary is internal to this type; callers do not provide an executor or async
/// transport.
#[derive(Debug, Clone, Copy)]
pub struct HttpClient<T> {
    transport: T,
}

impl<T> HttpClient<T> {
    /// Creates a client over a preconnected transport.
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Returns a shared reference to the wrapped transport.
    pub const fn get_ref(&self) -> &T {
        &self.transport
    }

    /// Returns a mutable reference to the wrapped transport.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Consumes the client and returns the wrapped transport.
    pub fn into_inner(self) -> T {
        self.transport
    }
}

impl<T> HttpClient<T>
where
    T: SyncRead + SyncWrite,
{
    /// Sends a reqwless request and parses response headers into `response_header_buf`.
    ///
    /// The buffer must be large enough for the response headers. The response body is not read.
    pub fn send<'request, 'buf, B>(
        &mut self,
        request: Request<'request, B>,
        response_header_buf: &'buf mut [u8],
    ) -> Result<HttpResponse, ClientError<T::Error>>
    where
        B: RequestBody,
    {
        let mut io = SyncIo::new(&mut self.transport);
        let response = {
            let mut connection = reqwless::client::HttpConnection::Plain(&mut io);
            match block_on_ready(connection.send(request, response_header_buf)) {
                Ok(Ok(response)) => Ok(HttpResponse {
                    status: response.status,
                    content_type: content_type_str(response.content_type),
                    content_length: response.content_length,
                }),
                Ok(Err(error)) => Err(Some(error)),
                Err(()) => Err(None),
            }
        };

        match response {
            Ok(response) => Ok(response),
            Err(Some(error)) => Err(map_reqwless_error(error, &mut io)),
            Err(None) => Err(ClientError::WouldBlock),
        }
    }
}

fn content_type_str(content_type: Option<ContentType>) -> Option<&'static str> {
    match content_type {
        Some(ContentType::TextPlain) => Some("text/plain"),
        Some(ContentType::ApplicationJson) => Some("application/json"),
        Some(ContentType::ApplicationCbor) => Some("application/cbor"),
        Some(ContentType::ApplicationOctetStream) => Some("application/octet-stream"),
        None => None,
    }
}

fn map_reqwless_error<E>(
    error: reqwless::Error,
    io: &mut SyncIo<'_, impl embedded_io::ErrorType<Error = E>>,
) -> ClientError<E> {
    match io.take_error() {
        Some(error) => ClientError::Transport(error),
        None => ClientError::Reqwless(error),
    }
}

struct SyncIo<'a, T: embedded_io::ErrorType> {
    inner: &'a mut T,
    error: Option<T::Error>,
}

impl<'a, T: embedded_io::ErrorType> SyncIo<'a, T> {
    fn new(inner: &'a mut T) -> Self {
        Self { inner, error: None }
    }

    fn take_error(&mut self) -> Option<T::Error> {
        self.error.take()
    }
}

impl<T> embedded_io::ErrorType for SyncIo<'_, T>
where
    T: embedded_io::ErrorType,
{
    type Error = embedded_io::ErrorKind;
}

impl<T> Read for SyncIo<'_, T>
where
    T: SyncRead,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buf).map_err(|error| {
            let kind = error.kind();
            self.error = Some(error);
            kind
        })
    }
}

impl<T> Write for SyncIo<'_, T>
where
    T: SyncWrite,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buf).map_err(|error| {
            let kind = error.kind();
            self.error = Some(error);
            kind
        })
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().map_err(|error| {
            let kind = error.kind();
            self.error = Some(error);
            kind
        })
    }

    async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), Self::Error> {
        while !buf.is_empty() {
            match self.write(buf).await {
                Ok(0) => return Err(embedded_io::ErrorKind::WriteZero),
                Ok(written) => buf = &buf[written..],
                Err(error) => return Err(error),
            }
        }
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
    use crate::{Status, TelemetryMetricsRequestBuilder};

    use super::*;

    #[test]
    fn client_sends_reqwless_request_without_async_api() -> Result<(), ClientError<TestError>> {
        let payload = br#"{"series":[]}"#;
        let builder = TelemetryMetricsRequestBuilder::agent("localhost:8126", payload);
        let headers = builder.headers();
        let request = builder.build(&headers);
        let transport = FakeTransport::new(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
        let mut client = HttpClient::new(transport);
        let mut response_header_buf = [0_u8; 128];

        let response = client.send(request, &mut response_header_buf)?;

        assert_eq!(response.status, Status::Accepted);
        assert_eq!(response.content_length, Some(0));
        assert_eq!(
            client.get_ref().written.as_slice(),
            concat!(
                "POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n",
                "Host: localhost:8126\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: 13\r\n",
                "Connection: close\r\n",
                "DD-Telemetry-Request-Type: generate-metrics\r\n",
                "DD-Telemetry-API-Version: v2\r\n",
                "DD-Telemetry-Debug-Enabled: false\r\n",
                "\r\n",
                r#"{"series":[]}"#
            )
            .as_bytes()
        );
        Ok(())
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestError;

    impl embedded_io::Error for TestError {
        fn kind(&self) -> embedded_io::ErrorKind {
            embedded_io::ErrorKind::Other
        }
    }

    struct FakeTransport {
        written: std::vec::Vec<u8>,
        response: &'static [u8],
        read_offset: usize,
    }

    impl FakeTransport {
        fn new(response: &'static [u8]) -> Self {
            Self {
                written: std::vec::Vec::new(),
                response,
                read_offset: 0,
            }
        }
    }

    impl embedded_io::ErrorType for FakeTransport {
        type Error = TestError;
    }

    impl SyncRead for FakeTransport {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let remaining = &self.response[self.read_offset..];
            let read = remaining.len().min(buf.len());
            buf[..read].copy_from_slice(&remaining[..read]);
            self.read_offset += read;
            Ok(read)
        }
    }

    impl SyncWrite for FakeTransport {
        fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }
}
