// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common_ffi::slice::CharSlice;
use libdd_data_pipeline::trace_exporter::error::{
    BuilderErrorKind, NetworkErrorKind, TraceExporterError,
};
use std::fmt::Display;

/// Context field for structured error data.
/// Contains a key-value pair that can be safely used for logging or debugging.
#[repr(C)]
#[derive(Debug)]
pub struct ContextField {
    /// Key name for this context field
    pub key: CharSlice<'static>,
    /// Value for this context field
    pub value: CharSlice<'static>,
}

/// Represent error codes that `Error` struct can hold
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ExporterErrorCode {
    AddressInUse,
    ConnectionAborted,
    ConnectionRefused,
    ConnectionReset,
    HttpBodyFormat,
    HttpBodyTooLong,
    HttpClient,
    HttpEmptyBody,
    HttpParse,
    HttpServer,
    HttpUnknown,
    HttpWrongStatus,
    InvalidArgument,
    InvalidData,
    InvalidInput,
    InvalidUrl,
    IoError,
    NetworkUnknown,
    Serde,
    Shutdown,
    TimedOut,
    Telemetry,
    Internal,
    #[cfg(feature = "catch_panic")]
    Panic,
}

impl Display for ExporterErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddressInUse => write!(f, "Address already in use"),
            Self::ConnectionAborted => write!(f, "Connection aborted"),
            Self::ConnectionRefused => write!(f, "Connection refused"),
            Self::ConnectionReset => write!(f, "Connection reset by peer"),
            Self::HttpBodyFormat => write!(f, "Error parsing HTTP body"),
            Self::HttpBodyTooLong => write!(f, "HTTP body too long"),
            Self::HttpClient => write!(f, "HTTP error originated by client"),
            Self::HttpEmptyBody => write!(f, "HTTP empty body"),
            Self::HttpParse => write!(f, "Error while parsing HTTP message"),
            Self::HttpServer => write!(f, "HTTP error originated by server"),
            Self::HttpWrongStatus => write!(f, "HTTP wrong status number"),
            Self::HttpUnknown => write!(f, "HTTP unknown error"),
            Self::InvalidArgument => write!(f, "Invalid argument provided"),
            Self::InvalidData => write!(f, "Invalid data payload"),
            Self::InvalidInput => write!(f, "Invalid input"),
            Self::InvalidUrl => write!(f, "Invalid URL"),
            Self::IoError => write!(f, "Input/Output error"),
            Self::NetworkUnknown => write!(f, "Unknown network error"),
            Self::Serde => write!(f, "Serialization/Deserialization error"),
            Self::Shutdown => write!(f, "Shutdown timed out"),
            Self::TimedOut => write!(f, "Operation timed out"),
            Self::Telemetry => write!(f, "Telemetry error"),
            Self::Internal => write!(f, "Internal error"),
            #[cfg(feature = "catch_panic")]
            Self::Panic => write!(f, "Operation panicked"),
        }
    }
}

/// Structure that contains error information that `TraceExporter` API can return.
#[repr(C)]
#[derive(Debug)]
pub struct ExporterError {
    pub code: ExporterErrorCode,
    /// Static error message template
    pub msg_template: CharSlice<'static>,
    /// Vector of context fields
    pub context_fields: libdd_common_ffi::Vec<ContextField>,
}

impl ExporterError {
    /// Creates a new ExporterError with a static template and no context fields.
    ///
    /// # Arguments
    ///
    /// * `code` - The error code representing the type of error
    /// * `template` - A static string template for the error message
    pub fn new(code: ExporterErrorCode, template: &'static str) -> Self {
        Self {
            code,
            msg_template: CharSlice::from(template),
            context_fields: libdd_common_ffi::Vec::default(),
        }
    }

    /// Creates a new ExporterError with a static template and context fields.
    ///
    /// # Arguments
    ///
    /// * `code` - The error code representing the type of error
    /// * `template` - A static string template for the error message
    /// * `context_fields` - Vector of context fields containing structured error data
    pub fn with_template_and_context(
        code: ExporterErrorCode,
        template: &'static str,
        context_fields: std::vec::Vec<ContextField>,
    ) -> Self {
        Self {
            code,
            msg_template: CharSlice::from(template),
            context_fields: libdd_common_ffi::Vec::from(context_fields),
        }
    }
}

impl From<TraceExporterError> for ExporterError {
    fn from(value: TraceExporterError) -> Self {
        let code = match &value {
            TraceExporterError::Agent(_) => ExporterErrorCode::HttpEmptyBody,
            TraceExporterError::Builder(builder_error) => match builder_error {
                BuilderErrorKind::InvalidUri(_) => ExporterErrorCode::InvalidUrl,
                _ => ExporterErrorCode::InvalidArgument,
            },
            TraceExporterError::Internal(_) => ExporterErrorCode::Internal,
            TraceExporterError::Network(network_error) => match network_error.kind() {
                NetworkErrorKind::Body => ExporterErrorCode::HttpBodyFormat,
                NetworkErrorKind::Parse => ExporterErrorCode::HttpParse,
                NetworkErrorKind::TimedOut => ExporterErrorCode::TimedOut,
                NetworkErrorKind::WrongStatus => ExporterErrorCode::HttpWrongStatus,
                NetworkErrorKind::ConnectionClosed => ExporterErrorCode::ConnectionReset,
                NetworkErrorKind::MessageTooLarge => ExporterErrorCode::HttpBodyTooLong,
                NetworkErrorKind::Canceled => ExporterErrorCode::HttpClient,
                NetworkErrorKind::Unknown => ExporterErrorCode::NetworkUnknown,
            },
            TraceExporterError::Request(request_error) => {
                if request_error.status().is_client_error() {
                    ExporterErrorCode::HttpClient
                } else if request_error.status().is_server_error() {
                    ExporterErrorCode::HttpServer
                } else {
                    ExporterErrorCode::HttpUnknown
                }
            }
            TraceExporterError::Shutdown(_) => ExporterErrorCode::Shutdown,
            TraceExporterError::Deserialization(_) => ExporterErrorCode::InvalidData,
            TraceExporterError::Io(io_error) => match io_error.kind() {
                std::io::ErrorKind::ConnectionAborted => ExporterErrorCode::ConnectionAborted,
                std::io::ErrorKind::ConnectionRefused => ExporterErrorCode::ConnectionRefused,
                std::io::ErrorKind::ConnectionReset => ExporterErrorCode::ConnectionReset,
                std::io::ErrorKind::TimedOut => ExporterErrorCode::TimedOut,
                std::io::ErrorKind::AddrInUse => ExporterErrorCode::AddressInUse,
                _ => ExporterErrorCode::IoError,
            },
            TraceExporterError::Telemetry(_) => ExporterErrorCode::Telemetry,
            TraceExporterError::Serialization(_) => ExporterErrorCode::Serde,
        };

        let template = value.template();
        let context = value.context();

        // Leak context field strings into static lifetime for FFI safety.
        // These allocations will remain until process termination.
        let context_fields: std::vec::Vec<ContextField> = context
            .fields()
            .iter()
            .map(|(key, value)| {
                let key_leaked: &'static str = Box::leak(key.clone().into_boxed_str());
                let value_leaked: &'static str = Box::leak(value.clone().into_boxed_str());

                ContextField {
                    key: CharSlice::from(key_leaked),
                    value: CharSlice::from(value_leaked),
                }
            })
            .collect();

        ExporterError::with_template_and_context(code, template, context_fields)
    }
}

// Note: ExporterError does not implement Drop for context field strings.
// Context fields from From<TraceExporterError> use leaked strings for FFI safety.
// This results in a small memory leak, which is acceptable for error handling.
// The FfiVec and its ContextField structs are cleaned up automatically.
// msg_template is always a static reference and requires no cleanup.

/// Frees `error` and all its contents. After being called error will not point to a valid memory
/// address so any further actions on it could lead to undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_error_free(error: Option<Box<ExporterError>>) {
    if let Some(error) = error {
        drop(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common_ffi::slice::AsBytes;

    #[test]
    fn constructor_test() {
        let code = ExporterErrorCode::InvalidArgument;
        let template = "Invalid argument provided";
        let error = Box::new(ExporterError::new(code, template));

        assert_eq!(error.code, ExporterErrorCode::InvalidArgument);
        let template_str = error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Invalid argument provided");
        assert_eq!(error.context_fields.len(), 0);
    }

    #[test]
    fn destructor_test() {
        let code = ExporterErrorCode::InvalidArgument;
        let template = "Test template";
        let error = Box::new(ExporterError::new(code, template));

        assert_eq!(error.code, ExporterErrorCode::InvalidArgument);
        let template_str = error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Test template");

        unsafe { ddog_trace_exporter_error_free(Some(error)) };
    }

    #[test]
    fn template_and_context_test() {
        let code = ExporterErrorCode::InvalidUrl;
        let template = "Invalid URI provided";
        let context_fields = vec![ContextField {
            key: CharSlice::from("details"),
            value: CharSlice::from("invalid://url"),
        }];

        let error = Box::new(ExporterError::with_template_and_context(
            code,
            template,
            context_fields,
        ));

        assert_eq!(error.code, ExporterErrorCode::InvalidUrl);
        let template_str = error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Invalid URI provided");
        assert_eq!(error.context_fields.len(), 1);

        let context_field = &error.context_fields[0];
        let key_str = context_field.key.try_to_utf8().unwrap();
        let value_str = context_field.value.try_to_utf8().unwrap();
        assert_eq!(key_str, "details");
        assert_eq!(value_str, "invalid://url");

        unsafe { ddog_trace_exporter_error_free(Some(error)) };
    }

    #[test]
    fn from_trace_exporter_error_builder_test() {
        use libdd_data_pipeline::trace_exporter::error::{BuilderErrorKind, TraceExporterError};

        let builder_error =
            TraceExporterError::Builder(BuilderErrorKind::InvalidUri("bad://url".to_string()));
        let ffi_error = ExporterError::from(builder_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::InvalidUrl);
        let template_str = ffi_error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Invalid URI provided: {details}");
        assert_eq!(ffi_error.context_fields.len(), 1);

        let context_field = &ffi_error.context_fields[0];
        let key_str = context_field.key.try_to_utf8().unwrap();
        let value_str = context_field.value.try_to_utf8().unwrap();
        assert_eq!(key_str, "details");
        assert_eq!(value_str, "bad://url");
    }

    #[test]
    fn from_trace_exporter_error_network_test() {
        use libdd_data_pipeline::trace_exporter::error::TraceExporterError;
        use std::io::{Error as IoError, ErrorKind};

        let io_error = IoError::new(ErrorKind::ConnectionAborted, "Connection closed");
        let network_error = TraceExporterError::Io(io_error);
        let ffi_error = ExporterError::from(network_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::ConnectionAborted);
        let template_str = ffi_error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Connection aborted");
        assert!(ffi_error.context_fields.len() > 0);
    }

    #[test]
    fn from_trace_exporter_error_agent_test() {
        use libdd_data_pipeline::trace_exporter::error::{AgentErrorKind, TraceExporterError};

        let agent_error = TraceExporterError::Agent(AgentErrorKind::EmptyResponse);
        let ffi_error = ExporterError::from(agent_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::HttpEmptyBody);
        let template_str = ffi_error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Agent returned empty response");
        assert_eq!(ffi_error.context_fields.len(), 0);
    }

    #[test]
    fn from_trace_exporter_error_without_template_test() {
        use libdd_data_pipeline::trace_exporter::error::TraceExporterError;
        use std::io::{Error as IoError, ErrorKind};

        let io_error =
            TraceExporterError::Io(IoError::new(ErrorKind::PermissionDenied, "Access denied"));
        let ffi_error = ExporterError::from(io_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::IoError);
        let template_str = ffi_error.msg_template.try_to_utf8().unwrap();
        assert_eq!(template_str, "Permission denied");
        assert!(ffi_error.context_fields.len() > 0);
    }

    #[test]
    fn from_trace_exporter_error_memory_safety_test() {
        use libdd_data_pipeline::trace_exporter::error::{BuilderErrorKind, TraceExporterError};

        let builder_error = TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
            "Missing service name".to_string(),
        ));
        let ffi_error = Box::new(ExporterError::from(builder_error));

        assert_eq!(ffi_error.context_fields.len(), 1);

        unsafe { ddog_trace_exporter_error_free(Some(ffi_error)) };
    }
}
