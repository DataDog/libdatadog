// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::telemetry::error::TelemetryError;
use crate::trace_exporter::msgpack_decoder::decode::error::DecodeError;
use ddcommon::hyper_migration;
use hyper::http::StatusCode;
use hyper::Error as HyperError;
use rmp_serde::encode::Error as EncodeError;
use std::error::Error;
use std::fmt::{Debug, Display};

/// Represents different kinds of errors that can occur when interacting with the agent.
#[derive(Debug, PartialEq)]
pub enum AgentErrorKind {
    /// Indicates that the agent returned an empty response.
    EmptyResponse,
}

impl Display for AgentErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentErrorKind::EmptyResponse => write!(f, "Agent empty response"),
        }
    }
}

/// Represents different kinds of errors that can occur during the builder process.
#[derive(Debug, PartialEq)]
pub enum BuilderErrorKind {
    /// Represents an error when an invalid URI is provided.
    /// The associated `String` contains underlying error message.
    InvalidUri(String),
    /// Indicates that the telemetry configuration is invalid.
    InvalidTelemetryConfig,
    /// Indicates any incompatible configuration
    InvalidConfiguration(String),
}

impl Display for BuilderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuilderErrorKind::InvalidUri(msg) => write!(f, "Invalid URI: {}", msg),
            BuilderErrorKind::InvalidTelemetryConfig => {
                write!(f, "Invalid telemetry configuration")
            }
            BuilderErrorKind::InvalidConfiguration(msg) => {
                write!(f, "Invalid configuration: {}", msg)
            }
        }
    }
}

/// Represents different kinds of internal errors.
#[derive(Debug, PartialEq)]
pub enum InternalErrorKind {
    /// Indicates that some background workers are in an invalid state. The associated `String`
    /// contains the error message.
    InvalidWorkerState(String),
}

impl Display for InternalErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InternalErrorKind::InvalidWorkerState(msg) => {
                write!(f, "Invalid worker state: {}", msg)
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
    fn new_hyper(kind: NetworkErrorKind, source: HyperError) -> Self {
        Self {
            kind,
            source: source.into(),
        }
    }

    fn new_hyper_util(kind: NetworkErrorKind, source: hyper_util::client::legacy::Error) -> Self {
        Self {
            kind,
            source: source.into(),
        }
    }

    pub fn kind(&self) -> NetworkErrorKind {
        self.kind
    }
}

impl Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[allow(clippy::unwrap_used)]
        std::fmt::Display::fmt(self.source().unwrap(), f)
    }
}

#[derive(Debug, PartialEq)]
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
    pub fn new(code: StatusCode, msg: &str) -> Self {
        Self {
            code,
            msg: msg.to_owned(),
        }
    }

    pub fn status(&self) -> StatusCode {
        self.code
    }
}

/// TraceExporterError holds different types of errors that occur when handling traces.
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
    /// Network related error (i.e. hyper error).
    Network(NetworkError),
    /// Agent responded with an error code.
    Request(RequestError),
    /// Error in serialization of processed trace payload.
    Serialization(EncodeError),
}

impl From<EncodeError> for TraceExporterError {
    fn from(value: EncodeError) -> Self {
        TraceExporterError::Serialization(value)
    }
}

impl From<hyper::http::uri::InvalidUri> for TraceExporterError {
    fn from(value: hyper::http::uri::InvalidUri) -> Self {
        TraceExporterError::Builder(BuilderErrorKind::InvalidUri(value.to_string()))
    }
}

impl From<hyper_migration::Error> for TraceExporterError {
    fn from(err: hyper_migration::Error) -> Self {
        match err {
            hyper_migration::Error::Hyper(e) => e.into(),
            hyper_migration::Error::Other(e) => TraceExporterError::Network(NetworkError {
                kind: NetworkErrorKind::Unknown,
                source: e,
            }),
            hyper_migration::Error::Infallible(e) => match e {},
        }
    }
}

impl From<hyper_util::client::legacy::Error> for TraceExporterError {
    fn from(err: hyper_util::client::legacy::Error) -> Self {
        if err.is_connect() {
            return TraceExporterError::Network(NetworkError::new_hyper_util(
                NetworkErrorKind::ConnectionClosed,
                err,
            ));
        }
        if let Some(e) = err.source().and_then(|e| e.downcast_ref::<HyperError>()) {
            if e.is_parse() {
                return TraceExporterError::Network(NetworkError::new_hyper_util(
                    NetworkErrorKind::Parse,
                    err,
                ));
            } else if e.is_canceled() {
                return TraceExporterError::Network(NetworkError::new_hyper_util(
                    NetworkErrorKind::Canceled,
                    err,
                ));
            } else if e.is_incomplete_message() || e.is_body_write_aborted() {
                return TraceExporterError::Network(NetworkError::new_hyper_util(
                    NetworkErrorKind::Body,
                    err,
                ));
            } else if e.is_parse_status() {
                return TraceExporterError::Network(NetworkError::new_hyper_util(
                    NetworkErrorKind::WrongStatus,
                    err,
                ));
            } else if e.is_timeout() {
                return TraceExporterError::Network(NetworkError::new_hyper_util(
                    NetworkErrorKind::TimedOut,
                    err,
                ));
            }
        }
        TraceExporterError::Network(NetworkError::new_hyper_util(NetworkErrorKind::Unknown, err))
    }
}

impl From<HyperError> for TraceExporterError {
    fn from(err: HyperError) -> Self {
        if err.is_parse() {
            TraceExporterError::Network(NetworkError::new_hyper(NetworkErrorKind::Parse, err))
        } else if err.is_canceled() {
            TraceExporterError::Network(NetworkError::new_hyper(NetworkErrorKind::Canceled, err))
        } else if err.is_incomplete_message() || err.is_body_write_aborted() {
            TraceExporterError::Network(NetworkError::new_hyper(NetworkErrorKind::Body, err))
        } else if err.is_parse_status() {
            TraceExporterError::Network(NetworkError::new_hyper(NetworkErrorKind::WrongStatus, err))
        } else if err.is_timeout() {
            TraceExporterError::Network(NetworkError::new_hyper(NetworkErrorKind::TimedOut, err))
        } else {
            TraceExporterError::Network(NetworkError::new_hyper(NetworkErrorKind::Unknown, err))
        }
    }
}

impl From<DecodeError> for TraceExporterError {
    fn from(err: DecodeError) -> Self {
        TraceExporterError::Deserialization(err)
    }
}

impl From<std::io::Error> for TraceExporterError {
    fn from(err: std::io::Error) -> Self {
        TraceExporterError::Io(err)
    }
}

impl From<TelemetryError> for TraceExporterError {
    fn from(value: TelemetryError) -> Self {
        match value {
            TelemetryError::Builder(_) => {
                TraceExporterError::Builder(BuilderErrorKind::InvalidTelemetryConfig)
            }
            TelemetryError::Send(_) => {
                TraceExporterError::Io(std::io::ErrorKind::WouldBlock.into())
            }
        }
    }
}

impl Display for TraceExporterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceExporterError::Agent(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Builder(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Internal(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Deserialization(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Io(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Network(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Request(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Serialization(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl Error for TraceExporterError {}
