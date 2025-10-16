// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Read, Write};

/// This type wraps a [`Vec`] to provide a [`Write`] interface that has a max
/// capacity that won't be exceeded. Additionally, it gracefully handles
/// out-of-memory conditions instead of panicking (unfortunately not compatible
/// with the `no-panic` crate, though).
pub struct SizeRestrictedBuffer {
    vec: Vec<u8>,
    max_capacity: usize,
}

impl SizeRestrictedBuffer {
    /// Returns a buffer which can never hold any data.
    pub const fn zero_capacity() -> Self {
        Self {
            vec: Vec::new(),
            max_capacity: 0,
        }
    }

    /// Tries to create an initial buffer with the provided size hint as well
    /// as the provided max capacity. Neither number is required to be a power
    /// of 2.
    ///
    /// # Errors
    ///
    /// - Fails if the `size_hint` is larger than the `max_capacity`.
    /// - Fails if memory cannot be reserved.
    pub fn try_new(size_hint: usize, max_capacity: usize) -> io::Result<Self> {
        if size_hint > max_capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "size hint shouldn't be larger than max capacity",
            ));
        }
        let mut vec = Vec::new();
        vec.try_reserve(size_hint)?;
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

impl AsRef<[u8]> for SizeRestrictedBuffer {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
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

pub trait ProfileCodec {
    type Encoder: Write;

    fn new_encoder(
        size_hint: usize,
        max_capacity: usize,
        compression_level: i32,
    ) -> io::Result<Self::Encoder>;

    fn finish(encoder: Self::Encoder) -> io::Result<Vec<u8>>;
}

#[allow(unused)]
pub struct NoopProfileCodec;

impl ProfileCodec for NoopProfileCodec {
    type Encoder = SizeRestrictedBuffer;

    fn new_encoder(
        size_hint: usize,
        max_capacity: usize,
        _compression_level: i32,
    ) -> io::Result<Self::Encoder> {
        SizeRestrictedBuffer::try_new(size_hint, max_capacity)
    }

    fn finish(encoder: Self::Encoder) -> io::Result<Vec<u8>> {
        Ok(encoder.into())
    }
}

#[allow(unused)]
pub struct ZstdProfileCodec;

impl ProfileCodec for ZstdProfileCodec {
    type Encoder = zstd::Encoder<'static, SizeRestrictedBuffer>;

    fn new_encoder(
        size_hint: usize,
        max_capacity: usize,
        compression_level: i32,
    ) -> io::Result<Self::Encoder> {
        let buffer = SizeRestrictedBuffer::try_new(size_hint, max_capacity)?;
        zstd::Encoder::<'static, SizeRestrictedBuffer>::new(buffer, compression_level)
    }

    fn finish(encoder: Self::Encoder) -> io::Result<Vec<u8>> {
        match encoder.try_finish() {
            Ok(buffer) => Ok(buffer.into()),
            Err((_enc, error)) => Err(error),
        }
    }
}

#[cfg(not(miri))]
pub type DefaultProfileCodec = ZstdProfileCodec;
#[cfg(miri)]
pub type DefaultProfileCodec = NoopProfileCodec;

pub trait ObservationCodec {
    type Encoder: Write;
    type Decoder: Read;

    fn new_encoder(size_hint: usize, max_capacity: usize) -> io::Result<Self::Encoder>;
    fn encoder_into_decoder(encoder: Self::Encoder) -> io::Result<Self::Decoder>;
}

#[allow(unused)]
pub struct NoopObservationCodec;

impl ObservationCodec for NoopObservationCodec {
    type Encoder = SizeRestrictedBuffer;
    type Decoder = io::Cursor<SizeRestrictedBuffer>;

    fn new_encoder(size_hint: usize, max_capacity: usize) -> io::Result<Self::Encoder> {
        SizeRestrictedBuffer::try_new(size_hint, max_capacity)
    }

    fn encoder_into_decoder(encoder: Self::Encoder) -> io::Result<Self::Decoder> {
        Ok(io::Cursor::new(encoder))
    }
}

#[allow(unused)]
pub struct ZstdObservationCodec;

impl ObservationCodec for ZstdObservationCodec {
    type Encoder = zstd::Encoder<'static, SizeRestrictedBuffer>;
    type Decoder = zstd::Decoder<'static, io::Cursor<SizeRestrictedBuffer>>;

    fn new_encoder(size_hint: usize, max_capacity: usize) -> io::Result<Self::Encoder> {
        let buffer = SizeRestrictedBuffer::try_new(size_hint, max_capacity)?;
        zstd::Encoder::new(buffer, 1)
    }

    fn encoder_into_decoder(encoder: Self::Encoder) -> io::Result<Self::Decoder> {
        match encoder.try_finish() {
            Ok(buffer) => zstd::Decoder::with_buffer(io::Cursor::new(buffer)),
            Err((_enc, error)) => Err(error),
        }
    }
}

#[cfg(not(miri))]
pub type DefaultObservationCodec = ZstdObservationCodec;
#[cfg(miri)]
pub type DefaultObservationCodec = NoopObservationCodec;

/// Used to compress profile data.
pub struct Compressor<C: ProfileCodec = DefaultProfileCodec> {
    encoder: C::Encoder,
}

impl<C: ProfileCodec> Compressor<C> {
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
    ) -> io::Result<Compressor<C>> {
        Ok(Compressor {
            encoder: C::new_encoder(size_hint, max_capacity, compression_level)?,
        })
    }

    /// Finish the compression, and return the compressed data.
    pub fn finish(self) -> io::Result<Vec<u8>> {
        C::finish(self.encoder)
    }
}

impl<C: ProfileCodec> Write for Compressor<C> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.encoder.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.encoder.flush()
    }
}
