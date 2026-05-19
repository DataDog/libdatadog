// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "telemetry")]
use crate::telemetry::error::TelemetryError;
use crate::trace_exporter::msgpack_decoder::decode::error::DecodeError;
use http::StatusCode;
use libdd_common::http_common;
use rmp_serde::encode::Error as EncodeError;
use std::error::Error;
use std::fmt::{Debug, Display};

/// Represents different kinds of errors that can occur when interacting with the agent.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentErrorKind {
    /// Indicates that the agent returned an empty response.
    EmptyResponse,
}

impl Display for AgentErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyResponse => write!(f, "Agent empty response"),
        }
    }
}

/// Represents different kinds of errors that can occur during the builder process.
#[derive(Debug, PartialEq, Eq)]
pub enum BuilderErrorKind {
    /// Represents an error when an invalid URI is provided.
    /// The associated `String` contains underlying error message.
    InvalidUri(String),
    /// Indicates that the telemetry configuration is invalid.
    InvalidTelemetryConfig(String),
    /// Indicates any incompatible configuration
    InvalidConfiguration(String),
}

impl Display for BuilderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUri(msg) => write!(f, "Invalid URI: {msg}"),
            Self::InvalidTelemetryConfig(msg) => {
                write!(f, "Invalid telemetry configuration: {msg}")
            }
            Self::InvalidConfiguration(msg) => {
                write!(f, "Invalid configuration: {msg}")
            }
        }
    }
}

/// Represents different kinds of internal errors.
#[derive(Debug, PartialEq, Eq)]
pub enum InternalErrorKind {
    /// Indicates that some background workers are in an invalid state. The associated `String`
    /// contains the error message.
    InvalidWorkerState(String),
}

impl Display for InternalErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidWorkerState(msg) => {
                write!(f, "Invalid worker state: {msg}")
            }
        }
    }
}

/// Represents different kinds of network errors.
#[derive(Copy, Clone, Debug)]
pub enum NetworkErrorKind {
    /// Indicates an error with the body of the request/response.
    Body,
    /// Indicates that the request was canceled.
    Canceled,
    /// Indicates that the connection was closed.
    ConnectionClosed,
    /// Indicates that the message is too large.
    MessageTooLarge,
    /// Indicates a parsing error.
    Parse,
    /// Indicates that the request timed out.
    TimedOut,
    /// Indicates an unknown error.
    Unknown,
    /// Indicates that the status code is incorrect.
    WrongStatus,
}

/// Represents a network error, containing the kind of error and the source error.
#[derive(Debug)]
pub struct NetworkError {
    kind: NetworkErrorKind,
    source: anyhow::Error,
}

impl Error for NetworkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.chain().next()
    }
}

impl NetworkError {
    fn new<E: Into<anyhow::Error>>(kind: NetworkErrorKind, source: E) -> Self {
        Self {
            kind,
            source: source.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> NetworkErrorKind {
        self.kind
    }
}

impl Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[allow(clippy::unwrap_used)]
        std::fmt::Display::fmt(self.source().unwrap(), f)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RequestError {
    code: StatusCode,
    msg: String,
}

impl Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            format_args!("Error code: {}, Response: {}", self.code, self.msg)
        )
    }
}

impl RequestError {
    #[must_use]
    pub fn new(code: StatusCode, msg: &str) -> Self {
        Self {
            code,
            msg: msg.to_owned(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.code
    }

    #[must_use]
    pub fn msg(&self) -> &str {
        &self.msg
    }
}

#[derive(Debug)]
pub enum ShutdownError {
    TimedOut(std::time::Duration),
}

impl Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut(dur) => {
                write!(f, "Shutdown timed out after {}s", dur.as_secs_f32())
            }
        }
    }
}

/// `TraceExporterError` holds different types of errors that occur when handling traces.
#[derive(Debug)]
pub enum TraceExporterError {
    /// Error in agent response processing.
    Agent(AgentErrorKind),
    /// Invalid builder input.
    Builder(BuilderErrorKind),
    /// Error internal to the trace exporter.
    Internal(InternalErrorKind),
    /// Error in deserialization of incoming trace payload.
    Deserialization(DecodeError),
    /// Generic IO error.
    Io(std::io::Error),
    // Shutdown as not succeeded after some time
    Shutdown(ShutdownError),
    /// Telemetry related error.
    Telemetry(String),
    /// Network related error (i.e. hyper error).
    Network(NetworkError),
    /// Agent responded with an error code.
    Request(RequestError),
    /// Error in serialization of processed trace payload.
    Serialization(EncodeError),
}

impl Display for TraceExporterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent(e) => write!(f, "Agent response processing: {e}"),
            Self::Builder(e) => write!(f, "Invalid builder input: {e}"),
            Self::Internal(e) => write!(f, "Internal: {e}"),
            Self::Deserialization(e) => {
                write!(f, "Deserialization of incoming payload: {e}")
            }
            Self::Io(e) => write!(f, "IO: {e}"),
            Self::Shutdown(e) => write!(f, "Shutdown: {e}"),
            Self::Telemetry(e) => write!(f, "Telemetry: {e}"),
            Self::Network(e) => write!(f, "Network: {e}"),
            Self::Request(e) => write!(f, "Agent responded with an error code: {e}"),
            Self::Serialization(e) => {
                write!(f, "Serialization of trace payload payload: {e}")
            }
        }
    }
}

impl From<EncodeError> for TraceExporterError {
    fn from(value: EncodeError) -> Self {
        Self::Serialization(value)
    }
}

impl From<http::uri::InvalidUri> for TraceExporterError {
    fn from(value: http::uri::InvalidUri) -> Self {
        Self::Builder(BuilderErrorKind::InvalidUri(value.to_string()))
    }
}

impl From<http_common::Error> for TraceExporterError {
    fn from(err: http_common::Error) -> Self {
        match err {
            http_common::Error::Client(e) => Self::from(e),
            http_common::Error::Other(e) => Self::Network(NetworkError {
                kind: NetworkErrorKind::Unknown,
                source: e,
            }),
            http_common::Error::Infallible(e) => match e {},
        }
    }
}

impl From<http_common::ClientError> for TraceExporterError {
    fn from(err: http_common::ClientError) -> Self {
        use http_common::ErrorKind;
        let network_kind = match err.kind() {
            ErrorKind::Closed => NetworkErrorKind::ConnectionClosed,
            ErrorKind::Parse => NetworkErrorKind::Parse,
            ErrorKind::Canceled => NetworkErrorKind::Canceled,
            ErrorKind::Incomplete | ErrorKind::WriteAborted => NetworkErrorKind::Body,
            ErrorKind::ParseStatus => NetworkErrorKind::WrongStatus,
            ErrorKind::Timeout => NetworkErrorKind::TimedOut,
            ErrorKind::Other => NetworkErrorKind::Unknown,
        };
        Self::Network(NetworkError::new(network_kind, err))
    }
}

impl From<DecodeError> for TraceExporterError {
    fn from(err: DecodeError) -> Self {
        Self::Deserialization(err)
    }
}

impl From<std::io::Error> for TraceExporterError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<http::Error> for TraceExporterError {
    fn from(err: http::Error) -> Self {
        Self::Network(NetworkError {
            kind: NetworkErrorKind::Parse,
            source: err.into(),
        })
    }
}

impl From<libdd_capabilities::HttpError> for TraceExporterError {
    fn from(err: libdd_capabilities::HttpError) -> Self {
        Self::Network(NetworkError {
            kind: match &err {
                libdd_capabilities::HttpError::Timeout => NetworkErrorKind::TimedOut,
                libdd_capabilities::HttpError::Network(_) => NetworkErrorKind::ConnectionClosed,
                libdd_capabilities::HttpError::ResponseBody(_) => NetworkErrorKind::Body,
                libdd_capabilities::HttpError::InvalidRequest(_) => NetworkErrorKind::Parse,
                libdd_capabilities::HttpError::Other(_) => NetworkErrorKind::Unknown,
            },
            source: anyhow::anyhow!("{}", err),
        })
    }
}

#[cfg(feature = "telemetry")]
impl From<TelemetryError> for TraceExporterError {
    fn from(value: TelemetryError) -> Self {
        match value {
            TelemetryError::Builder(e) => {
                Self::Builder(BuilderErrorKind::InvalidTelemetryConfig(e))
            }
            TelemetryError::Send(e) => Self::Telemetry(e),
        }
    }
}

impl Error for TraceExporterError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_error() {
        let error = RequestError::new(StatusCode::NOT_FOUND, "Not found");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.msg(), "Not found");
    }
}
