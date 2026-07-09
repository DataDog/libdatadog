// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// A caller-owned destination for encoded HTTP request bytes.
///
/// Implementations decide how bytes are delivered. For signal-handler use, the implementation must
/// avoid allocation, locks, and non-async-signal-safe OS calls.
pub trait HttpSink {
    /// The sink-specific write error.
    type Error;

    /// Writes the entire chunk or returns an error.
    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Self::Error>;

    /// Converts a sink-specific write error into an embedded I/O error kind.
    fn error_kind(_error: &Self::Error) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

/// Error returned by [`FixedBuffer`] when there is not enough remaining capacity.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("buffer too small")]
pub struct BufferTooSmall;

/// A fixed-size, caller-owned byte buffer implementing [`HttpSink`].
pub struct FixedBuffer<'a> {
    buffer: &'a mut [u8],
    len: usize,
}

impl<'a> FixedBuffer<'a> {
    /// Creates an empty fixed buffer over the supplied storage.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, len: 0 }
    }

    /// Clears the written length without zeroing the underlying storage.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns the number of bytes written.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no bytes have been written.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the total buffer capacity.
    pub const fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Returns the remaining writable capacity.
    pub fn remaining(&self) -> usize {
        self.capacity() - self.len
    }

    /// Returns the initialized prefix containing all written bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}

impl HttpSink for FixedBuffer<'_> {
    type Error = BufferTooSmall;

    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Self::Error> {
        let Some(end) = self.len.checked_add(chunk.len()) else {
            return Err(BufferTooSmall);
        };
        if end > self.buffer.len() {
            return Err(BufferTooSmall);
        }

        self.buffer[self.len..end].copy_from_slice(chunk);
        self.len = end;
        Ok(())
    }

    fn error_kind(_error: &Self::Error) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::WriteZero
    }
}

/// A [`std::io::Write`]-backed sink for non-signal-handler use.
#[cfg(feature = "std")]
pub struct StdWriteSink<W> {
    writer: W,
}

#[cfg(feature = "std")]
impl<W> StdWriteSink<W> {
    /// Creates a sink over a standard writer.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Returns a shared reference to the wrapped writer.
    pub const fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Returns a mutable reference to the wrapped writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Consumes the sink and returns the wrapped writer.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[cfg(feature = "std")]
impl<W: std::io::Write> HttpSink for StdWriteSink<W> {
    type Error = std::io::Error;

    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Self::Error> {
        std::io::Write::write_all(&mut self.writer, chunk)
    }

    fn error_kind(_error: &Self::Error) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

#[cfg(feature = "alloc")]
impl HttpSink for alloc::vec::Vec<u8> {
    type Error = core::convert::Infallible;

    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Self::Error> {
        self.extend_from_slice(chunk);
        Ok(())
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use crate::{Header, Request};

    use super::*;

    #[cfg(feature = "std")]
    #[test]
    fn std_write_sink_writes_to_standard_writer() -> Result<(), crate::SendError<std::io::Error>> {
        let mut bytes = std::vec::Vec::new();
        let headers = [Header::new_unchecked("X-Test", "yes")];
        let request = Request::post("localhost:8126", "/v1/input")
            .with_body(b"body")
            .with_headers(&headers);

        request.write_to(&mut StdWriteSink::new(&mut bytes))?;

        assert!(bytes.starts_with(b"POST /v1/input HTTP/1.1\r\n"));
        assert!(bytes.ends_with(b"\r\n\r\nbody"));
        Ok(())
    }
}
