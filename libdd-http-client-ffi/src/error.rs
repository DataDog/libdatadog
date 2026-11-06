// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use libdd_common_ffi::CharSlice;
use libdd_http_client::HttpClientError as InnerError;

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

/// Error of a fallible call, where `NULL` means success. Release it with
/// [`ddog_http_client_error_free`].
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

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(HttpClientErrorCode::InvalidArgument, message)
    }

    pub(crate) fn invalid_config(message: impl Into<String>) -> Self {
        Self::new(HttpClientErrorCode::InvalidConfig, message)
    }

    pub(crate) fn from_build(error: InnerError) -> Self {
        Self::from_inner(error, HttpClientErrorCode::InvalidConfig)
    }

    /// When sending, an invalid configuration can only come from the request.
    pub(crate) fn from_send(error: InnerError) -> Self {
        Self::from_inner(error, HttpClientErrorCode::InvalidArgument)
    }

    fn from_inner(error: InnerError, invalid_config_code: HttpClientErrorCode) -> Self {
        match error {
            InnerError::ConnectionFailed(message) => {
                Self::new(HttpClientErrorCode::ConnectionFailed, message)
            }
            InnerError::TimedOut => Self::new(HttpClientErrorCode::TimedOut, "request timed out"),
            InnerError::InvalidConfig(message) => Self::new(invalid_config_code, message),
            InnerError::IoError(message) => Self::new(HttpClientErrorCode::Io, message),
            // Unreachable: HTTP errors are returned as responses.
            InnerError::RequestFailed { status, body } => Self::new(
                HttpClientErrorCode::Io,
                format!("unexpected HTTP error status {status}: {body}"),
            ),
        }
    }
}

#[cfg(feature = "catch_panic")]
#[allow(dead_code)] // unused with `panic = "abort"`
pub(crate) fn panic_error() -> Option<Box<HttpClientError>> {
    Some(Box::new(HttpClientError::new(
        HttpClientErrorCode::Panic,
        "panic",
    )))
}

/// Returns the code of an error.
///
/// # Safety
///
/// `error` must be a live error pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_error_code(
    error: *const HttpClientError,
) -> HttpClientErrorCode {
    catch_panic!(
        match unsafe { error.as_ref() } {
            Some(error) => error.code,
            None => HttpClientErrorCode::InvalidArgument,
        },
        HttpClientErrorCode::Runtime
    )
}

/// Returns the message of an error, valid until the error is freed.
///
/// # Safety
///
/// `error` must be a live error pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_error_message(
    error: *const HttpClientError,
) -> CharSlice<'static> {
    catch_panic!(
        match unsafe { error.as_ref() } {
            Some(error) => CharSlice::from(error.message.as_str()),
            None => CharSlice::default(),
        },
        CharSlice::default()
    )
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
