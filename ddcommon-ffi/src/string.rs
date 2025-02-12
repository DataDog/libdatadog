// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::CharSlice;
use crate::vec::Vec;
use crate::Error;

/// A wrapper for returning owned strings from FFI
#[derive(Debug)]
#[repr(C)]
pub struct StringWrapper {
    /// This is a String stuffed into the vec.
    message: Vec<u8>,
}

impl AsRef<str> for StringWrapper {
    fn as_ref(&self) -> &str {
        // Safety: .message is a String (just FFI safe).
        unsafe { std::str::from_utf8_unchecked(self.message.as_slice().as_slice()) }
    }
}

impl From<String> for StringWrapper {
    fn from(value: String) -> Self {
        let message = Vec::from(value.into_bytes());
        Self { message }
    }
}

impl From<StringWrapper> for String {
    fn from(mut value: StringWrapper) -> Self {
        let msg = std::mem::take(&mut value.message);
        String::from_utf8(msg.into()).unwrap()
    }
}

impl From<&str> for StringWrapper {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl Drop for StringWrapper {
    fn drop(&mut self) {
        // Leave an empty Vec, as it can help with use-after-free and double-free from C.
        let mut vec = Vec::default();
        std::mem::swap(&mut vec, &mut self.message);
        drop(vec);
    }
}

/// # Safety
/// Only pass null or a valid reference to a `ddog_StringWrapper`.
#[no_mangle]
pub unsafe extern "C" fn ddog_StringWrapper_drop(s: Option<&mut StringWrapper>) {
    if let Some(s) = s {
        // Safety: many other _drop functions need to re-box first, but StringWrapper
        // is repr(C) and not boxed, so it can be dropped in place. Of course,
        // C users must respect the StringWrapper requirements (treat as opaque, don't
        // reach in).
        std::ptr::drop_in_place(s as *mut _)
    }
}

/// Returns a CharSlice of the message that is valid until the StringWrapper
/// is dropped.
/// # Safety
/// Only pass null or a valid reference to a `ddog_StringWrapper`.
#[no_mangle]
pub unsafe extern "C" fn ddog_StringWrapper_message(s: Option<&StringWrapper>) -> CharSlice {
    match s {
        None => CharSlice::empty(),
        Some(s) => CharSlice::from(s.as_ref()),
    }
}

#[repr(C)]
#[allow(dead_code)]
pub enum StringWrapperResult {
    Ok(StringWrapper),
    Err(Error),
}

// Useful for testing
impl StringWrapperResult {
    pub fn unwrap(self) -> StringWrapper {
        match self {
            StringWrapperResult::Ok(s) => s,
            StringWrapperResult::Err(e) => panic!("{e}"),
        }
    }
}

impl From<anyhow::Result<String>> for StringWrapperResult {
    fn from(value: anyhow::Result<String>) -> Self {
        match value {
            Ok(x) => Self::Ok(x.into()),
            Err(err) => Self::Err(err.into()),
        }
    }
}

impl From<String> for StringWrapperResult {
    fn from(value: String) -> Self {
        Self::Ok(value.into())
    }
}
