// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use bytes::Bytes;
use data_pipeline::trace_exporter::TraceExporter;
use data_pipeline::trace_exporter::TraceExporterBuilder;
use ddcommon_ffi::{
    slice::{AsBytes, ByteSlice},
    CharSlice,
};
use std::ffi::c_char;

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_new(
    host: CharSlice,
    port: u16,
    tracer_version: CharSlice,
    language: CharSlice,
    language_version: CharSlice,
    language_interpreter: CharSlice,
) -> *mut TraceExporter {
    let mut builder = TraceExporterBuilder::default();

    let exporter = builder
        .set_host(host.to_utf8_lossy().as_ref())
        .set_port(port)
        .set_tracer_version(tracer_version.to_utf8_lossy().as_ref())
        .set_language(language.to_utf8_lossy().as_ref())
        .set_language_version(language_version.to_utf8_lossy().as_ref())
        .set_language_interpreter(language_interpreter.to_utf8_lossy().as_ref())
        .build()
        .unwrap();

    Box::into_raw(Box::new(exporter))
}

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_free(ctx: *mut TraceExporter) {
    if let Some(p) = ctx.as_mut() {
        drop(Box::from_raw(p))
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_send(
    ctx: *mut TraceExporter,
    trace: ByteSlice,
    trace_count: usize,
) -> *mut c_char {
    let handle = Box::from_raw(ctx);
    let response = handle
        .send(Bytes::copy_from_slice(trace.as_bytes()), trace_count)
        .unwrap_or(String::from(""));

    let ret = libc::malloc(response.len() + 1);
    std::ptr::copy(response.as_bytes().as_ptr(), ret.cast(), response.len());
    std::ptr::write(ret.add(response.len()) as *mut u8, 0u8);
    ret as *mut c_char
}
