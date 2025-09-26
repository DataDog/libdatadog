// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::error::TraceExporterError;
use std::ffi::{c_char, CString};
use std::fmt::Display;

/// Context field for structured error data.
/// Contains a key-value pair that can be safely used for logging or debugging.
#[repr(C)]
#[derive(Debug)]
pub struct ContextField {
    /// Key name for this context field
    pub key: *const c_char,
    /// Value for this context field  
    pub value: *const c_char,
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
    pub msg_template: *const c_char,
    /// Array of context fields
    pub context_fields: *const ContextField,
    /// Number of context fields
    pub context_count: usize,
}

impl ExporterError {
    /// Creates a new ExporterError with a static template and no context fields.
    ///
    /// # Arguments
    ///
    /// * `code` - The error code representing the type of error
    /// * `template` - A static string template for the error message
    ///
    /// The returned error owns the template string and will free it when dropped.
    /// The template string is converted to a null-terminated C string.
    pub fn new(code: ExporterErrorCode, template: &'static str) -> Self {
        // Convert to CString to ensure null termination
        let template_cstring = CString::new(template).unwrap_or_default();
        let template_ptr = template_cstring.into_raw();

        Self {
            code,
            msg_template: template_ptr,
            context_fields: std::ptr::null(),
            context_count: 0,
        }
    }

    /// Creates a new ExporterError with a static template and context fields.
    ///
    /// This method is designed for template-based error messaging where static error
    /// templates are separated from dynamic context data.
    ///
    /// # Arguments
    ///
    /// * `code` - The error code representing the type of error
    /// * `template` - A static string template for the error message
    /// * `context_fields` - Vector of context fields containing structured error data
    ///
    /// The returned error owns all the strings and will free them when dropped.
    /// Both the template and all context field keys/values are converted to
    /// null-terminated C strings. The context fields array is heap-allocated
    /// and will be properly freed.
    pub fn with_template_and_context(
        code: ExporterErrorCode,
        template: &'static str,
        context_fields: Vec<ContextField>,
    ) -> Self {
        let (fields_ptr, count) = if context_fields.is_empty() {
            (std::ptr::null(), 0)
        } else {
            let boxed_fields = context_fields.into_boxed_slice();
            let len = boxed_fields.len();
            let ptr = Box::into_raw(boxed_fields) as *const ContextField;
            (ptr, len)
        };

        // Convert to CString to ensure null termination
        let template_cstring = CString::new(template).unwrap_or_default();
        let template_ptr = template_cstring.into_raw();

        Self {
            code,
            msg_template: template_ptr,
            context_fields: fields_ptr,
            context_count: count,
        }
    }
}

impl From<TraceExporterError> for ExporterError {
    fn from(value: TraceExporterError) -> Self {
        let code = match &value {
            TraceExporterError::Agent(_) => ExporterErrorCode::HttpEmptyBody,
            TraceExporterError::Builder(builder_error) => match builder_error {
                data_pipeline::trace_exporter::error::BuilderErrorKind::InvalidUri(_) => {
                    ExporterErrorCode::InvalidUrl
                }
                _ => ExporterErrorCode::InvalidArgument,
            },
            TraceExporterError::Internal(_) => ExporterErrorCode::Internal,
            TraceExporterError::Network(network_error) => match network_error.kind() {
                data_pipeline::trace_exporter::error::NetworkErrorKind::Body => {
                    ExporterErrorCode::HttpBodyFormat
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::Parse => {
                    ExporterErrorCode::HttpParse
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::TimedOut => {
                    ExporterErrorCode::TimedOut
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::WrongStatus => {
                    ExporterErrorCode::HttpWrongStatus
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::ConnectionClosed => {
                    ExporterErrorCode::ConnectionReset
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::MessageTooLarge => {
                    ExporterErrorCode::HttpBodyTooLong
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::Canceled => {
                    ExporterErrorCode::HttpClient
                }
                data_pipeline::trace_exporter::error::NetworkErrorKind::Unknown => {
                    ExporterErrorCode::NetworkUnknown
                }
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

        let context_fields: Vec<ContextField> = context
            .fields()
            .iter()
            .map(|(key, value)| {
                let key_cstring = CString::new(key.as_str()).unwrap_or_default();
                let value_cstring = CString::new(value.as_str()).unwrap_or_default();

                ContextField {
                    key: key_cstring.into_raw(),
                    value: value_cstring.into_raw(),
                }
            })
            .collect();

        ExporterError::with_template_and_context(code, template, context_fields)
    }
}

impl Drop for ExporterError {
    fn drop(&mut self) {
        unsafe {
            // Free the msg_template
            if !self.msg_template.is_null() {
                // SAFETY: msg_template is originated from CString::into_raw in new() and
                // with_template_and_context() methods
                drop(CString::from_raw(self.msg_template as *mut c_char));
                self.msg_template = std::ptr::null();
            }

            // Free the context fields
            if !self.context_fields.is_null() && self.context_count > 0 {
                // SAFETY: `context_fields` and individual key/value pointers are originated from
                // `CString::into_raw` calls in the `From<TraceExporterError>` conversion and
                // `with_template_and_context` method. The array is created via `Box::into_raw`
                // from a boxed slice. Any other creation path could lead to UB.
                for i in 0..self.context_count {
                    let field = &*self.context_fields.add(i);
                    if !field.key.is_null() {
                        drop(CString::from_raw(field.key as *mut c_char));
                    }
                    if !field.value.is_null() {
                        drop(CString::from_raw(field.value as *mut c_char));
                    }
                }

                // Free the context fields array
                drop(Box::from_raw(std::slice::from_raw_parts_mut(
                    self.context_fields as *mut ContextField,
                    self.context_count,
                )));
                self.context_fields = std::ptr::null();
                self.context_count = 0;
            }
        }
    }
}

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
    use std::ffi::CStr;

    #[test]
    fn constructor_test() {
        let code = ExporterErrorCode::InvalidArgument;
        let template = "Invalid argument provided";
        let error = Box::new(ExporterError::new(code, template));

        assert_eq!(error.code, ExporterErrorCode::InvalidArgument);
        assert!(!error.msg_template.is_null());
        let template_str = unsafe { CStr::from_ptr(error.msg_template).to_string_lossy() };
        assert_eq!(template_str, "Invalid argument provided");
        assert!(error.context_fields.is_null());
        assert_eq!(error.context_count, 0);
    }

    #[test]
    fn destructor_test() {
        let code = ExporterErrorCode::InvalidArgument;
        let template = "Test template";
        let error = Box::new(ExporterError::new(code, template));

        assert_eq!(error.code, ExporterErrorCode::InvalidArgument);
        assert!(!error.msg_template.is_null());

        unsafe { ddog_trace_exporter_error_free(Some(error)) };
    }

    #[test]
    fn template_and_context_test() {
        let code = ExporterErrorCode::InvalidUrl;
        let template = "Invalid URI provided";
        let context_fields = vec![ContextField {
            key: CString::new("details").unwrap().into_raw(),
            value: CString::new("invalid://url").unwrap().into_raw(),
        }];

        let error = Box::new(ExporterError::with_template_and_context(
            code,
            template,
            context_fields,
        ));

        assert_eq!(error.code, ExporterErrorCode::InvalidUrl);
        assert!(!error.msg_template.is_null());
        let template_str = unsafe { CStr::from_ptr(error.msg_template).to_string_lossy() };
        assert_eq!(template_str, "Invalid URI provided");
        assert!(!error.context_fields.is_null());
        assert_eq!(error.context_count, 1);

        unsafe { ddog_trace_exporter_error_free(Some(error)) };
    }

    #[test]
    fn from_trace_exporter_error_builder_test() {
        use data_pipeline::trace_exporter::error::{BuilderErrorKind, TraceExporterError};

        let builder_error =
            TraceExporterError::Builder(BuilderErrorKind::InvalidUri("bad://url".to_string()));
        let ffi_error = ExporterError::from(builder_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::InvalidUrl);
        assert!(!ffi_error.msg_template.is_null());
        let template_str = unsafe { CStr::from_ptr(ffi_error.msg_template).to_string_lossy() };
        assert_eq!(template_str, "Invalid URI provided: {details}");
        assert!(!ffi_error.context_fields.is_null());
        assert_eq!(ffi_error.context_count, 1);

        // Check context field content
        let context_field = unsafe { &*ffi_error.context_fields };
        let key_str = unsafe { CStr::from_ptr(context_field.key).to_string_lossy() };
        let value_str = unsafe { CStr::from_ptr(context_field.value).to_string_lossy() };
        assert_eq!(key_str, "details");
        assert_eq!(value_str, "bad://url");
    }

    #[test]
    fn from_trace_exporter_error_network_test() {
        use data_pipeline::trace_exporter::error::TraceExporterError;
        use std::io::{Error as IoError, ErrorKind};

        // Create a network error by wrapping an IO error
        let io_error = IoError::new(ErrorKind::ConnectionAborted, "Connection closed");
        let network_error = TraceExporterError::Io(io_error);
        let ffi_error = ExporterError::from(network_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::ConnectionAborted);
        assert!(!ffi_error.msg_template.is_null());
        let template_str = unsafe { CStr::from_ptr(ffi_error.msg_template).to_string_lossy() };
        assert_eq!(template_str, "Connection aborted");
        assert!(!ffi_error.context_fields.is_null());
        assert!(ffi_error.context_count > 0);
    }

    #[test]
    fn from_trace_exporter_error_agent_test() {
        use data_pipeline::trace_exporter::error::{AgentErrorKind, TraceExporterError};

        let agent_error = TraceExporterError::Agent(AgentErrorKind::EmptyResponse);
        let ffi_error = ExporterError::from(agent_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::HttpEmptyBody);
        assert!(!ffi_error.msg_template.is_null());
        let template_str = unsafe { CStr::from_ptr(ffi_error.msg_template).to_string_lossy() };
        assert_eq!(template_str, "Agent returned empty response");
        assert!(ffi_error.context_fields.is_null()); // AgentErrorKind has no context
        assert_eq!(ffi_error.context_count, 0);
    }

    #[test]
    fn from_trace_exporter_error_without_template_test() {
        use data_pipeline::trace_exporter::error::TraceExporterError;
        use std::io::{Error as IoError, ErrorKind};

        let io_error =
            TraceExporterError::Io(IoError::new(ErrorKind::PermissionDenied, "Access denied"));
        let ffi_error = ExporterError::from(io_error);

        assert_eq!(ffi_error.code, ExporterErrorCode::IoError);
        assert!(!ffi_error.msg_template.is_null());
        let template_str = unsafe { CStr::from_ptr(ffi_error.msg_template).to_string_lossy() };
        assert_eq!(template_str, "Permission denied");
        assert!(!ffi_error.context_fields.is_null());
        assert!(ffi_error.context_count > 0);
    }

    #[test]
    fn from_trace_exporter_error_memory_safety_test() {
        use data_pipeline::trace_exporter::error::{BuilderErrorKind, TraceExporterError};

        // Create error with context
        let builder_error = TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
            "Missing service name".to_string(),
        ));
        let ffi_error = Box::new(ExporterError::from(builder_error));

        // Verify context is properly allocated
        assert_eq!(ffi_error.context_count, 1);
        assert!(!ffi_error.context_fields.is_null());

        // Memory should be properly freed when dropped
        unsafe { ddog_trace_exporter_error_free(Some(ffi_error)) };
        // If this doesn't crash/leak, memory management is working correctly
    }
}
