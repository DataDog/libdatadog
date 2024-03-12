use std::ffi::{CString, c_char};
// use bytes::Bytes;
// use data_pipeline::trace_exporter::TraceExporter;

pub struct TraceExporter;

#[no_mangle]
pub unsafe extern "C" fn dd_trace_exporter_new(
    host: *const c_char,
    port: u16,
    timeout: u64,
    tracer_version: *const c_char,
    language: *const c_char,
    language_version: *const c_char,
    language_interpreter: *const c_char) -> *mut TraceExporter {

    Box::into_raw(Box::new(TraceExporter{}))
}

#[no_mangle]
pub unsafe extern "C" fn dd_trace_exporter_free(ctx: *mut TraceExporter) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx))
    }
}

#[no_mangle]
pub unsafe extern "C" fn dd_trace_exporter_send(
    ctx: *mut TraceExporter,
    trace: *const u8,
    size: usize,
    trace_count: usize) -> *const c_char {

    CString::new(String::from("Response")).unwrap().into_raw()
}

