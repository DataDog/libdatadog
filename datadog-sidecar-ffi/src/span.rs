// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! In-memory v0.4 trace collection (`TracesBytes`), kept only as the downgrade target of the native
//! V1 builder for the in-process (`coms.c`) sender. The standalone v0.4 builder that used to live
//! here is gone (the tracer builds V1 directly, see [`crate::span_v1`]); this is just the surface
//! [`crate::ddog_downgrade_v1_builder_to_v04_traces`] needs to serialize each downgraded trace.

use libdd_common_ffi::slice::CharSlice;
use libdd_trace_utils::span::v04::SpanBytes;
use std::ffi::c_char;

fn new_vector_item<T: Default>(vec: &mut Vec<T>) -> &mut T {
    vec.push(T::default());
    // Safety: we just pushed a value to the vector, so `last_mut()` returns `Some`
    unsafe { vec.last_mut().unwrap_unchecked() }
}

// ------------------ TracesBytes ------------------

pub type TraceBytes = Vec<SpanBytes>;
pub type TracesBytes = Vec<TraceBytes>;

#[no_mangle]
pub extern "C" fn ddog_get_traces() -> Box<TracesBytes> {
    Box::default()
}

#[no_mangle]
pub extern "C" fn ddog_free_traces(_traces: Box<TracesBytes>) {}

#[no_mangle]
pub extern "C" fn ddog_get_traces_size(traces: &TracesBytes) -> usize {
    traces.len()
}

#[no_mangle]
pub extern "C" fn ddog_get_trace(
    traces: &mut TracesBytes,
    index: usize,
) -> Option<&mut TraceBytes> {
    traces.get_mut(index)
}

// ------------------ TraceBytes ------------------

#[no_mangle]
pub extern "C" fn ddog_traces_new_trace(traces: &mut TracesBytes) -> &mut TraceBytes {
    new_vector_item(traces)
}

// ------------------- Free / export helpers -------------------

/// Frees an owned [`CharSlice`]. Note that some functions of this API return borrowed slices that
/// must NOT be freed. Only a few selected functions return slices that must be freed, and this is
/// mentioned explicitly in their documentation.
///
/// # Safety
///
/// `slice` must be an owned char slice that has been returned by one of the functions of this API.
#[no_mangle]
pub unsafe extern "C" fn ddog_free_charslice(slice: CharSlice<'static>) {
    let (ptr, len) = slice.as_raw_parts();

    if len == 0 || ptr.is_null() {
        return;
    }

    unsafe {
        let owned_ptr = ptr as *mut c_char;
        let _ = Box::from_raw(owned_ptr);
    }
}

/// Serializes one v0.4 trace as a msgpack array-of-1, the framing the background sender
/// (`ddtrace_send_traces_via_thread`) expects. Returns an owned slice; free with
/// [`ddog_free_charslice`].
#[no_mangle]
pub extern "C" fn ddog_serialize_trace_into_charslice(
    trace: &mut TraceBytes,
) -> CharSlice<'static> {
    match rmp_serde::encode::to_vec_named(&vec![trace]) {
        Ok(vec) => {
            let boxed_str = vec.into_boxed_slice();
            let boxed_len = boxed_str.len();

            let leaked_ptr = Box::into_raw(boxed_str) as *const c_char;

            unsafe { CharSlice::from_raw_parts(leaked_ptr, boxed_len) }
        }
        Err(_) => CharSlice::empty(),
    }
}
