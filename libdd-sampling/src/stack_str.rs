// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Stack-buffer string formatting for hot paths.
//!
//! [`StackStr`] implements [`fmt::Write`] over a caller-provided byte buffer, so
//! short formatted strings (sampling keys, integral attribute values) can be
//! built without allocating. Writes that don't fit return an error so callers
//! can fall back to the heap.

use std::fmt;

/// A fixed-capacity string builder over a borrowed byte buffer.
pub(crate) struct StackStr<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> StackStr<'a> {
    /// Creates a builder writing into `buf`.
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    /// Consumes the builder and returns the bytes written so far as a string slice
    /// borrowing from the original buffer.
    ///
    /// Only `write_str`/`write_char` copy data, so the bytes are always valid UTF-8.
    /// The fallback is unreachable in practice but keeps this panic-free.
    pub(crate) fn into_str(self) -> &'a str {
        let len = self.len;
        str::from_utf8(&self.buf[..len]).unwrap_or_default()
    }
}

impl fmt::Write for StackStr<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.len + s.len() > self.buf.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }

    fn write_char(&mut self, c: char) -> fmt::Result {
        let mut encoded = [0u8; 4];
        self.write_str(c.encode_utf8(&mut encoded))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn writes_multiple_pieces() {
        let mut buf = [0u8; 32];
        let mut s = StackStr::new(&mut buf);
        let service = "web";
        let env = "prod";
        write!(s, "service:{service},env:{env}").unwrap();
        assert_eq!(s.into_str(), "service:web,env:prod");
    }

    #[test]
    fn rejects_overflow() {
        let mut buf = [0u8; 4];
        let mut s = StackStr::new(&mut buf);
        assert!(write!(s, "toolong").is_err());
        assert_eq!(s.into_str(), "");
    }

    #[test]
    fn writes_unicode() {
        let mut buf = [0u8; 32];
        let mut s = StackStr::new(&mut buf);
        let service = "caf\u{00e9}";
        let env = "PROD";
        write!(s, "{service}-{env}").unwrap();
        assert_eq!(s.into_str(), "caf\u{00e9}-PROD");
    }

    #[test]
    fn formats_integral_floats() {
        // `f64` Display prints integral values without a fraction, matching
        // `to_string()` for the values that reach the integer path.
        let mut buf = [0u8; 24];
        let mut s = StackStr::new(&mut buf);
        let value = 200.0f64;
        write!(s, "{value}").unwrap();
        assert_eq!(s.into_str(), "200");
    }
}
