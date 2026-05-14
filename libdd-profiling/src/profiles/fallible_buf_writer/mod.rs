// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::io::{self, IoSlice, Write};

/// A buffered writer which can fail while reserving its internal buffer.
///
/// This mirrors the parts of [`io::BufWriter`] used by this crate, except
/// construction is fallible and there is intentionally no default-capacity
/// constructor.
#[derive(Debug)]
pub(crate) struct BufWriter<W: Write> {
    inner: Option<W>,
    buf: Vec<u8>,
    panicked: bool,
}

#[derive(Debug)]
pub(crate) struct IntoInnerError<W> {
    writer: W,
    error: io::Error,
}

impl<W> IntoInnerError<W> {
    pub(crate) fn into_error(self) -> io::Error {
        self.error
    }

    #[allow(unused)]
    pub(crate) fn error(&self) -> &io::Error {
        &self.error
    }

    #[allow(unused)]
    pub(crate) fn into_inner(self) -> W {
        self.writer
    }
}

impl<W> fmt::Display for IntoInnerError<W> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(fmt)
    }
}

impl<W: fmt::Debug> std::error::Error for IntoInnerError<W> {}

impl<W> From<IntoInnerError<W>> for io::Error {
    fn from(error: IntoInnerError<W>) -> Self {
        error.into_error()
    }
}

impl<W: Write> BufWriter<W> {
    pub(crate) fn try_with_capacity(capacity: usize, inner: W) -> io::Result<Self> {
        let mut buf = Vec::new();
        buf.try_reserve_exact(capacity)?;

        Ok(Self {
            inner: Some(inner),
            buf,
            panicked: false,
        })
    }

    #[allow(unused)]
    pub(crate) fn get_ref(&self) -> &W {
        self.inner_ref()
    }

    #[allow(unused)]
    pub(crate) fn get_mut(&mut self) -> &mut W {
        self.inner_mut()
    }

    #[allow(unused)]
    pub(crate) fn buffer(&self) -> &[u8] {
        &self.buf
    }

    #[allow(unused)]
    pub(crate) fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    pub(crate) fn into_inner(mut self) -> Result<W, IntoInnerError<Self>> {
        if let Err(error) = self.flush_buf() {
            return Err(IntoInnerError {
                writer: self,
                error,
            });
        }

        let inner = self.inner.take();
        debug_assert!(inner.is_some(), "BufWriter missing inner writer");
        // SAFETY: `inner` is only set to `None` by this method after the
        // buffer has been flushed. Since this method takes ownership of
        // `self`, no public API can observe or call it again after that move.
        Ok(unsafe { inner.unwrap_unchecked() })
    }

    fn inner_ref(&self) -> &W {
        debug_assert!(self.inner.is_some(), "BufWriter missing inner writer");
        // SAFETY: `inner` is `Some` for every live `BufWriter` except during
        // `into_inner` after it has taken ownership of the writer and before
        // the moved `BufWriter` is dropped. This method requires `&self`, so
        // it cannot be called during that consumed state.
        unsafe { self.inner.as_ref().unwrap_unchecked() }
    }

    fn inner_mut(&mut self) -> &mut W {
        debug_assert!(self.inner.is_some(), "BufWriter missing inner writer");
        // SAFETY: `inner` is `Some` for every live `BufWriter` except during
        // `into_inner` after it has taken ownership of the writer and before
        // the moved `BufWriter` is dropped. This method requires `&mut self`,
        // so it cannot be called during that consumed state.
        unsafe { self.inner.as_mut().unwrap_unchecked() }
    }

    #[inline]
    pub fn spare_capacity(&self) -> usize {
        self.buf.capacity() - self.buf.len()
    }

    fn flush_buf(&mut self) -> io::Result<()> {
        struct BufGuard<'a> {
            buffer: &'a mut Vec<u8>,
            written: usize,
        }

        impl Drop for BufGuard<'_> {
            fn drop(&mut self) {
                if self.written > 0 {
                    self.buffer.drain(..self.written);
                }
            }
        }

        impl<'a> BufGuard<'a> {
            fn new(buffer: &'a mut Vec<u8>) -> Self {
                Self { buffer, written: 0 }
            }

            fn remaining(&self) -> &[u8] {
                &self.buffer[self.written..]
            }

            fn consume(&mut self, amt: usize) {
                self.written += amt;
            }

            fn done(&self) -> bool {
                self.written >= self.buffer.len()
            }
        }

        debug_assert!(self.inner.is_some(), "BufWriter missing inner writer");
        // SAFETY: `flush_buf` is not called after `into_inner` has taken the
        // inner writer. Drop also checks `inner.is_some()` before flushing.
        let inner = unsafe { self.inner.as_mut().unwrap_unchecked() };
        let panicked = &mut self.panicked;
        let mut guard = BufGuard::new(&mut self.buf);

        while !guard.done() {
            *panicked = true;
            let result = inner.write(guard.remaining());
            *panicked = false;

            match result {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => guard.consume(n),
                Err(error) => {
                    return Err(error);
                }
            }
        }

        Ok(())
    }
}

impl<W: Write> Write for BufWriter<W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.spare_capacity() {
            self.flush_buf()?;
        }

        if buf.len() >= self.buf.capacity() {
            self.panicked = true;
            let result = self.inner_mut().write(buf);
            self.panicked = false;
            result
        } else {
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }
    }

    #[inline]
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        if bufs.iter().all(|buf| buf.is_empty()) {
            return Ok(0);
        }

        if let [buf] = bufs {
            return self.write(buf);
        }

        let mut written = 0;
        for buf in bufs {
            if buf.len() > self.spare_capacity() {
                if written > 0 {
                    return Ok(written);
                }
                self.flush_buf()?;
            }

            if buf.len() >= self.buf.capacity() {
                self.panicked = true;
                let result = self.inner_mut().write_vectored(bufs);
                self.panicked = false;
                return result;
            }

            self.buf.extend_from_slice(buf);
            written += buf.len();
        }

        Ok(written)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.flush_buf()?;
        self.panicked = true;
        let result = self.inner_mut().flush();
        self.panicked = false;
        result
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if buf.len() > self.spare_capacity() {
            self.flush_buf()?;
        }

        if buf.len() >= self.buf.capacity() {
            self.panicked = true;
            let result = self.inner_mut().write_all(buf);
            self.panicked = false;
            result
        } else {
            self.buf.extend_from_slice(buf);
            Ok(())
        }
    }
}

impl<W: Write> Drop for BufWriter<W> {
    fn drop(&mut self) {
        if self.inner.is_some() && !self.panicked {
            let _ = self.flush_buf();
        }
    }
}

#[cfg(test)]
mod tests;
