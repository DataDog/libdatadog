// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use libdd_http_client::HttpClientError as UpstreamError;
use std::ffi::{CString, c_char};
use std::fmt::{self, Display, Formatter};

/// Why a call failed.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HttpClientErrorCode {
    /// A `NULL` pointer, a non-UTF-8 string, or a malformed request.
    InvalidArgument,
    /// The client configuration was rejected.
    InvalidConfig,
    /// The connection could not be established.
    ConnectionFailed,
    /// The request exceeded its timeout.
    TimedOut,
    /// The request's cancellation token was cancelled.
    Cancelled,
    /// Any other I/O error.
    Io,
    /// The runtime could not run the request.
    Runtime,
    /// A panic was caught at the FFI boundary.
    #[cfg(feature = "catch_panic")]
    Panic,
}

impl Display for HttpClientErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument => f.write_str("Invalid argument provided"),
            Self::InvalidConfig => f.write_str("Invalid client configuration"),
            Self::ConnectionFailed => f.write_str("Connection failed"),
            Self::TimedOut => f.write_str("Operation timed out"),
            Self::Cancelled => f.write_str("Operation cancelled"),
            Self::Io => f.write_str("Input/Output error"),
            Self::Runtime => f.write_str("Runtime error"),
            #[cfg(feature = "catch_panic")]
            Self::Panic => f.write_str("Panic caught at the FFI boundary"),
        }
    }
}

/// Error of a fallible call, where `NULL` means success. Release it with
/// [`ddog_http_client_error_free`].
#[repr(C)]
#[derive(Debug)]
pub struct HttpClientError {
    pub code: HttpClientErrorCode,
    pub message: *mut c_char,
}

impl HttpClientError {
    pub(crate) fn new(code: HttpClientErrorCode, message: &str) -> Self {
        Self {
            code,
            message: CString::new(message).unwrap_or_default().into_raw(),
        }
    }
}

impl Drop for HttpClientError {
    fn drop(&mut self) {
        if !self.message.is_null() {
            // SAFETY: `the caller must ensure that `HttpClientError` has been created through its
            // `new` method which ensures that `msg` property is originated from
            // `Cstring::into_raw` call. Any other possibility could lead to UB.
            unsafe {
                drop(CString::from_raw(self.message));
                self.message = std::ptr::null_mut();
            }
        }
    }
}

impl From<UpstreamError> for HttpClientError {
    fn from(value: UpstreamError) -> Self {
        match value {
            UpstreamError::ConnectionFailed(message) => {
                Self::new(HttpClientErrorCode::ConnectionFailed, &message)
            }
            UpstreamError::TimedOut => {
                Self::new(HttpClientErrorCode::TimedOut, "request timed out")
            }
            UpstreamError::InvalidConfig(message) => {
                Self::new(HttpClientErrorCode::InvalidConfig, &message)
            }
            UpstreamError::IoError(message) => Self::new(HttpClientErrorCode::Io, &message),
            UpstreamError::RequestFailed { status, body } => Self::new(
                HttpClientErrorCode::Io,
                &format!("unexpected HTTP error status {status}: {body}"),
            ),
        }
    }
}

/// Frees an error. Passing `NULL` is a no-op.
///
/// # Safety
///
/// `error` must be `NULL` or a live error pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_error_free(error: Option<Box<HttpClientError>>) {
    catch_panic!(drop(error), ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    fn message(error: &HttpClientError) -> &str {
        unsafe { CStr::from_ptr(error.message) }
            .to_str()
            .expect("a UTF-8 message")
    }

    #[cfg(feature = "catch_panic")]
    #[test]
    fn gen_error_uses_the_description_of_the_code() {
        use crate::gen_error;

        let error = gen_error!(HttpClientErrorCode::Panic).expect("an error");

        assert_eq!(error.code, HttpClientErrorCode::Panic);
        assert_eq!(message(&error), "Panic caught at the FFI boundary");
    }

    #[test]
    fn upstream_errors_keep_their_message_and_get_a_code() {
        for (upstream, code, text) in [
            (
                UpstreamError::ConnectionFailed("refused".to_owned()),
                HttpClientErrorCode::ConnectionFailed,
                "refused",
            ),
            (
                UpstreamError::InvalidConfig("bad".to_owned()),
                HttpClientErrorCode::InvalidConfig,
                "bad",
            ),
            (
                UpstreamError::IoError("broken pipe".to_owned()),
                HttpClientErrorCode::Io,
                "broken pipe",
            ),
        ] {
            let error = HttpClientError::from(upstream);

            assert_eq!(error.code, code);
            assert_eq!(message(&error), text);
        }
    }

    #[test]
    fn an_upstream_timeout_is_a_timed_out_error() {
        let error = HttpClientError::from(UpstreamError::TimedOut);

        assert_eq!(error.code, HttpClientErrorCode::TimedOut);
    }

    #[test]
    fn an_unexpected_http_status_is_an_io_error_naming_the_status() {
        let error = HttpClientError::from(UpstreamError::RequestFailed {
            status: 503,
            body: "down".to_owned(),
        });

        assert_eq!(error.code, HttpClientErrorCode::Io);
        assert!(message(&error).contains("503"), "{}", message(&error));
    }

    #[test]
    fn a_message_with_an_interior_nul_becomes_empty() {
        let error = HttpClientError::new(HttpClientErrorCode::Io, "a\0b");

        assert_eq!(message(&error), "");
    }

    #[test]
    fn freeing_a_null_error_is_a_no_op() {
        unsafe { ddog_http_client_error_free(None) };
    }
}
