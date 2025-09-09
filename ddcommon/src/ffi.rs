// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// A trait to represent that the error type can be represented by a static
/// c-str while also being valid UTF-8.
///
/// # Safety
///
/// The strings returned by `as_ffi_str` must be valid UTF-8.
pub unsafe trait ThinError {
    fn as_ffi_str(&self) -> &'static std::ffi::CStr;
}

/// Converts an FFI-compatible [`ThinError`] to a Rust string.
pub fn error_as_rust_str<E: ThinError>(error: &E) -> &'static str {
    // Bytes will not contain the null terminator.
    let bytes = error.as_ffi_str().to_bytes();
    unsafe { std::str::from_utf8_unchecked(bytes) }
}
