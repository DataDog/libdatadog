// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C-ABI error fan-out for `libdd-agent-client`.
//!
//! Mirrors `libdd_agent_client::{BuildError, SendError}` as a flat C
//! enum (`DdogAgentClientErrorCode`) plus a struct
//! (`DdogAgentClientError`) carrying a human-readable message and, for
//! `HttpError`, the upstream status code and response body.
//!
//! Shape mirrors `DdogHttpClientError` from `libdd-http-client-ffi`
//! intentionally — callers can write the same error-handling code for
//! both surfaces.

use libdd_agent_client::{BuildError, SendError};
use std::ffi::{c_char, CString};

/// Discriminant for [`DdogAgentClientError`].
///
/// Mirrors `libdd_agent_client::BuildError` and
/// `libdd_agent_client::SendError` one-for-one, plus an
/// `InvalidArgument` variant for FFI boundary violations.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DdogAgentClientErrorCode {
    /// `BuildError::MissingTransport` — no transport was configured on
    /// the builder.
    MissingTransport,
    /// `BuildError::MissingLanguageMetadata` — no language metadata was
    /// configured on the builder.
    MissingLanguageMetadata,
    /// `BuildError::HttpClient` — the underlying HTTP client could not
    /// be constructed.
    HttpClient,
    /// `SendError::Transport` — connection refused, timeout, or I/O
    /// error.
    Transport,
    /// `SendError::HttpError` — the server returned an HTTP error
    /// status. `status` and `body` on the carrier struct are populated.
    HttpError,
    /// `SendError::RetriesExhausted` — all retry attempts exhausted
    /// without a successful response.
    RetriesExhausted,
    /// `SendError::Encoding` — payload serialisation or compression
    /// failure.
    Encoding,
    /// A null pointer or otherwise invalid argument was passed across
    /// the FFI.
    InvalidArgument,
}

/// FFI-safe error type for the agent client.
///
/// `msg` is always non-null and owned by the struct; it must be
/// released via [`ddog_agent_client_error_free`]. For `HttpError`,
/// `status` is the HTTP status code and `body` (if non-null) is a
/// `\0`-terminated byte string with the response body. For other
/// variants `status` is 0 and `body` is null.
#[repr(C)]
pub struct DdogAgentClientError {
    /// The error code discriminant.
    pub code: DdogAgentClientErrorCode,
    /// `\0`-terminated UTF-8 message describing the error.
    pub msg: *mut c_char,
    /// HTTP status (only populated when `code == HttpError`; 0
    /// otherwise).
    pub status: u16,
    /// Response body (only populated when `code == HttpError`; null
    /// otherwise).
    pub body: *mut c_char,
}

impl DdogAgentClientError {
    /// Construct a non-`HttpError` error with just a message.
    pub(crate) fn new(code: DdogAgentClientErrorCode, msg: &str) -> Self {
        Self {
            code,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
            status: 0,
            body: std::ptr::null_mut(),
        }
    }

    /// Construct an `HttpError`.
    pub(crate) fn http_error(status: u16, body: &[u8]) -> Self {
        let msg = format!("HTTP error {status}");
        let body_cstr = CString::new(body).unwrap_or_default().into_raw();
        Self {
            code: DdogAgentClientErrorCode::HttpError,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
            status,
            body: body_cstr,
        }
    }

    /// Construct an `InvalidArgument` error.
    pub(crate) fn invalid_argument(msg: &str) -> Self {
        Self::new(DdogAgentClientErrorCode::InvalidArgument, msg)
    }
}

impl From<BuildError> for DdogAgentClientError {
    fn from(err: BuildError) -> Self {
        match err {
            BuildError::MissingTransport => Self::new(
                DdogAgentClientErrorCode::MissingTransport,
                "transport is required",
            ),
            BuildError::MissingLanguageMetadata => Self::new(
                DdogAgentClientErrorCode::MissingLanguageMetadata,
                "language metadata is required",
            ),
            BuildError::HttpClient(msg) => {
                Self::new(DdogAgentClientErrorCode::HttpClient, &msg)
            }
        }
    }
}

impl From<SendError> for DdogAgentClientError {
    fn from(err: SendError) -> Self {
        match err {
            SendError::Transport(io_err) => {
                Self::new(DdogAgentClientErrorCode::Transport, &io_err.to_string())
            }
            SendError::HttpError { status, body } => Self::http_error(status, body.as_ref()),
            SendError::RetriesExhausted { last_error } => Self::new(
                DdogAgentClientErrorCode::RetriesExhausted,
                &format!("retries exhausted: {last_error}"),
            ),
            SendError::Encoding(msg) => Self::new(DdogAgentClientErrorCode::Encoding, &msg),
        }
    }
}

impl Drop for DdogAgentClientError {
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

/// Free a [`DdogAgentClientError`].
///
/// After this call the pointer is invalid and must not be used again.
///
/// # Safety
/// `error` must be `None` or have been produced by this crate's API and
/// not yet freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_error_free(error: Option<Box<DdogAgentClientError>>) {
    drop(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::ffi::CStr;
    use std::io::{Error, ErrorKind};

    #[test]
    fn from_missing_transport() {
        let err: DdogAgentClientError = BuildError::MissingTransport.into();
        assert_eq!(err.code, DdogAgentClientErrorCode::MissingTransport);
        let msg = unsafe { CStr::from_ptr(err.msg).to_string_lossy().into_owned() };
        assert!(msg.contains("transport"));
    }

    #[test]
    fn from_missing_language_metadata() {
        let err: DdogAgentClientError = BuildError::MissingLanguageMetadata.into();
        assert_eq!(err.code, DdogAgentClientErrorCode::MissingLanguageMetadata);
    }

    #[test]
    fn from_http_client_build_error() {
        let err: DdogAgentClientError = BuildError::HttpClient("boom".into()).into();
        assert_eq!(err.code, DdogAgentClientErrorCode::HttpClient);
    }

    #[test]
    fn from_transport_send_error() {
        let err: DdogAgentClientError =
            SendError::Transport(Error::new(ErrorKind::ConnectionRefused, "no")).into();
        assert_eq!(err.code, DdogAgentClientErrorCode::Transport);
    }

    #[test]
    fn from_http_error_send_error() {
        let err: DdogAgentClientError = SendError::HttpError {
            status: 503,
            body: Bytes::from_static(b"boom"),
        }
        .into();
        assert_eq!(err.code, DdogAgentClientErrorCode::HttpError);
        assert_eq!(err.status, 503);
        assert!(!err.body.is_null());
        let body = unsafe { CStr::from_ptr(err.body).to_string_lossy().into_owned() };
        assert_eq!(body, "boom");
    }

    #[test]
    fn from_retries_exhausted_send_error() {
        let err: DdogAgentClientError = SendError::RetriesExhausted {
            last_error: Box::new(SendError::Encoding("boom".into())),
        }
        .into();
        assert_eq!(err.code, DdogAgentClientErrorCode::RetriesExhausted);
    }

    #[test]
    fn from_encoding_send_error() {
        let err: DdogAgentClientError = SendError::Encoding("boom".into()).into();
        assert_eq!(err.code, DdogAgentClientErrorCode::Encoding);
    }

    #[test]
    fn drop_releases_strings() {
        let err = DdogAgentClientError::http_error(500, b"boom");
        drop(err);
    }

    #[test]
    fn free_handles_none() {
        unsafe { ddog_agent_client_error_free(None) };
    }
}
