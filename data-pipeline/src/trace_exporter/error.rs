// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::telemetry::error::TelemetryError;
use datadog_trace_utils::msgpack_decoder::decode::error::DecodeError;
use ddcommon::hyper_migration;
use hyper::http::StatusCode;
use hyper::Error as HyperError;
use rmp_serde::encode::Error as EncodeError;
use std::error::Error;
use std::fmt::{Debug, Display};

/// Context data for structured error information.
/// Contains key-value pairs that can be safely used for logging or debugging.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ErrorContext {
    fields: Vec<(String, String)>,
}

impl ErrorContext {
    /// Creates a new empty context.
    pub fn new() -> Self {
        Self { fields: Vec::new() }
    }

    /// Adds a key-value pair to the context.
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), value.into()));
        self
    }

    /// Returns all context fields as key-value pairs.
    pub fn fields(&self) -> &[(String, String)] {
        &self.fields
    }

    /// Checks if the context is empty.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Trait for errors that can provide template-based error messages.
pub trait ErrorTemplate {
    /// Returns a static error message template.
    /// May contain placeholders like {field_name} for structured data.
    fn template(&self) -> &'static str;

    /// Returns structured context data that can be used to populate templates.
    /// Default implementation returns empty context.
    fn context(&self) -> ErrorContext {
        ErrorContext::new()
    }
}

/// Represents different kinds of errors that can occur when interacting with the agent.
#[derive(Debug, PartialEq)]
pub enum AgentErrorKind {
    /// Indicates that the agent returned an empty response.
    EmptyResponse,
}

impl AgentErrorKind {
    const EMPTY_RESPONSE_TEMPLATE: &'static str = "Agent returned empty response";
}

impl Display for AgentErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentErrorKind::EmptyResponse => write!(f, "Agent empty response"),
        }
    }
}

impl ErrorTemplate for AgentErrorKind {
    fn template(&self) -> &'static str {
        Self::EMPTY_RESPONSE_TEMPLATE
    }
}

/// Represents different kinds of errors that can occur during the builder process.
#[derive(Debug, PartialEq)]
pub enum BuilderErrorKind {
    /// Represents an error when an invalid URI is provided.
    /// The associated `String` contains underlying error message.
    InvalidUri(String),
    /// Indicates that the telemetry configuration is invalid.
    InvalidTelemetryConfig(String),
    /// Indicates any incompatible configuration
    InvalidConfiguration(String),
}

impl BuilderErrorKind {
    const INVALID_URI_TEMPLATE: &'static str = "Invalid URI provided: {details}";
    const INVALID_TELEMETRY_CONFIG_TEMPLATE: &'static str =
        "Invalid telemetry configuration: {details}";
    const INVALID_CONFIGURATION_TEMPLATE: &'static str = "Invalid configuration: {details}";
}

impl Display for BuilderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuilderErrorKind::InvalidUri(msg) => write!(f, "Invalid URI: {msg}"),
            BuilderErrorKind::InvalidTelemetryConfig(msg) => {
                write!(f, "Invalid telemetry configuration: {msg}")
            }
            BuilderErrorKind::InvalidConfiguration(msg) => {
                write!(f, "Invalid configuration: {msg}")
            }
        }
    }
}

impl ErrorTemplate for BuilderErrorKind {
    fn template(&self) -> &'static str {
        match self {
            BuilderErrorKind::InvalidUri(_) => Self::INVALID_URI_TEMPLATE,
            BuilderErrorKind::InvalidTelemetryConfig(_) => Self::INVALID_TELEMETRY_CONFIG_TEMPLATE,
            BuilderErrorKind::InvalidConfiguration(_) => Self::INVALID_CONFIGURATION_TEMPLATE,
        }
    }

    fn context(&self) -> ErrorContext {
        match self {
            BuilderErrorKind::InvalidUri(details) => {
                ErrorContext::new().with_field("details", details)
            }
            BuilderErrorKind::InvalidTelemetryConfig(details) => {
                ErrorContext::new().with_field("details", details)
            }
            BuilderErrorKind::InvalidConfiguration(details) => {
                ErrorContext::new().with_field("details", details)
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

impl InternalErrorKind {
    const INVALID_WORKER_STATE_TEMPLATE: &'static str =
        "Background worker in invalid state: {details}";
}

impl Display for InternalErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InternalErrorKind::InvalidWorkerState(msg) => {
                write!(f, "Invalid worker state: {msg}")
            }
        }
    }
}

impl ErrorTemplate for InternalErrorKind {
    fn template(&self) -> &'static str {
        Self::INVALID_WORKER_STATE_TEMPLATE
    }

    fn context(&self) -> ErrorContext {
        match self {
            InternalErrorKind::InvalidWorkerState(details) => {
                ErrorContext::new().with_field("details", details)
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

impl NetworkErrorKind {
    const BODY_TEMPLATE: &'static str = "Error processing request/response body";
    const CANCELED_TEMPLATE: &'static str = "Request was canceled";
    const CONNECTION_CLOSED_TEMPLATE: &'static str = "Connection was closed";
    const MESSAGE_TOO_LARGE_TEMPLATE: &'static str = "Message size exceeds limit";
    const PARSE_TEMPLATE: &'static str = "Error parsing network response";
    const TIMED_OUT_TEMPLATE: &'static str = "Request timed out";
    const UNKNOWN_TEMPLATE: &'static str = "Unknown network error";
    const WRONG_STATUS_TEMPLATE: &'static str = "Unexpected status code received";
}

impl ErrorTemplate for NetworkErrorKind {
    fn template(&self) -> &'static str {
        match self {
            NetworkErrorKind::Body => Self::BODY_TEMPLATE,
            NetworkErrorKind::Canceled => Self::CANCELED_TEMPLATE,
            NetworkErrorKind::ConnectionClosed => Self::CONNECTION_CLOSED_TEMPLATE,
            NetworkErrorKind::MessageTooLarge => Self::MESSAGE_TOO_LARGE_TEMPLATE,
            NetworkErrorKind::Parse => Self::PARSE_TEMPLATE,
            NetworkErrorKind::TimedOut => Self::TIMED_OUT_TEMPLATE,
            NetworkErrorKind::Unknown => Self::UNKNOWN_TEMPLATE,
            NetworkErrorKind::WrongStatus => Self::WRONG_STATUS_TEMPLATE,
        }
    }
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

impl ErrorTemplate for NetworkError {
    fn template(&self) -> &'static str {
        self.kind.template()
    }

    fn context(&self) -> ErrorContext {
        self.kind.context()
    }
}

#[derive(Debug, PartialEq)]
pub struct RequestError {
    code: StatusCode,
    msg: String,
}

impl RequestError {
    const REQUEST_ERROR_TEMPLATE: &'static str =
        "Agent responded with error status {status_code}: {response}";
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

    pub fn msg(&self) -> &str {
        &self.msg
    }
}

impl ErrorTemplate for RequestError {
    fn template(&self) -> &'static str {
        Self::REQUEST_ERROR_TEMPLATE
    }

    fn context(&self) -> ErrorContext {
        ErrorContext::new()
            .with_field("status_code", self.code.as_u16().to_string())
            .with_field("response", &self.msg)
    }
}

#[derive(Debug)]
pub enum ShutdownError {
    TimedOut(std::time::Duration),
}

impl ShutdownError {
    const TIMED_OUT_TEMPLATE: &'static str =
        "Shutdown operation timed out after {timeout_seconds} seconds";
}

impl Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownError::TimedOut(dur) => {
                write!(f, "Shutdown timed out after {}s", dur.as_secs_f32())
            }
        }
    }
}

impl ErrorTemplate for ShutdownError {
    fn template(&self) -> &'static str {
        Self::TIMED_OUT_TEMPLATE
    }

    fn context(&self) -> ErrorContext {
        match self {
            ShutdownError::TimedOut(duration) => ErrorContext::new()
                .with_field("timeout_seconds", duration.as_secs_f32().to_string()),
        }
    }
}

/// Local ErrorTemplate implementation for DecodeError to avoid dependency on trace-utils.
impl ErrorTemplate for DecodeError {
    fn template(&self) -> &'static str {
        match self {
            DecodeError::InvalidConversion(_) => "Failed to convert decoded value: {details}",
            DecodeError::InvalidType(_) => "Invalid type in trace payload: {details}",
            DecodeError::InvalidFormat(_) => "Invalid msgpack format: {details}",
            DecodeError::IOError => "Failed to read from buffer",
            DecodeError::Utf8Error(_) => "Failed to decode UTF-8 string: {details}",
        }
    }

    fn context(&self) -> ErrorContext {
        match self {
            DecodeError::InvalidConversion(details) => {
                ErrorContext::new().with_field("details", details)
            }
            DecodeError::InvalidType(details) => ErrorContext::new().with_field("details", details),
            DecodeError::InvalidFormat(details) => {
                ErrorContext::new().with_field("details", details)
            }
            DecodeError::IOError => ErrorContext::new(),
            DecodeError::Utf8Error(details) => ErrorContext::new().with_field("details", details),
        }
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
        // Use template + context pattern for consistent formatting across all variants
        let template = self.template();
        let context = self.context();

        // Start with the template
        write!(f, "{}", template)?;

        // Add context data if available
        if !context.fields().is_empty() {
            write!(f, " (")?;
            for (i, (key, value)) in context.fields().iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: {}", key, value)?;
            }
            write!(f, ")")?;
        }

        Ok(())
    }
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
            TelemetryError::Builder(e) => {
                TraceExporterError::Builder(BuilderErrorKind::InvalidTelemetryConfig(e))
            }
            TelemetryError::Send(e) => TraceExporterError::Telemetry(e),
        }
    }
}

impl TraceExporterError {
    fn io_error_template(io_error: &std::io::Error) -> &'static str {
        match io_error.kind() {
            std::io::ErrorKind::NotFound => "File or resource not found",
            std::io::ErrorKind::PermissionDenied => "Permission denied",
            std::io::ErrorKind::ConnectionRefused => "Connection refused",
            std::io::ErrorKind::ConnectionReset => "Connection reset by peer",
            std::io::ErrorKind::ConnectionAborted => "Connection aborted",
            std::io::ErrorKind::TimedOut => "Operation timed out",
            std::io::ErrorKind::AddrInUse => "Address already in use",
            _ => "Input/Output error occurred",
        }
    }

    fn io_error_context(io_error: &std::io::Error) -> ErrorContext {
        let mut context = ErrorContext::new();

        // Add the raw OS error code if available
        if let Some(raw_os_error) = io_error.raw_os_error() {
            context = context.with_field("os_error_code", raw_os_error.to_string());
        }

        // Add the inner error message if it provides additional details
        let error_msg = io_error.to_string();
        if !error_msg.is_empty() && error_msg != io_error.kind().to_string() {
            context = context.with_field("details", &error_msg);
        }

        context
    }
}

impl ErrorTemplate for TraceExporterError {
    fn template(&self) -> &'static str {
        match self {
            TraceExporterError::Agent(e) => e.template(),
            TraceExporterError::Builder(e) => e.template(),
            TraceExporterError::Internal(e) => e.template(),
            TraceExporterError::Network(e) => e.template(),
            TraceExporterError::Request(e) => e.template(),
            TraceExporterError::Shutdown(e) => e.template(),
            TraceExporterError::Deserialization(e) => e.template(),
            TraceExporterError::Io(io_error) => Self::io_error_template(io_error),
            TraceExporterError::Telemetry(_) => "Telemetry operation failed: {details}",
            TraceExporterError::Serialization(_) => "Failed to serialize data: {details}",
        }
    }

    fn context(&self) -> ErrorContext {
        match self {
            TraceExporterError::Agent(e) => e.context(),
            TraceExporterError::Builder(e) => e.context(),
            TraceExporterError::Internal(e) => e.context(),
            TraceExporterError::Network(e) => e.context(),
            TraceExporterError::Request(e) => e.context(),
            TraceExporterError::Shutdown(e) => e.context(),
            TraceExporterError::Deserialization(e) => e.context(),
            TraceExporterError::Io(io_error) => Self::io_error_context(io_error),
            TraceExporterError::Telemetry(msg) => {
                ErrorContext::new().with_field("details", msg.as_str())
            }
            TraceExporterError::Serialization(encode_error) => {
                ErrorContext::new().with_field("details", encode_error.to_string())
            }
        }
    }
}

impl TraceExporterError {
    /// Returns the static error message template.
    pub fn template(&self) -> &'static str {
        ErrorTemplate::template(self)
    }

    /// Returns structured context data for the error.
    pub fn context(&self) -> ErrorContext {
        ErrorTemplate::context(self)
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
        assert_eq!(error.msg(), "Not found")
    }

    #[test]
    fn test_error_context() {
        let context = ErrorContext::new();
        assert!(context.is_empty());
        assert_eq!(context.fields().len(), 0);

        let context = ErrorContext::new()
            .with_field("key1", "value1")
            .with_field("key2", "value2");

        assert!(!context.is_empty());
        assert_eq!(context.fields().len(), 2);
        assert_eq!(
            context.fields()[0],
            ("key1".to_string(), "value1".to_string())
        );
        assert_eq!(
            context.fields()[1],
            ("key2".to_string(), "value2".to_string())
        );
    }

    #[test]
    fn test_agent_error_template() {
        let error = AgentErrorKind::EmptyResponse;
        assert_eq!(error.template(), "Agent returned empty response");
        assert!(error.context().is_empty());
    }

    #[test]
    fn test_builder_error_template() {
        let error = BuilderErrorKind::InvalidUri("invalid://url".to_string());
        assert_eq!(error.template(), "Invalid URI provided: {details}");
        let context = error.context();
        assert_eq!(context.fields().len(), 1);
        assert_eq!(
            context.fields()[0],
            ("details".to_string(), "invalid://url".to_string())
        );

        let error = BuilderErrorKind::InvalidTelemetryConfig("missing field".to_string());
        assert_eq!(
            error.template(),
            "Invalid telemetry configuration: {details}"
        );
        let context = error.context();
        assert_eq!(context.fields().len(), 1);
        assert_eq!(
            context.fields()[0],
            ("details".to_string(), "missing field".to_string())
        );

        let error = BuilderErrorKind::InvalidConfiguration("bad setting".to_string());
        assert_eq!(error.template(), "Invalid configuration: {details}");
        let context = error.context();
        assert_eq!(context.fields().len(), 1);
        assert_eq!(
            context.fields()[0],
            ("details".to_string(), "bad setting".to_string())
        );
    }

    #[test]
    fn test_internal_error_template() {
        let error = InternalErrorKind::InvalidWorkerState("worker crashed".to_string());
        assert_eq!(
            error.template(),
            "Background worker in invalid state: {details}"
        );
        let context = error.context();
        assert_eq!(context.fields().len(), 1);
        assert_eq!(
            context.fields()[0],
            ("details".to_string(), "worker crashed".to_string())
        );
    }

    #[test]
    fn test_network_error_kind_templates() {
        assert_eq!(
            NetworkErrorKind::Body.template(),
            "Error processing request/response body"
        );
        assert_eq!(
            NetworkErrorKind::Canceled.template(),
            "Request was canceled"
        );
        assert_eq!(
            NetworkErrorKind::ConnectionClosed.template(),
            "Connection was closed"
        );
        assert_eq!(
            NetworkErrorKind::MessageTooLarge.template(),
            "Message size exceeds limit"
        );
        assert_eq!(
            NetworkErrorKind::Parse.template(),
            "Error parsing network response"
        );
        assert_eq!(NetworkErrorKind::TimedOut.template(), "Request timed out");
        assert_eq!(
            NetworkErrorKind::Unknown.template(),
            "Unknown network error"
        );
        assert_eq!(
            NetworkErrorKind::WrongStatus.template(),
            "Unexpected status code received"
        );

        // All network error kinds should have empty context by default
        assert!(NetworkErrorKind::Body.context().is_empty());
        assert!(NetworkErrorKind::Canceled.context().is_empty());
        assert!(NetworkErrorKind::ConnectionClosed.context().is_empty());
        assert!(NetworkErrorKind::MessageTooLarge.context().is_empty());
        assert!(NetworkErrorKind::Parse.context().is_empty());
        assert!(NetworkErrorKind::TimedOut.context().is_empty());
        assert!(NetworkErrorKind::Unknown.context().is_empty());
        assert!(NetworkErrorKind::WrongStatus.context().is_empty());
    }

    #[test]
    fn test_error_context_chaining() {
        let context = ErrorContext::new()
            .with_field("host", "example.com")
            .with_field("port", "443")
            .with_field("timeout", "5000");

        assert_eq!(context.fields().len(), 3);
        assert_eq!(
            context.fields()[0],
            ("host".to_string(), "example.com".to_string())
        );
        assert_eq!(context.fields()[1], ("port".to_string(), "443".to_string()));
        assert_eq!(
            context.fields()[2],
            ("timeout".to_string(), "5000".to_string())
        );
    }

    #[test]
    fn test_request_error_template() {
        let error = RequestError::new(StatusCode::NOT_FOUND, "Resource not found");
        assert_eq!(
            error.template(),
            "Agent responded with error status {status_code}: {response}"
        );
        let context = error.context();
        assert_eq!(context.fields().len(), 2);
        assert_eq!(
            context.fields()[0],
            ("status_code".to_string(), "404".to_string())
        );
        assert_eq!(
            context.fields()[1],
            ("response".to_string(), "Resource not found".to_string())
        );

        let error = RequestError::new(StatusCode::INTERNAL_SERVER_ERROR, "Server error");
        assert_eq!(
            error.template(),
            "Agent responded with error status {status_code}: {response}"
        );
        let context = error.context();
        assert_eq!(context.fields().len(), 2);
        assert_eq!(
            context.fields()[0],
            ("status_code".to_string(), "500".to_string())
        );
        assert_eq!(
            context.fields()[1],
            ("response".to_string(), "Server error".to_string())
        );
    }

    #[test]
    fn test_shutdown_error_template() {
        use std::time::Duration;

        let error = ShutdownError::TimedOut(Duration::from_secs(5));
        assert_eq!(
            error.template(),
            "Shutdown operation timed out after {timeout_seconds} seconds"
        );
        let context = error.context();
        assert_eq!(context.fields().len(), 1);
        assert_eq!(
            context.fields()[0],
            ("timeout_seconds".to_string(), "5".to_string())
        );

        let error = ShutdownError::TimedOut(Duration::from_millis(2500));
        assert_eq!(
            error.template(),
            "Shutdown operation timed out after {timeout_seconds} seconds"
        );
        let context = error.context();
        assert_eq!(context.fields().len(), 1);
        assert_eq!(
            context.fields()[0],
            ("timeout_seconds".to_string(), "2.5".to_string())
        );
    }
}
