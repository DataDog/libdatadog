// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::{AsBytes, CharSlice};
use crate::vec::Vec;
use std::fmt::{Debug, Display, Formatter};

/// You probably don't want to use this directly. This constant is used by `handle_panic_error` to
/// signal that something went wrong, but avoid needing any allocations to represent it.
pub(crate) const CANNOT_ALLOCATE_ERROR: Error = Error {
    message: Vec::new(),
};

// This error message is used as a placeholder for errors without message -- corresponding to an
// error where we couldn't even _allocate_ the message (or some other even weirder error).
const CANNOT_ALLOCATE: &std::ffi::CStr =
    c"libdatadog failed: (panic) Cannot allocate error message";
const CANNOT_ALLOCATE_CHAR_SLICE: CharSlice = unsafe {
    crate::Slice::from_raw_parts(
        CANNOT_ALLOCATE.as_ptr(),
        CANNOT_ALLOCATE.to_bytes_with_nul().len(),
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

impl AsRef<str> for Error {
    fn as_ref(&self) -> &str {
        // Safety: .message is a String (just FFI safe).
        unsafe { self.message.as_slice().assume_utf8() }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("Error(\"{}\")", self.as_ref()))
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        let message = Vec::from(value.into_bytes());
        Self { message }
    }
}

impl From<Error> for String {
    fn from(mut value: Error) -> String {
        let mut vec = Vec::default();
        std::mem::swap(&mut vec, &mut value.message);
        // Safety: .message is a String (just FFI safe).
        unsafe { String::from_utf8_unchecked(vec.into()) }
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        // {:#} is the "alternate" format, see:
        // https://docs.rs/anyhow/latest/anyhow/struct.Error.html#display-representations
        Self::from(format!("{value:#}"))
    }
}

impl From<Box<&dyn std::error::Error>> for Error {
    fn from(value: Box<&dyn std::error::Error>) -> Self {
        Self::from(value.to_string())
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

/// Drops the error. It should not be used after this, though the
/// implementation tries to limit the damage in the case of use-after-free and
/// double-free scenarios.
///
/// # Safety
///
/// Only pass null or a pointer to a valid, mutable `ddog_Error`.
#[no_mangle]
pub unsafe extern "C" fn ddog_Error_drop(error: Option<&mut Error>) {
    if let Some(err) = error {
        clear_error(err);
    }
}

/// Returns a CharSlice of the error's message that is valid until the error
/// is dropped.
/// # Safety
/// Only pass null or a valid reference to a `ddog_Error`.
#[no_mangle]
pub unsafe extern "C" fn ddog_Error_message(error: Option<&Error>) -> CharSlice<'_> {
    match error {
        None => CharSlice::empty(),
        // When the error is empty (CANNOT_ALLOCATE_ERROR) we assume we failed to allocate an actual
        // error and return this placeholder message instead.
        Some(err) => {
            if *err == CANNOT_ALLOCATE_ERROR {
                CANNOT_ALLOCATE_CHAR_SLICE
            } else {
                CharSlice::from(err.as_ref())
            }
        }
    }
}

pub type MaybeError = crate::Option<Error>;

#[no_mangle]
pub extern "C" fn ddog_MaybeError_drop(_: MaybeError) {}
