use std::ffi::{CStr, CString, c_char};
use bytes::Bytes;
use data_pipeline::trace_exporter::TraceExporter;
use data_pipeline::trace_exporter::TraceExporterBuilder;


#[no_mangle]
pub unsafe extern "C" fn dd_trace_exporter_new(
    host: *const c_char,
    port: u16,
    tracer_version: *const c_char,
    language: *const c_char,
    language_version: *const c_char,
    language_interpreter: *const c_char) -> *mut TraceExporter {

    let mut builder = TraceExporterBuilder::default();

    let exporter = builder
        .set_host(CStr::from_ptr(host).to_str().unwrap())
        .set_port(port)
        .set_tracer_version(CStr::from_ptr(tracer_version).to_str().unwrap())
        .set_language(CStr::from_ptr(language).to_str().unwrap())
        .set_language_version(CStr::from_ptr(language_version).to_str().unwrap())
        .set_language_interpreter(CStr::from_ptr(language_interpreter).to_str().unwrap())
        .build().unwrap();

    Box::into_raw(Box::new(exporter))

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

    let handle = Box::from_raw(ctx);
    let response = handle.send(
        Bytes::copy_from_slice(std::slice::from_raw_parts(trace, size)),
        trace_count).unwrap_or(String::from(""));

    CString::new(response).unwrap().into_raw()
}

