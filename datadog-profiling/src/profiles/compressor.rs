// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Write};

/// This type wraps a [`Vec`] to provide a [`Write`] interface that has a max
/// capacity that won't be exceeded. Additionally, it gracefully handles
/// out-of-memory conditions instead of panicking (unfortunately not compatible
/// with the `no-panic` crate, though).
pub struct SizeRestrictedBuffer {
    vec: Vec<u8>,
    max_capacity: usize,
}

impl SizeRestrictedBuffer {
    pub fn try_new(size_hint: usize, max_capacity: usize) -> io::Result<Self> {
        if size_hint > max_capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "size hint shouldn't be larger than max capacity",
            ));
        }
        // Round up to the next power of 2, but don't exceed the max_capacity.
        let initial_capacity = size_hint.next_power_of_two().min(max_capacity);
        let mut vec = Vec::new();
        vec.try_reserve(initial_capacity)?;
        Ok(SizeRestrictedBuffer { vec, max_capacity })
    }

    pub fn as_slice(&self) -> &[u8] {
        self.vec.as_slice()
    }
}

impl From<SizeRestrictedBuffer> for Vec<u8> {
    fn from(buf: SizeRestrictedBuffer) -> Self {
        buf.vec
    }
}

impl Write for SizeRestrictedBuffer {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let additional = buf.len();
        if additional <= self.max_capacity.wrapping_sub(self.vec.len()) {
            self.vec.try_reserve(additional)?;
            self.vec.extend(buf);
            Ok(additional)
        } else {
            Err(io::ErrorKind::StorageFull.into())
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Used to compress profile data.
pub struct Compressor {
    encoder: zstd::Encoder<'static, SizeRestrictedBuffer>,
}

impl Compressor {
    /// Creates a new compressor with the provided configuration.
    ///
    /// - `size_hint`: beginning capacity for the output buffer. This is a hint for the starting
    ///   size, and the implementation may use something different.
    /// - `max_capacity`: the maximum size for the output buffer (hard limit).
    /// - `compression_level`: see [`zstd::Encoder::new`] for the valid range.
    pub fn try_new(
        size_hint: usize,
        max_capacity: usize,
        compression_level: i32,
    ) -> io::Result<Compressor> {
        let buffer = SizeRestrictedBuffer::try_new(size_hint, max_capacity)?;
        let encoder =
            zstd::Encoder::<'static, SizeRestrictedBuffer>::new(buffer, compression_level)?;
        Ok(Compressor { encoder })
    }

    /// Finish the compression, and return the compressed data. The compressor
    /// remains valid but is missing its encoder, so it will fail to encode
    /// data.
    ///
    /// # Errors
    ///
    ///  1. Fails if the compressor's encoder is missing.
    ///  2. Fails if the encoder fails, e.g., the output buffer is full.
    pub fn finish(self) -> io::Result<Vec<u8>> {
        match self.encoder.try_finish() {
            Ok(buffer) => Ok(buffer.vec),
            Err(err) => Err(err.1),
        }
    }
}

impl Write for Compressor {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let encoder = &mut self.encoder;
        encoder.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.encoder.flush()
    }
}
