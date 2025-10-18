// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileError;
use datadog_profiling_protobuf::{Record, Value};
use lz4_flex::frame::FrameEncoder;
use std::io::{self, Write};

/// This type wraps a [`Vec`] to provide a [`Write`] interface that has a max
/// capacity that won't be exceeded. Additionally, it gracefully handles
/// out-of-memory conditions instead of panicking (unfortunately not compatible
/// with the `no-panic` crate, though).
#[derive(Debug)]
pub struct SizeRestrictedBuffer {
    vec: Vec<u8>,
    max_capacity: usize,
}

impl SizeRestrictedBuffer {
    pub fn new(max_capacity: usize) -> Self {
        let mut vec = Vec::new();

        // Use about 1/4 of the requested capacity as the initial size, but
        // no more than 2 MiB to begin with. Basically, we assume very large
        // max_capacity are used for edge cases based on protocols or upload
        // limits, and don't make for good size hints.
        const MIB: usize = 1024 * 1024;
        let initial_capacity = (max_capacity >> 2).min(2 * MIB).next_power_of_two();

        // If this fails, then later allocations are likely to fail too. But
        // it's annoying in some cases to have a fallible constructor, so
        // we delay this.
        _ = vec.try_reserve(initial_capacity);
        SizeRestrictedBuffer { vec, max_capacity }
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

/// An opaque encoder which does the compression for the Compressor.
#[derive(Debug)]
pub struct Encoder(FrameEncoder<SizeRestrictedBuffer>);

/// Used to compress profile data.
#[derive(Debug)]
pub struct Compressor {
    encoder: Encoder,
}

impl Compressor {
    /// Creates a new compressor with the given max capacity for the output
    /// buffer. This capacity is for after compression, not the input.
    pub fn with_max_capacity(max_capacity: usize) -> Compressor {
        let encoder = FrameEncoder::new(SizeRestrictedBuffer::new(max_capacity));
        Compressor {
            encoder: Encoder(encoder),
        }
    }

    /// Encodes the record.
    ///
    /// # Errors
    ///
    ///  1. Fails if the compressor's encoder pointer is null.
    ///  2. Fails if the encoder fails, e.g., the output buffer is full.
    pub fn encode<P: Value, const F: u32, const O: bool>(
        &mut self,
        data: Record<P, F, O>,
    ) -> Result<(), ProfileError> {
        data.encode(&mut self.encoder.0).map_err(ProfileError::from)
    }

    /// Finish the compression, and return the compressed data. The compressor
    /// remains valid, has been cleared, and will use the same max  capacity
    /// as it was configured with before.
    ///
    /// # Errors
    ///
    ///  1. Fails if the compressor's encoder pointer is null.
    ///  2. Fails if the encoder fails, e.g., the output buffer is full.
    pub fn finish(&mut self) -> Result<Vec<u8>, ProfileError> {
        if let Err(err) = self.encoder.0.try_finish() {
            return Err(ProfileError::fmt(format_args!(
                "profile compressor failed to finish: {err}"
            )));
        }

        // Move out the current encoder, create a fresh one with the same cap,
        // and return the finished bytes from the old buffer.
        let old = core::mem::replace(
            &mut self.encoder,
            Encoder(FrameEncoder::new(SizeRestrictedBuffer::new(0))),
        );
        let buffer = old.0.into_inner();
        let max_capacity = buffer.max_capacity;
        self.encoder = Encoder(FrameEncoder::new(SizeRestrictedBuffer::new(max_capacity)));
        Ok(buffer.vec)
    }
}

impl Write for Compressor {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.encoder.0.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.encoder.0.flush()
    }
}
