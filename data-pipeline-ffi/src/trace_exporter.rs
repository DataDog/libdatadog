// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::{
    ResponseCallback, TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use ddcommon_ffi::{
    slice::{AsBytes, ByteSlice},
    CharSlice, MaybeError,
};
use std::{ffi::c_char, ptr::NonNull, time::Duration};

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
/// * `hostname` - The hostname of the application, used for stats aggregation
/// * `env` - The environment of the application, used for stats aggregation
/// * `version` - The version of the application, used for stats aggregation
/// * `service` - The service name of the application, used for stats aggregation
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
    hostname: CharSlice,
    env: CharSlice,
    version: CharSlice,
    service: CharSlice,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    compute_stats: bool,
    agent_response_callback: extern "C" fn(*const c_char),
) -> MaybeError {
    let callback_wrapper = ResponseCallbackWrapper {
        response_callback: agent_response_callback,
    };
    // TODO - handle errors - https://datadoghq.atlassian.net/browse/APMSP-1095
    let mut builder = TraceExporter::builder()
        .set_url(url.to_utf8_lossy().as_ref())
        .set_tracer_version(tracer_version.to_utf8_lossy().as_ref())
        .set_language(language.to_utf8_lossy().as_ref())
        .set_language_version(language_version.to_utf8_lossy().as_ref())
        .set_language_interpreter(language_interpreter.to_utf8_lossy().as_ref())
        .set_hostname(hostname.to_utf8_lossy().as_ref())
        .set_env(env.to_utf8_lossy().as_ref())
        .set_app_version(version.to_utf8_lossy().as_ref())
        .set_service(service.to_utf8_lossy().as_ref())
        .set_input_format(input_format)
        .set_output_format(output_format)
        .set_response_callback(Box::new(callback_wrapper));
    if compute_stats {
        builder = builder.enable_stats(Duration::from_secs(10))
        // TODO: APMSP-1317 Enable peer tags aggregation and stats by span_kind based on agent
        // configuration
    }
    let exporter = builder.build().unwrap();
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

    // necessary that the trace be static for the life of the handle. Memory should be freed when
    // the handle is dropped.
    let static_trace: ByteSlice<'static> = std::mem::transmute(trace);

    handle
        .send(static_trace, trace_count)
        .unwrap_or(String::from(""));
    MaybeError::None
}
