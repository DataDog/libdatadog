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
        #[allow(clippy::unwrap_used)]
        String::from_utf8(msg.into()).unwrap()
    }
}

impl From<&str> for StringWrapper {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

/// Drops a `ddog_StringWrapper`. It should not be used after this, though the
/// implementation tries to limit the damage in the case of use-after-free and
/// double-free scenarios.
///
/// # Safety
///
/// Only pass null or a pointer to a valid, mutable `ddog_StringWrapper`.
#[no_mangle]
pub unsafe extern "C" fn ddog_StringWrapper_drop(s: Option<&mut StringWrapper>) {
    if let Some(s) = s {
        // Replacing the contents will drop the old string, freeing its
        // resources. The new one requires no allocations, so there's nothing
        // that needs dropped, but it's safe to be dropped.
        s.message = Default::default();
    }
}

/// Returns a CharSlice of the message.
///
/// # Safety
///
/// Only pass null or a valid reference to a `ddog_StringWrapper`.
/// The string should not be mutated nor dropped while the CharSlice is alive.
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
            #[allow(clippy::panic)]
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
