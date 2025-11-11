// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use function_name::named;
use libdd_common_ffi::{
    wrap_with_ffi_result, wrap_with_void_ffi_result, CharSlice, Result, VoidResult,
};
/// Removes all existing additional tags
/// Expected to be used after a fork, to reset the additional tags on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before profiling ops start on the child.
/// # Safety
/// No safety concerns.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_clear_additional_tags() -> VoidResult {
    wrap_with_void_ffi_result!({ libdd_crashtracker::clear_additional_tags()? })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically registers a string as an additional tag.
/// Useful for tracking what operations were occurring when a crash occurred.
/// The set does not check for duplicates.
///
/// Returns:
///   Ok(handle) on success.  The handle is needed to later remove the id;
///   Err() on failure. The most likely cause of failure is that the underlying set is full.
///
/// # Safety
/// The string argument must be valid.
pub unsafe extern "C" fn ddog_crasht_insert_additional_tag(s: CharSlice) -> Result<usize> {
    wrap_with_ffi_result!({ libdd_crashtracker::insert_additional_tag(s.to_string()) })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically removes a completed SpanId.
/// Useful for tracking what operations were occurring when a crash occurred.
/// 0 is reserved for "NoId"
///
/// Returns:
///   `Ok` on success.  
///   `Err` on failure.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_remove_additional_tag(idx: usize) -> VoidResult {
    wrap_with_void_ffi_result!({
        libdd_crashtracker::remove_additional_tag(idx)?;
    })
}
