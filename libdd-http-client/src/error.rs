// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Error types for `libdd-http-client`.

use thiserror::Error;

/// Errors that can occur during HTTP client operations.
#[derive(Debug, Error)]
pub enum HttpClientError {
    /// The TCP/socket connection to the server could not be established.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// The request exceeded the configured timeout duration.
    #[error("request timed out")]
    TimedOut,

    /// The server returned an HTTP error status code.
    ///
    /// Only raised when `treat_http_errors_as_errors` is `true` (the default).
    /// The `body` field contains the response body as a UTF-8 string (lossy decoded).
    #[error("request failed with status {status}: {body}")]
    RequestFailed {
        /// The HTTP status code (e.g. 404, 503).
        status: u16,
        /// The response body, lossy-decoded as UTF-8.
        body: String,
    },

    /// The client configuration was invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// An I/O error occurred during the request.
    #[error("I/O error: {0}")]
    IoError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failed_display() {
        let err = HttpClientError::ConnectionFailed("refused".to_owned());
        assert_eq!(err.to_string(), "connection failed: refused");
    }

    #[test]
    fn timed_out_display() {
        let err = HttpClientError::TimedOut;
        assert_eq!(err.to_string(), "request timed out");
    }

    #[test]
    fn request_failed_display() {
        let err = HttpClientError::RequestFailed {
            status: 503,
            body: "service unavailable".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "request failed with status 503: service unavailable"
        );
    }

    #[test]
    fn invalid_config_display() {
        let err = HttpClientError::InvalidConfig("missing url".to_owned());
        assert_eq!(err.to_string(), "invalid configuration: missing url");
    }

    #[test]
    fn io_error_display() {
        let err = HttpClientError::IoError("broken pipe".to_owned());
        assert_eq!(err.to_string(), "I/O error: broken pipe");
    }
}
