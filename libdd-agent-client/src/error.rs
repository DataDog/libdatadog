// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Error types for [`crate::AgentClient`].

use bytes::Bytes;
use thiserror::Error;

/// Errors that can occur when building an [`crate::AgentClient`].
#[derive(Debug, Error)]
pub enum BuildError {
    /// No transport was configured.
    #[error("transport is required")]
    MissingTransport,
    /// No language metadata was configured.
    #[error("language metadata is required")]
    MissingLanguageMetadata,
    /// The underlying HTTP client could not be constructed.
    #[error("HTTP client error: {0}")]
    HttpClient(String),
}

/// Errors that can occur when sending a request via [`crate::AgentClient`].
#[derive(Debug, Error)]
pub enum SendError {
    /// Connection refused, timeout, or I/O error.
    #[error("transport error: {0}")]
    Transport(#[source] std::io::Error),
    /// The server returned an HTTP error status. Includes the raw status and body.
    #[error("HTTP error {status}: {body:?}")]
    HttpError {
        /// HTTP status code returned by the server.
        status: u16,
        /// Raw response body.
        body: Bytes,
    },
    /// All retry attempts exhausted without a successful response.
    #[error("retries exhausted: {last_error}")]
    RetriesExhausted {
        /// The last error encountered before giving up.
        last_error: Box<SendError>,
    },
    /// Payload serialisation or compression failure.
    #[error("encoding error: {0}")]
    Encoding(String),
}
