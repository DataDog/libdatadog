// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Errors detected while validating or sizing an HTTP request.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildError {
    /// The `Host` header value is empty or contains bytes that are not valid in this crate's
    /// conservative HTTP header subset.
    #[error("invalid HTTP host")]
    InvalidHost,
    /// The request path is not an origin-form path or contains unsupported bytes.
    #[error("invalid HTTP path")]
    InvalidPath,
    /// A header name is empty or contains a byte outside the HTTP token grammar.
    #[error("invalid HTTP header name")]
    InvalidHeaderName,
    /// A header value contains a byte outside the accepted visible ASCII / horizontal-tab subset.
    #[error("invalid HTTP header value")]
    InvalidHeaderValue,
    /// The encoded request length overflowed `usize`.
    #[error("encoded HTTP request length overflowed")]
    LengthOverflow,
    /// The caller provided more headers than the fixed reqwless header buffer can hold.
    #[error("too many HTTP headers; maximum supported header count is {max}")]
    TooManyHeaders {
        /// Maximum number of headers accepted by this crate's reqwless wrapper.
        max: usize,
    },
    /// An owned allocation failed while using an `alloc`-backed convenience API.
    #[error("failed to allocate HTTP request buffer")]
    AllocationFailed,
}

/// Errors returned while writing a request into a sink.
#[derive(Debug, thiserror::Error)]
pub enum SendError<E> {
    /// Request validation or sizing failed before the request was fully emitted.
    #[error(transparent)]
    Build(BuildError),
    /// The caller-provided sink rejected a write.
    #[error("HTTP sink write failed: {0}")]
    Sink(E),
    /// reqwless rejected or failed to emit the request.
    #[error("reqwless HTTP request emission failed: {0:?}")]
    Reqwless(reqwless::Error),
    /// reqwless yielded while writing to a sink that is expected to complete immediately.
    #[error("reqwless HTTP request emission unexpectedly yielded pending")]
    Pending,
}

impl<E> From<BuildError> for SendError<E> {
    fn from(error: BuildError) -> Self {
        Self::Build(error)
    }
}
