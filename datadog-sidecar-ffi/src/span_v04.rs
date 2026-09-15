// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! In-memory v0.4 trace collection (`TracesBytes`), kept only as the downgrade target of the native
//! V1 builder for the in-process (`coms.c`) sender (the tracer builds V1 directly, see
//! [`crate::span`]).

use libdd_common_ffi::slice::CharSlice;
use libdd_trace_utils::span::v04::SpanBytes;
use std::ffi::c_char;

// ------------------ TracesBytes ------------------

pub type TraceBytes = Vec<SpanBytes>;
pub type TracesBytes = Vec<TraceBytes>;

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

// ------------------- Serialization / export helpers -------------------

/// Serializes one v0.4 trace as a msgpack array-of-1, the framing the background sender
/// (`ddtrace_send_traces_via_thread`) expects. Returns an owned slice; free with
/// [`crate::span::ddog_free_charslice`].
#[no_mangle]
pub extern "C" fn ddog_serialize_trace_into_charslice(
    trace: &mut TraceBytes,
) -> CharSlice<'static> {
    match rmp_serde::encode::to_vec_named(&vec![trace]) {
        Ok(mut vec) => {
            if vec.is_empty() {
                return CharSlice::empty();
            }
            let payload_len = vec.len();
            // Append a trailing NUL so the allocation is `payload_len + 1` bytes, matching the
            // shape `ddog_free_charslice` reclaims. The terminator is excluded from the reported
            // length, so consumers only ever see the msgpack payload.
            vec.push(0);
            let boxed_str = vec.into_boxed_slice();

            let leaked_ptr = Box::into_raw(boxed_str) as *const c_char;

            unsafe { CharSlice::from_raw_parts(leaked_ptr, payload_len) }
        }
        Err(_) => CharSlice::empty(),
    }
}
