// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileError;
use datadog_alloc::Box;
use datadog_profiling_protobuf::{Record, Value};
use ddcommon_ffi as common;
use lz4_flex::frame::FrameEncoder;
use std::io::{self, Write};
use std::ptr;

/// This type wraps a [`Vec`] to provide a [`Write`] interface that has a max
/// capacity that won't be exceeded. Additionally, it gracefully handles
/// out-of-memory conditions instead of panicking (unfortunately not compatible
/// with the `no-panic` crate, though).
struct SizeRestrictedBuffer {
    vec: Vec<u8>,
    max_capacity: usize,
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
pub struct Encoder(FrameEncoder<SizeRestrictedBuffer>);

/// Used to compress profile data. Treat the encoder as opaque.
#[repr(C)]
pub struct Compressor {
    encoder: *mut Encoder,
}

impl Compressor {
    fn create_buffer(max_capacity: usize) -> SizeRestrictedBuffer {
        let mut vec = Vec::new();

        // Use about 1/4 of the requested capacity as the initial size.
        let initial_capacity = (max_capacity >> 2).next_power_of_two();

        // If this fails, then later allocations are likely to fail too. But
        // it's annoying in some cases to have a fallible constructor, so
        // we delay this.
        _ = vec.try_reserve(initial_capacity);
        SizeRestrictedBuffer { vec, max_capacity }
    }

    /// Creates a new compressor with the given max capacity for the output
    /// buffer. This capacity is for after compression, not the input.
    pub fn with_max_capacity(max_capacity: usize) -> Compressor {
        let encoder = FrameEncoder::new(Self::create_buffer(max_capacity));
        let boxed = Box::new(Encoder(encoder));
        let encoder = Box::into_raw(boxed);
        Compressor { encoder }
    }

    fn encoder(&mut self) -> Result<&mut Encoder, ProfileError> {
        let encoder = self.encoder;
        if !encoder.is_null() {
            Ok(unsafe { &mut *encoder })
        } else {
            Err(ProfileError::InvalidInput)
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
        data.encode(&mut self.encoder()?.0).map_err(ProfileError::from)
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
        let encoder = self.encoder()?;
        if let Err(err) = encoder.0.try_finish() {
            return Err(ProfileError::from(io::Error::from(err)));
        }

        // SAFETY: taking ownership, and will replace this later. It is
        // important that we do not return until the `write` is complete. This
        // code is not panic safe.
        let encoder = unsafe { self.encoder.read() };
        let buffer = encoder.0.into_inner();
        let new_buffer = Self::create_buffer(buffer.max_capacity);
        // SAFETY: replacing the value that was read earlier.
        // todo: lz4 allocates and doesn't have a fallible API, zstd may be
        //       better (at least at a glance, its constructor is a Result).
        unsafe { self.encoder.write(Encoder(FrameEncoder::new(new_buffer))) };
        Ok(buffer.vec)
    }
}

impl Drop for Compressor {
    fn drop(&mut self) {
        ffi_drop(self)
    }
}

fn ffi_drop(compressor: &mut Compressor) {
    let encoder = compressor.encoder;
    if !encoder.is_null() {
        drop(unsafe { Box::from_raw(encoder) });
        compressor.encoder = ptr::null_mut();
    }
}

#[must_use]
#[no_mangle]
pub extern "C" fn ddog_prof_Compressor_new(max_capacity: usize) -> Compressor {
    Compressor::with_max_capacity(max_capacity)
}

#[repr(C)]
pub enum CompressorFinishResult {
    Ok(common::Vec<u8>),
    Err(ProfileError),
}

impl From<CompressorFinishResult> for Result<Vec<u8>, ProfileError> {
    fn from(result: CompressorFinishResult) -> Self {
        match result {
            CompressorFinishResult::Ok(vec) => Ok(vec.into()),
            CompressorFinishResult::Err(err) => Err(err),
        }
    }
}

impl From<Result<Vec<u8>, ProfileError>> for CompressorFinishResult {
    fn from(result: Result<Vec<u8>, ProfileError>) -> Self {
        match result {
            Ok(ok) => CompressorFinishResult::Ok(ok.into()),
            Err(err) => CompressorFinishResult::Err(err),
        }
    }
}

/// # Safety
///
/// The `compressor` must be a valid pointer to a properly initialized
/// `Compressor` and not previously finished or dropped.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Compressor_finish(
    compressor: *mut Compressor,
) -> CompressorFinishResult {
    if let Some(compressor) = compressor.as_mut() {
        CompressorFinishResult::from(compressor.finish())
    } else {
        CompressorFinishResult::Err(ProfileError::InvalidInput)
    }
}

/// # Safety
///
/// The `compressor` must be a valid pointer that was returned by
/// `ddog_prof_Compressor_new` and not previously dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Compressor_drop(
    compressor: *mut Compressor,
) {
    if let Some(compressor) = compressor.as_mut() {
        ffi_drop(compressor);
    }
}
