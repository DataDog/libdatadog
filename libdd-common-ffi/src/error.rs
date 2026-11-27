// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "std")]

use crate::slice::{AsBytes, CharSlice};
use crate::vec::Vec;
use core::fmt::{Debug, Display, Formatter};

// An empty message is the allocation-free out-of-memory sentinel. The static text is returned by
// ddog_Error_message, but is never stored in Error and therefore is never freed by Error_drop.
const OUT_OF_MEMORY_MESSAGE: &core::ffi::CStr = c"memory allocation failed";
const OUT_OF_MEMORY_CHAR_SLICE: CharSlice = unsafe {
    crate::Slice::from_raw_parts(
        OUT_OF_MEMORY_MESSAGE.as_ptr(),
        OUT_OF_MEMORY_MESSAGE.to_bytes().len(),
    )
};

/// Please treat this as opaque; do not reach into it, and especially don't
/// write into it! The most relevant APIs are:
/// * `ddog_Error_message`, to get the message as a slice.
/// * `ddog_Error_drop`.
#[derive(PartialEq, Eq)]
#[repr(C)]
pub struct Error {
    /// This is a String stuffed into the vec.
    message: Vec<u8>,
}

impl Error {
    /// Returns an out-of-memory error without allocating.
    pub const fn out_of_memory() -> Self {
        Self {
            message: Vec::new(),
        }
    }

    /// Formats an error into fallibly allocated storage. If formatting cannot allocate, returns the
    /// allocation-free out-of-memory error instead.
    pub fn from_display(value: impl Display) -> Self {
        Self::from_format_args(format_args!("{value}"))
    }

    fn is_out_of_memory(&self) -> bool {
        self.message.is_empty() && self.message.capacity() == 0
    }

    pub(crate) fn from_format_args(args: core::fmt::Arguments<'_>) -> Self {
        use core::fmt::Write;

        let mut writer = FallibleStringWriter::default();
        match writer.write_fmt(args) {
            Ok(()) => Self::from(writer.into_string()),
            Err(_) => Self::out_of_memory(),
        }
    }
}

#[derive(Default)]
struct FallibleStringWriter {
    buffer: String,
}

impl FallibleStringWriter {
    fn into_string(self) -> String {
        self.buffer
    }
}

impl core::fmt::Write for FallibleStringWriter {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.buffer
            .try_reserve(value.len())
            .map_err(|_| core::fmt::Error)?;
        self.buffer.push_str(value);
        Ok(())
    }
}

impl AsRef<str> for Error {
    fn as_ref(&self) -> &str {
        // Safety: .message is a String (just FFI safe).
        unsafe { self.message.as_slice().assume_utf8() }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_ref())
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("Error(\"{}\")", self.as_ref()))
    }
}

impl core::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        let message = Vec::from(value.into_bytes());
        Self { message }
    }
}

impl From<Error> for String {
    fn from(mut value: Error) -> String {
        let mut vec = Vec::default();
        core::mem::swap(&mut vec, &mut value.message);
        // Safety: .message is a String (just FFI safe).
        unsafe { String::from_utf8_unchecked(vec.into()) }
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self::from_display(value)
    }
}

impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        // {:#} is the "alternate" format, see:
        // https://docs.rs/anyhow/latest/anyhow/struct.Error.html#display-representations
        Self::from_format_args(format_args!("{value:#}"))
    }
}

impl From<Box<&dyn core::error::Error>> for Error {
    fn from(value: Box<&dyn core::error::Error>) -> Self {
        Self::from_display(value)
    }
}

/// Internal function to safely clear an error's contents
pub fn clear_error(err: &mut Error) {
    // Replacing the contents will drop the old message, freeing its
    // resources. The new one requires no allocations, so there's nothing
    // that needs dropped, but it's safe to be dropped.
    let message = Vec::new();
    *err = Error { message };
}

/// Clears the error. Heap-backed messages are freed; the allocation-free out-of-memory sentinel
/// owns no message storage. The error must not be read afterward. Clearing an already-cleared
/// error is a no-op.
///
/// # Safety
///
/// Only pass null or a pointer to a valid, mutable `ddog_Error`.
// allow(ffi-panic-boundary): clearing only drops an owned Vec and installs an empty sentinel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_Error_drop(error: Option<&mut Error>) {
    if let Some(err) = error {
        clear_error(err);
    }
}

/// Drops and frees a boxed error, such as the one returned by
/// `ddog_endpoint_from_api_key_and_site`. Null is accepted.
// allow(ffi-panic-boundary): only drops a `Box<Error>` holding a `Vec<u8>`, so it cannot panic.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_Error_free(error: Option<Box<Error>>) {
    drop(error);
}

/// Borrows the error message. The returned slice is valid until the error is cleared or dropped.
/// For an allocation-free out-of-memory error, the slice points to static storage.
///
/// # Safety
/// Only pass null or a valid reference to a `ddog_Error`.
// allow(ffi-panic-boundary): valid errors contain UTF-8 or the static OOM sentinel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_Error_message(error: Option<&Error>) -> CharSlice<'_> {
    match error {
        None => CharSlice::empty(),
        // The empty, zero-capacity error is the allocation-free out-of-memory sentinel.
        Some(err) => {
            if err.is_out_of_memory() {
                OUT_OF_MEMORY_CHAR_SLICE
            } else {
                CharSlice::from(err.as_ref())
            }
        }
    }
}

pub type MaybeError = crate::Option<Error>;

#[unsafe(no_mangle)]
pub extern "C" fn ddog_MaybeError_drop(_: MaybeError) {}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn out_of_memory_message_and_repeated_clear_do_not_allocate_or_deallocate() {
        let mut error = Error::out_of_memory();

        assert_no_alloc(|| {
            for _ in 0..2 {
                let message = unsafe { ddog_Error_message(Some(&error)) };
                assert_eq!(message.as_bytes(), OUT_OF_MEMORY_MESSAGE.to_bytes());
                unsafe { ddog_Error_drop(Some(&mut error)) };
            }
        });
    }

    #[test]
    fn clearing_an_ordinary_error_releases_its_message() {
        let mut error = Error::from(String::from("heap-backed message"));
        assert!(!error.message.is_empty());

        unsafe { ddog_Error_drop(Some(&mut error)) };

        assert_eq!(error, Error::out_of_memory());
    }
}
