// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// A trait to represent a static error message that is represented by both
/// the requirements of Rust strings and C strings:
///
///  1. It must be a null terminated string with no interior null bytes.
///  2. It must be valid UTF-8.
///  3. It must not allocate to achieve the static bounds.
///
/// Using a c-str literal in Rust generally achieves all these requirements:
///
/// ```
/// c"this string is compatible with FfiSafeErrorMessage";
/// ```
///
/// # Safety
///
/// The strings returned by `as_ffi_str` must be valid UTF-8.
pub unsafe trait FfiSafeErrorMessage {
    /// Returns the error message as a static CStr. It must also be a valid
    /// Rust string, including being UTF-8.
    fn as_ffi_str(&self) -> &'static std::ffi::CStr;

    /// Returns the error message as a static Rust str, excluding the null
    /// terminator. If you need it, use [`FfiSafeErrorMessage::as_ffi_str`].
    ///
    /// Do not override this method, it would be marked final if it existed.
    fn as_rust_str(&self) -> &'static str {
        // Bytes will not contain the null terminator.
        let bytes = self.as_ffi_str().to_bytes();
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }
}
