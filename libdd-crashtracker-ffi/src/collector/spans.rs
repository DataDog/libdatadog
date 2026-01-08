// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use function_name::named;
use libdd_common_ffi::{wrap_with_ffi_result, wrap_with_void_ffi_result, Result, VoidResult};
/// Resets all stored spans to 0.
/// Expected to be used after a fork, to reset the spans on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before profiling ops start on the child.
/// # Safety
/// No safety concerns.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_clear_span_ids() -> VoidResult {
    wrap_with_void_ffi_result!({ libdd_crashtracker::clear_spans()? })
}

/// Resets all stored traces to 0.
/// Expected to be used after a fork, to reset the traces on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before profiling ops start on the child.
/// # Safety
/// No safety concerns.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_clear_trace_ids() -> VoidResult {
    wrap_with_void_ffi_result!({ libdd_crashtracker::clear_traces()? })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically registers an active traceId.
/// Useful for tracking what operations were occurring when a crash occurred.
/// 0 is reserved for "NoId"
/// The set does not check for duplicates.  Adding the same id twice is an error.
///
/// Inputs:
/// id<high/low>: the 128 bit id, broken into 2 64 bit chunks (see note)
///
/// Returns:
///   Ok(handle) on success.  The handle is needed to later remove the id;
///   Err() on failure. The most likely cause of failure is that the underlying set is full.
///
/// Note: 128 bit ints in FFI were not stabilized until Rust 1.77
/// <https://blog.rust-lang.org/2024/03/30/i128-layout-update.html>
/// We're currently locked into 1.76.0, have to do an ugly workaround involving 2 64 bit ints
/// until we can upgrade.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_insert_trace_id(id_high: u64, id_low: u64) -> Result<usize> {
    wrap_with_ffi_result!({
        let id: u128 = ((id_high as u128) << 64) | (id_low as u128);
        libdd_crashtracker::insert_trace(id)
    })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically registers an active SpanId.
/// Useful for tracking what operations were occurring when a crash occurred.
/// 0 is reserved for "NoId".
/// The set does not check for duplicates.  Adding the same id twice is an error.
///
/// Inputs:
/// id<high/low>: the 128 bit id, broken into 2 64 bit chunks (see note)
///
/// Returns:
///   Ok(handle) on success.  The handle is needed to later remove the id;
///   Err() on failure. The most likely cause of failure is that the underlying set is full.
///
/// Note: 128 bit ints in FFI were not stabilized until Rust 1.77
/// <https://blog.rust-lang.org/2024/03/30/i128-layout-update.html>
/// We're currently locked into 1.76.0, have to do an ugly workaround involving 2 64 bit ints
/// until we can upgrade.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_insert_span_id(id_high: u64, id_low: u64) -> Result<usize> {
    wrap_with_ffi_result!({
        let id: u128 = ((id_high as u128) << 64) | (id_low as u128);
        libdd_crashtracker::insert_span(id)
    })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically removes a completed SpanId.
/// Useful for tracking what operations were occurring when a crash occurred.
/// 0 is reserved for "NoId"
///
/// Inputs:
/// id<high/low>: the 128 bit id, broken into 2 64 bit chunks (see note)
/// idx: The handle for the id, from a previous successful call to `insert_span_id`.
///      Attempting to remove the same element twice is an error.
/// Returns:
///   `Ok` on success.  
///   `Err` on failure.  If `id` is not found at `idx`, `Err` will be returned and the set will not
///                      be modified.
///
/// Note: 128 bit ints in FFI were not stabilized until Rust 1.77
/// <https://blog.rust-lang.org/2024/03/30/i128-layout-update.html>
/// We're currently locked into 1.76.0, have to do an ugly workaround involving 2 64 bit ints
/// until we can upgrade.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_remove_span_id(
    id_high: u64,
    id_low: u64,
    idx: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let id: u128 = ((id_high as u128) << 64) | (id_low as u128);
        libdd_crashtracker::remove_span(id, idx)?
    })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically removes a completed TraceId.
/// Useful for tracking what operations were occurring when a crash occurred.
/// 0 is reserved for "NoId"
///
/// Inputs:
/// id<high/low>: the 128 bit id, broken into 2 64 bit chunks (see note)
/// idx: The handle for the id, from a previous successful call to `insert_span_id`.
///      Attempting to remove the same element twice is an error.
/// Returns:
///   `Ok` on success.  
///   `Err` on failure.  If `id` is not found at `idx`, `Err` will be returned and the set will not
///                      be modified.
///
/// Note: 128 bit ints in FFI were not stabilized until Rust 1.77
/// <https://blog.rust-lang.org/2024/03/30/i128-layout-update.html>
/// We're currently locked into 1.76.0, have to do an ugly workaround involving 2 64 bit ints
/// until we can upgrade.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_remove_trace_id(
    id_high: u64,
    id_low: u64,
    idx: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let id: u128 = ((id_high as u128) << 64) | (id_low as u128);
        libdd_crashtracker::remove_trace(id, idx)?
    })
}
