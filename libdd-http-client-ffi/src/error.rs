// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C-ABI error fan-out for `libdd-http-client`.
//!
//! Mirrors `libdd_http_client::HttpClientError` as a flat C enum
//! (`DdogHttpClientErrorCode`) plus a struct (`DdogHttpClientError`) carrying
//! a human-readable message and, for `RequestFailed`, the upstream status
//! code and response body.

use libdd_http_client::HttpClientError;
use std::ffi::{c_char, CString};

/// Discriminant for [`DdogHttpClientError`].
///
/// Mirrors [`libdd_http_client::HttpClientError`] one-for-one.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DdogHttpClientErrorCode {
    /// The TCP/socket connection to the server could not be established.
    ConnectionFailed,
    /// The request exceeded the configured timeout duration.
    TimedOut,
    /// The server returned an HTTP error status code (4xx / 5xx).
    ///
    /// `status` and `body` on the carrier struct are populated.
    RequestFailed,
    /// The client / request configuration was invalid.
    InvalidConfig,
    /// An I/O error occurred during the request.
    IoError,
    /// A null pointer or other invalid argument was passed across the FFI.
    InvalidArgument,
}

/// FFI-safe error type for the HTTP client.
///
/// `msg` is always non-null and owned by the struct; it must be released
/// via [`ddog_http_client_error_free`]. For `RequestFailed`, `status` is
/// the HTTP status code and `body` (if non-null) is a `\0`-terminated
/// UTF-8 string with the response body. For other variants `status` is 0
/// and `body` is null.
#[repr(C)]
pub struct DdogHttpClientError {
    /// The error code discriminant.
    pub code: DdogHttpClientErrorCode,
    /// `\0`-terminated UTF-8 message describing the error.
    pub msg: *mut c_char,
    /// HTTP status (only populated when `code == RequestFailed`; 0 otherwise).
    pub status: u16,
    /// Response body (only populated when `code == RequestFailed`; null otherwise).
    pub body: *mut c_char,
}

impl DdogHttpClientError {
    /// Construct a non-`RequestFailed` error with just a message.
    pub(crate) fn new(code: DdogHttpClientErrorCode, msg: &str) -> Self {
        Self {
            code,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
            status: 0,
            body: std::ptr::null_mut(),
        }
    }

    /// Construct a `RequestFailed` error.
    pub(crate) fn request_failed(status: u16, body: &str) -> Self {
        let msg = format!("request failed with status {status}");
        let body_cstr = CString::new(body.as_bytes()).unwrap_or_default().into_raw();
        Self {
            code: DdogHttpClientErrorCode::RequestFailed,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
            status,
            body: body_cstr,
        }
    }

    /// Construct an `InvalidArgument` error with the given message.
    pub(crate) fn invalid_argument(msg: &str) -> Self {
        Self::new(DdogHttpClientErrorCode::InvalidArgument, msg)
    }
}

impl From<HttpClientError> for DdogHttpClientError {
    fn from(err: HttpClientError) -> Self {
        match err {
            HttpClientError::ConnectionFailed(msg) => {
                Self::new(DdogHttpClientErrorCode::ConnectionFailed, &msg)
            }
            HttpClientError::TimedOut => {
                Self::new(DdogHttpClientErrorCode::TimedOut, "request timed out")
            }
            HttpClientError::RequestFailed { status, body } => Self::request_failed(status, &body),
            HttpClientError::InvalidConfig(msg) => {
                Self::new(DdogHttpClientErrorCode::InvalidConfig, &msg)
            }
            HttpClientError::IoError(msg) => Self::new(DdogHttpClientErrorCode::IoError, &msg),
        }
    }
}

impl Drop for DdogHttpClientError {
    fn drop(&mut self) {
        // SAFETY: msg/body are produced by CString::into_raw in the
        // constructors. Drop reclaims the storage and clears the pointer
        // so a subsequent (incorrect) drop is a no-op.
        if !self.msg.is_null() {
            unsafe {
                drop(CString::from_raw(self.msg));
            }
            self.msg = std::ptr::null_mut();
        }
        if !self.body.is_null() {
            unsafe {
                drop(CString::from_raw(self.body));
            }
            self.body = std::ptr::null_mut();
        }
    }
}

/// Free a [`DdogHttpClientError`].
///
/// After this call the pointer is invalid and must not be used again.
///
/// # Safety
/// `error` must be `None` or have been produced by this crate's API and
/// not yet freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_error_free(error: Option<Box<DdogHttpClientError>>) {
    drop(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn from_connection_failed() {
        let err: DdogHttpClientError =
            HttpClientError::ConnectionFailed("refused".to_owned()).into();
        assert_eq!(err.code, DdogHttpClientErrorCode::ConnectionFailed);
        let msg = unsafe { CStr::from_ptr(err.msg).to_string_lossy().into_owned() };
        assert!(msg.contains("refused"));
        assert_eq!(err.status, 0);
        assert!(err.body.is_null());
    }

    #[test]
    fn from_timed_out() {
        let err: DdogHttpClientError = HttpClientError::TimedOut.into();
        assert_eq!(err.code, DdogHttpClientErrorCode::TimedOut);
    }

    #[test]
    fn from_request_failed() {
        let err: DdogHttpClientError = HttpClientError::RequestFailed {
            status: 503,
            body: "service down".to_owned(),
        }
        .into();
        assert_eq!(err.code, DdogHttpClientErrorCode::RequestFailed);
        assert_eq!(err.status, 503);
        assert!(!err.body.is_null());
        let body = unsafe { CStr::from_ptr(err.body).to_string_lossy().into_owned() };
        assert_eq!(body, "service down");
    }

    #[test]
    fn from_invalid_config() {
        let err: DdogHttpClientError = HttpClientError::InvalidConfig("missing url".to_owned()).into();
        assert_eq!(err.code, DdogHttpClientErrorCode::InvalidConfig);
    }

    #[test]
    fn from_io_error() {
        let err: DdogHttpClientError = HttpClientError::IoError("broken pipe".to_owned()).into();
        assert_eq!(err.code, DdogHttpClientErrorCode::IoError);
    }

    #[test]
    fn drop_releases_strings() {
        let err = DdogHttpClientError::request_failed(500, "boom");
        // Just exercising drop; ASAN/MIRI will catch leaks.
        drop(err);
    }

    #[test]
    fn free_handles_none() {
        unsafe { ddog_http_client_error_free(None) };
    }
}
