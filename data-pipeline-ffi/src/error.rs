// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::error::{
    BuilderErrorKind, NetworkErrorKind, TraceExporterError,
};
use std::ffi::{c_char, CString};
use std::fmt::Display;
use std::io::ErrorKind as IoErrorKind;

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
    HttpParse,
    HttpServer,
    HttpWrongStatus,
    InvalidArgument,
    InvalidData,
    InvalidInput,
    InvalidUrl,
    IoError,
    NetworkUnknown,
    Serde,
    TimedOut,
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
            Self::HttpClient => write!(f, "HTTP error orgininated by client"),
            Self::HttpParse => write!(f, "Error while parsing HTTP message"),
            Self::HttpServer => write!(f, "HTTP error orgininated by server"),
            Self::HttpWrongStatus => write!(f, "HTTP wrong status number"),
            Self::InvalidArgument => write!(f, "Invalid argument provided"),
            Self::InvalidData => write!(f, "Invalid data payload"),
            Self::InvalidInput => write!(f, "Invalid input"),
            Self::InvalidUrl => write!(f, "Invalid URL"),
            Self::IoError => write!(f, "Input/Output error"),
            Self::NetworkUnknown => write!(f, "Unknown network error"),
            Self::Serde => write!(f, "Serialization/Deserialization error"),
            Self::TimedOut => write!(f, "Operation timed out"),
        }
    }
}

/// Stucture that contains error information that `TraceExporter` API can return.
#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct ExporterError {
    pub code: ExporterErrorCode,
    pub msg: *mut c_char,
}

impl ExporterError {
    pub fn new(code: ExporterErrorCode, msg: &str) -> Self {
        Self {
            code,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
        }
    }
}

impl From<TraceExporterError> for ExporterError {
    // add code here
    fn from(value: TraceExporterError) -> Self {
        let code = match &value {
            TraceExporterError::Builder(e) => match e {
                BuilderErrorKind::InvalidUri => ExporterErrorCode::InvalidUrl,
            },
            TraceExporterError::Deserialization(_) => ExporterErrorCode::Serde,
            TraceExporterError::Io(e) => match e.kind() {
                IoErrorKind::InvalidData => ExporterErrorCode::InvalidData,
                IoErrorKind::InvalidInput => ExporterErrorCode::InvalidInput,
                IoErrorKind::ConnectionReset => ExporterErrorCode::ConnectionReset,
                IoErrorKind::ConnectionAborted => ExporterErrorCode::ConnectionAborted,
                IoErrorKind::ConnectionRefused => ExporterErrorCode::ConnectionRefused,
                IoErrorKind::TimedOut => ExporterErrorCode::TimedOut,
                IoErrorKind::AddrInUse => ExporterErrorCode::AddressInUse,
                _ => ExporterErrorCode::IoError,
            },
            TraceExporterError::Network(e) => match e.kind() {
                NetworkErrorKind::Body => ExporterErrorCode::HttpBodyFormat,
                NetworkErrorKind::Canceled => ExporterErrorCode::ConnectionAborted,
                NetworkErrorKind::ConnectionClosed => ExporterErrorCode::ConnectionReset,
                NetworkErrorKind::MessageTooLarge => ExporterErrorCode::HttpBodyTooLong,
                NetworkErrorKind::Parse => ExporterErrorCode::HttpParse,
                NetworkErrorKind::TimedOut => ExporterErrorCode::TimedOut,
                NetworkErrorKind::Unknown => ExporterErrorCode::NetworkUnknown,
                NetworkErrorKind::WrongStatus => ExporterErrorCode::HttpWrongStatus,
            },
            TraceExporterError::Request(e) => {
                let status: u16 = e.status().into();
                if (400..500).contains(&status) {
                    ExporterErrorCode::HttpClient
                } else {
                    ExporterErrorCode::HttpServer
                }
            }
            TraceExporterError::Serde(_) => ExporterErrorCode::Serde,
        };
        ExporterError::new(code, &value.to_string())
    }
}

/// Free
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_error_free(
    error: ddcommon_ffi::Option<&mut ExporterError>,
) {
    if let ddcommon_ffi::Option::Some(error) = error {
        if !error.msg.is_null() {
            drop(CString::from_raw(error.msg));
            error.msg = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::ffi::CString;

    #[test]
    fn constructor_test() {
        let code = ExporterErrorCode::InvalidArgument;
        let error = ExporterError::new(code, &code.to_string());

        assert_eq!(error.code, ExporterErrorCode::InvalidArgument);
        let msg = unsafe { CString::from_raw(error.msg).into_string().unwrap() };
        assert_eq!(msg, ExporterErrorCode::InvalidArgument.to_string());
    }

    #[test]
    fn destructor_test() {
        let code = ExporterErrorCode::InvalidArgument;
        let mut error = ExporterError::new(code, &code.to_string());

        assert_eq!(error.code, ExporterErrorCode::InvalidArgument);
        let msg = unsafe { CStr::from_ptr(error.msg).to_string_lossy() };
        assert_eq!(msg, ExporterErrorCode::InvalidArgument.to_string());

        unsafe { ddog_trace_exporter_error_free(ddcommon_ffi::Option::Some(&mut error)) };

        assert_eq!(error.msg, std::ptr::null_mut());
    }
}
