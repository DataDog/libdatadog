// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use libdd_common_ffi::CharSlice;

/// Why a call failed. Callers are expected to branch on this, for example to tell a timeout from a
/// refused connection.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HttpClientErrorCode {
    /// A pointer argument was null, a string was not valid UTF-8, or a request was malformed
    /// (invalid header, relative URL without a leading `/`, both `body` and `multipart` set, ...).
    InvalidArgument,
    /// The client configuration was rejected (missing `base_url`, zero `timeout_ms`, ...).
    InvalidConfig,
    /// The connection to the server could not be established.
    ConnectionFailed,
    /// The request did not complete within its timeout.
    TimedOut,
    /// The request was cancelled through its [`crate::HttpCancellationToken`].
    Cancelled,
    /// Any other I/O error while sending the request or reading the response.
    Io,
    /// The runtime needed to drive the request could not be used.
    Runtime,
    /// A panic was caught at the FFI boundary.
    #[cfg(feature = "catch_panic")]
    Panic,
}

/// Error returned by the fallible functions of this crate. Opaque: use
/// [`ddog_http_client_error_code`] and [`ddog_http_client_error_message`], and release it with
/// [`ddog_http_client_error_free`]. A `NULL` return always means success.
#[derive(Debug)]
pub struct HttpClientError {
    code: HttpClientErrorCode,
    message: String,
}

impl HttpClientError {
    pub(crate) fn new(code: HttpClientErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Error value used when a panic is caught at the FFI boundary.
#[cfg(feature = "catch_panic")]
#[allow(dead_code)] // unused when `panic = "abort"` turns `catch_panic!` into a pass-through
pub(crate) fn panic_error() -> Option<Box<HttpClientError>> {
    Some(Box::new(HttpClientError::new(
        HttpClientErrorCode::Panic,
        "panic",
    )))
}

/// Error returned by the mocked entry points. Removed when the real implementation lands.
pub(crate) fn not_implemented(function: &str) -> Option<Box<HttpClientError>> {
    Some(Box::new(HttpClientError::new(
        HttpClientErrorCode::Runtime,
        format!("{function}: not implemented"),
    )))
}

/// Returns the code of an error.
///
/// # Safety
///
/// `error` must be a pointer returned by this library that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_error_code(
    error: *const HttpClientError,
) -> HttpClientErrorCode {
    catch_panic!(
        {
            // SAFETY: guaranteed by the caller, see the `# Safety` section.
            match unsafe { error.as_ref() } {
                Some(error) => error.code,
                None => HttpClientErrorCode::InvalidArgument,
            }
        },
        HttpClientErrorCode::Runtime
    )
}

/// Returns the human-readable message of an error, as UTF-8 without a trailing NUL. The slice is
/// valid until the error is freed.
///
/// # Safety
///
/// `error` must be a pointer returned by this library that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_error_message(
    error: *const HttpClientError,
) -> CharSlice<'static> {
    catch_panic!(
        {
            // SAFETY: guaranteed by the caller, see the `# Safety` section. The returned lifetime
            // is only nominal: the caller must not use the slice after freeing the error.
            match unsafe { error.as_ref() } {
                Some(error) => CharSlice::from(error.message.as_str()),
                None => CharSlice::default(),
            }
        },
        CharSlice::default()
    )
}

/// Frees an error. Passing `NULL` is a no-op. The pointer is invalid afterwards.
///
/// # Safety
///
/// `error` must be `NULL` or a pointer returned by this library that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_error_free(error: Option<Box<HttpClientError>>) {
    catch_panic!(drop(error), ())
}
