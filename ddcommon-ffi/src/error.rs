// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::slice::CharSlice;
use crate::vec::Vec;
use std::fmt::{Display, Formatter};

/// Please treat this as opaque; do not reach into it, and especially don't
/// write into it!
#[derive(Debug)]
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

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        let message = Vec::from(value.into_bytes());
        Self { message }
    }
}

impl From<Error> for String {
    fn from(value: Error) -> String {
        // Safety: .message is a String (just FFI safe).
        unsafe { String::from_utf8_unchecked(value.message.into()) }
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        Self::from(value.to_string())
    }
}

impl From<Box<&dyn std::error::Error>> for Error {
    fn from(value: Box<&dyn std::error::Error>) -> Self {
        Self::from(value.to_string())
    }
}

/// # Safety
/// Only pass null or a valid reference to a `ddog_Error`.
#[no_mangle]
pub unsafe extern "C" fn ddog_Error_drop(error: Option<&mut Error>) {
    if let Some(err) = error {
        // Leave an empty String in place to help with double-free issues.
        let mut tmp = Error::from(String::new());
        std::mem::swap(&mut tmp, err);
        drop(tmp)
    }
}

/// Returns a CharSlice of the error's message that is valid until the error
/// is dropped.
/// # Safety
/// Only pass null or a valid reference to a `ddog_Error`.
#[no_mangle]
pub unsafe extern "C" fn ddog_Error_message(error: Option<&Error>) -> CharSlice {
    match error {
        None => CharSlice::default(),
        Some(err) => CharSlice::from(err.as_ref()),
    }
}
