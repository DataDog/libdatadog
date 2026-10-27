// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared pieces of the per-format `ChunkPacker`s, which build one payload at a time from
//! successive chunks without exceeding a byte limit.
//!
//! A packer appends each chunk's encoding to its buffer, then checks the size of the complete
//! payload, envelope included. A rejected chunk is rolled back, leaving the payload as it was
//! before the call. Envelope parts whose width depends on the final contents (chunk count,
//! dictionaries, promoted metadata) are only written by `finish`, then rotated to the front.

use std::fmt;
use std::io;

/// Why a chunk was not added to a payload. Either way the payload is unchanged.
#[derive(Debug)]
pub enum PushError {
    /// The payload with this chunk would exceed the limit. A chunk rejected by an empty packer
    /// cannot fit in any payload, since adding chunks never makes a payload smaller.
    Full,
    /// The chunk cannot be represented in the output format.
    Invalid(anyhow::Error),
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Full => f.write_str("chunk does not fit within the payload limit"),
            PushError::Invalid(error) => write!(f, "chunk cannot be encoded: {error}"),
        }
    }
}

impl std::error::Error for PushError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PushError::Full => None,
            PushError::Invalid(error) => Some(error.as_ref()),
        }
    }
}

/// Appends to `buf` while its length stays within `max`, and never grows its capacity beyond
/// `cap`, so a payload never allocates more than its limit.
///
/// Overflow must not be reported as `ErrorKind::Interrupted`: the default `write_all`, which
/// rmp and rmp-serde use, retries those forever. `write_all` is overridden anyway, sparing the
/// retry loop on every small write.
pub(crate) struct Bounded<'a> {
    pub(crate) buf: &'a mut Vec<u8>,
    pub(crate) max: usize,
    pub(crate) cap: usize,
    pub(crate) full: bool,
}

impl<'a> Bounded<'a> {
    pub(crate) fn new(buf: &'a mut Vec<u8>, max: usize, cap: usize) -> Self {
        Self {
            buf,
            max,
            cap,
            full: false,
        }
    }

    #[cold]
    fn grow(&mut self, needed: usize) {
        let capacity = needed
            .max(self.buf.capacity().saturating_mul(2))
            .max(1024)
            .min(self.cap.max(needed));
        self.buf.reserve_exact(capacity - self.buf.len());
    }
}

impl io::Write for Bounded<'_> {
    #[inline]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_all(bytes)?;
        Ok(bytes.len())
    }

    #[inline]
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > self.max.saturating_sub(self.buf.len()) {
            self.full = true;
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        let needed = self.buf.len() + bytes.len();
        if needed > self.buf.capacity() {
            self.grow(needed);
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encoded size of a msgpack array or map header for `len` elements.
pub(crate) fn header_len(len: usize) -> usize {
    match len {
        0..16 => 1,
        16..=0xffff => 3,
        _ => 5,
    }
}

/// Encoded size of a msgpack string of `len` bytes.
pub(crate) fn str_len(len: usize) -> usize {
    len + match len {
        0..32 => 1,
        32..=0xff => 2,
        0x100..=0xffff => 3,
        _ => 5,
    }
}

/// Moves everything `buf` gained since it was `len` bytes long to its front, in place.
pub(crate) fn rotate_tail_to_front(buf: &mut [u8], len: usize) {
    let tail = buf.len() - len;
    buf.rotate_right(tail);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn header_len_matches_rmp() {
        for len in [0, 15, 16, 0xffff, 0x1_0000] {
            let mut buf = Vec::new();
            rmp::encode::write_array_len(&mut buf, len as u32).unwrap();
            assert_eq!(header_len(len), buf.len(), "array of {len}");
            buf.clear();
            rmp::encode::write_map_len(&mut buf, len as u32).unwrap();
            assert_eq!(header_len(len), buf.len(), "map of {len}");
        }
    }

    #[test]
    fn str_len_matches_rmp() {
        for len in [0, 31, 32, 255, 256, 0xffff, 0x1_0000] {
            let mut buf = Vec::new();
            rmp::encode::write_str(&mut buf, &"x".repeat(len)).unwrap();
            assert_eq!(str_len(len), buf.len(), "string of {len}");
        }
    }

    #[test]
    fn overflow_is_not_retried_and_keeps_capacity_bounded() {
        let mut buf = Vec::new();
        let mut writer = Bounded::new(&mut buf, 4, 6);
        writer.write_all(b"abc").unwrap();
        // `Interrupted` would make `write_all` loop forever on a sticky condition.
        let error = writer.write_all(b"de").unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
        assert!(writer.full);
        assert_eq!(buf, b"abc");
        assert!(buf.capacity() <= 6);
    }
}
