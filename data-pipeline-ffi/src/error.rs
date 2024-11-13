// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Represent error codes that `Error` struct can hold
#[repr(C)]
#[derive(Debug, PartialEq)]
pub enum TraceExporterErrorCode {
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

/// Stucture that contains error information that `TraceExporter` API can return.
#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct TraceExporterError {
    pub code: TraceExporterErrorCode,
}

impl From<TraceExporterErrorCode> for TraceExporterError {
    fn from(value: TraceExporterErrorCode) -> Self {
        TraceExporterError { code: value }
    }
}

/// Free
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_error_free(
    _error: ddcommon_ffi::Option<&mut TraceExporterError>,
) {
    // TODO: Placeholder function to remove dynamically allocated properties in Error.
}
