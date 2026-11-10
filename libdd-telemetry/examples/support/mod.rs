// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use embedded_io::ErrorType;
use embedded_io_async::{Read, Write};

pub struct FixedConnection {
    response: &'static [u8],
    response_offset: usize,
    request: [u8; 4_096],
    request_len: usize,
}

impl FixedConnection {
    pub const fn new(response: &'static [u8]) -> Self {
        Self {
            response,
            response_offset: 0,
            request: [0; 4_096],
            request_len: 0,
        }
    }

    pub fn request(&self) -> &[u8] {
        &self.request[..self.request_len]
    }
}

impl ErrorType for FixedConnection {
    type Error = embedded_io::SliceWriteError;
}

impl Read for FixedConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let remaining = &self.response[self.response_offset..];
        let length = remaining.len().min(buffer.len());
        buffer[..length].copy_from_slice(&remaining[..length]);
        self.response_offset += length;
        Ok(length)
    }
}

impl Write for FixedConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let remaining = self.request.len() - self.request_len;
        if remaining == 0 && !buffer.is_empty() {
            return Err(embedded_io::SliceWriteError::Full);
        }
        let length = remaining.min(buffer.len());
        self.request[self.request_len..self.request_len + length]
            .copy_from_slice(&buffer[..length]);
        self.request_len += length;
        Ok(length)
    }
}

// The in-memory transport is immediately ready, so no executor is needed.
pub fn poll_once<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}
