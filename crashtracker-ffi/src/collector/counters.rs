// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::datatypes::OpTypes;
use ::function_name::named;
use ddcommon_ffi::{wrap_with_void_ffi_result, VoidResult};

/// Resets all counters to 0.
/// Expected to be used after a fork, to reset the counters on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before profiling ops start on the child.
/// # Safety
/// No safety concerns.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_reset_counters() -> VoidResult {
    wrap_with_void_ffi_result!({ datadog_crashtracker::reset_counters()? })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically increments the count associated with `op`.
/// Useful for tracking what operations were occuring when a crash occurred.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_begin_op(op: OpTypes) -> VoidResult {
    wrap_with_void_ffi_result!({ datadog_crashtracker::begin_op(op)? })
}

#[no_mangle]
#[must_use]
#[named]
/// Atomically decrements the count associated with `op`.
/// Useful for tracking what operations were occuring when a crash occurred.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_crasht_end_op(op: OpTypes) -> VoidResult {
    wrap_with_void_ffi_result!({ datadog_crashtracker::end_op(op)? })
}
