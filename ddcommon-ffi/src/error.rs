// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::CharSlice;
use crate::vec::Vec;
use std::fmt::{Debug, Display, Formatter};

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
        unsafe { std::str::from_utf8_unchecked(self.message.as_slice().as_slice()) }
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

impl Drop for Error {
    fn drop(&mut self) {
        // Leave an empty Vec, as it can help with use-after-free and double-free from C.
        let mut vec = Vec::default();
        std::mem::swap(&mut vec, &mut self.message);
        drop(vec);
    }
}

/// # Safety
/// Only pass null or a valid reference to a `ddog_Error`.
#[no_mangle]
pub unsafe extern "C" fn ddog_Error_drop(error: Option<&mut Error>) {
    if let Some(err) = error {
        // Safety: many other _drop functions need to re-box first, but Error
        // is repr(C) and not boxed, so it can be dropped in place. Of course,
        // C users must respect the Error requirements (treat as opaque, don't
        // reach in).
        std::ptr::drop_in_place(err as *mut _)
    }
}

/// Returns a CharSlice of the error's message that is valid until the error
/// is dropped.
/// # Safety
/// Only pass null or a valid reference to a `ddog_Error`.
#[no_mangle]
pub unsafe extern "C" fn ddog_Error_message(error: Option<&Error>) -> CharSlice {
    match error {
        None => CharSlice::empty(),
        Some(err) => CharSlice::from(err.as_ref()),
    }
}

pub type MaybeError = crate::Option<Error>;

#[no_mangle]
pub extern "C" fn ddog_MaybeError_drop(_: MaybeError) {}

#[macro_export]
macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => {
                return ddcommon_ffi::MaybeError::Some(ddcommon_ffi::Error::from(format!("{:?}", e)))
            }
        }
    };
}
