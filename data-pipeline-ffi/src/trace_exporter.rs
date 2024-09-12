// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::{
    ResponseCallback, TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use ddcommon_ffi::{
    slice::{AsBytes, ByteSlice},
    CharSlice, MaybeError,
};
use std::{ffi::c_char, ptr::NonNull};

/// Create a new TraceExporter instance.
///
/// # Arguments
///
/// * `out_handle` - The handle to write the TraceExporter instance in.
/// * `url` - The URL of the Datadog Agent to communicate with.
/// * `tracer_version` - The version of the client library.
/// * `language` - The language of the client library.
/// * `language_version` - The version of the language of the client library.
/// * `language_interpreter` - The interpreter of the language of the client library.
/// * `input_format` - The input format of the traces. Setting this to Proxy will send the trace
///   data to the Datadog Agent as is.
/// * `output_format` - The output format of the traces to send to the Datadog Agent. If using the
///   Proxy input format, this should be set to format if the trace data that will be passed through
///   as is.
/// * `agent_response_callback` - The callback into the client library that the TraceExporter uses
///   for updated Agent JSON responses.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_new(
    out_handle: NonNull<Box<TraceExporter>>,
    url: CharSlice,
    tracer_version: CharSlice,
    language: CharSlice,
    language_version: CharSlice,
    language_interpreter: CharSlice,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    agent_response_callback: extern "C" fn(*const c_char),
) -> MaybeError {
    let callback_wrapper = ResponseCallbackWrapper {
        response_callback: agent_response_callback,
    };
    // TODO - handle errors - https://datadoghq.atlassian.net/browse/APMSP-1095
    let exporter = TraceExporter::builder()
        .set_url(url.to_utf8_lossy().as_ref())
        .set_tracer_version(tracer_version.to_utf8_lossy().as_ref())
        .set_language(language.to_utf8_lossy().as_ref())
        .set_language_version(language_version.to_utf8_lossy().as_ref())
        .set_language_interpreter(language_interpreter.to_utf8_lossy().as_ref())
        .set_input_format(input_format)
        .set_output_format(output_format)
        .set_response_callback(Box::new(callback_wrapper))
        .build()
        .unwrap();
    out_handle.as_ptr().write(Box::new(exporter));
    MaybeError::None
}

struct ResponseCallbackWrapper {
    response_callback: extern "C" fn(*const c_char),
}

impl ResponseCallback for ResponseCallbackWrapper {
    fn call(&self, response: &str) {
        let c_response = std::ffi::CString::new(response).unwrap();
        (self.response_callback)(c_response.as_ptr());
    }
}

/// Free the TraceExporter instance.
///
/// # Arguments
///
/// * handle - The handle to the TraceExporter instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_free(handle: Box<TraceExporter>) {
    drop(handle);
}

/// Send traces to the Datadog Agent.
///
/// # Arguments
///
/// * `handle` - The handle to the TraceExporter instance.
/// * `trace` - The traces to send to the Datadog Agent in the input format used to create the
///   TraceExporter.
/// * `trace_count` - The number of traces to send to the Datadog Agent.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_send(
    handle: &TraceExporter,
    trace: ByteSlice,
    trace_count: usize,
) -> MaybeError {
    // TODO - handle errors - https://datadoghq.atlassian.net/browse/APMSP-1095
    handle
        .send(trace.as_bytes(), trace_count)
        .unwrap_or(String::from(""));
    MaybeError::None
}
