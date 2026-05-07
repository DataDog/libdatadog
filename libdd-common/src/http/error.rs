// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Error types for the shared HTTP foundation.
//!
//! These types are portable (available on `wasm32` as well as native targets).
//! Implementations that depend on hyper / hyper-util are gated and live
//! alongside the types they bridge.

use core::fmt;
use std::convert::Infallible;

use thiserror::Error;

/// Categorisation of low-level HTTP client errors. Mirrors the conditions that
/// the underlying hyper / hyper-util layers expose, so consumers can decide
/// retry / drop / propagate behaviour without reaching into hyper directly.
#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    /// Failure parsing an HTTP message.
    Parse,
    /// Connection was closed before the response could be received.
    Closed,
    /// The request was canceled (e.g., the consumer dropped the future).
    Canceled,
    /// Response body ended before the declared length / chunked terminator.
    Incomplete,
    /// The request body finished writing prematurely.
    WriteAborted,
    /// Failure parsing the HTTP status line.
    ParseStatus,
    /// The request did not complete within the configured timeout.
    Timeout,
    /// Catch-all for errors that do not fit the categories above.
    Other,
}

/// Error wrapper around a hyper / hyper-util client error categorised by
/// [`ErrorKind`].
#[derive(Debug, Error)]
pub struct ClientError {
    pub(super) source: anyhow::Error,
    pub(super) kind: ErrorKind,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

impl ClientError {
    /// Returns the categorisation of this error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

/// Top-level error type produced by the shared HTTP foundation. Wraps a
/// [`ClientError`] when the failure originated in the underlying client and
/// `Other` for everything else.
#[derive(Debug)]
pub enum Error {
    /// Client-side failure (transport, timeout, parsing, …).
    Client(ClientError),
    /// Any other error not covered by [`Error::Client`].
    Other(anyhow::Error),
    /// Bridge for `Infallible` (used by `Body::poll_frame` for `Full`/`Empty`).
    Infallible(Infallible),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Client(e) => write!(f, "client error: {e}"),
            Error::Infallible(e) => match *e {},
            Error::Other(e) => write!(f, "other error: {e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Other(value.into())
    }
}

impl From<http::Error> for Error {
    fn from(value: http::Error) -> Self {
        Self::Other(value.into())
    }
}

impl std::error::Error for Error {}

// --- Native-only impls (require hyper / hyper-util) ---

#[cfg(not(target_arch = "wasm32"))]
impl From<hyper::Error> for ClientError {
    fn from(source: hyper::Error) -> Self {
        use ErrorKind::*;
        let kind = if source.is_canceled() {
            Canceled
        } else if source.is_parse() {
            Parse
        } else if source.is_parse_status() {
            ParseStatus
        } else if source.is_incomplete_message() {
            Incomplete
        } else if source.is_body_write_aborted() {
            WriteAborted
        } else if source.is_timeout() {
            Timeout
        } else if source.is_closed() {
            Closed
        } else {
            Other
        };
        Self {
            kind,
            source: source.into(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<super::client::HttpRequestError> for ClientError {
    fn from(err: super::client::HttpRequestError) -> Self {
        use std::error::Error as _;
        let kind = if let Some(source) = err.source().and_then(|s| s.downcast_ref::<Error>()) {
            match source {
                Error::Client(client_error) => client_error.kind,
                Error::Other(_) => ErrorKind::Other,
                Error::Infallible(infallible) => match *infallible {},
            }
        } else if err.is_connect() {
            ErrorKind::Closed
        } else {
            ErrorKind::Other
        };
        Self {
            source: err.into(),
            kind,
        }
    }
}
