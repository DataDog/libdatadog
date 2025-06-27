// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::{ProfileError, SliceSet, SliceSetInsertResult};
use ddcommon_ffi::Slice;
use std::ptr;

pub type StackTraceSet = SliceSet<u64>;

/// Creates a new, empty set of stack traces.
///
/// # Errors
///
/// Returns null if allocation fails.
#[no_mangle]
pub extern "C" fn ddog_prof_StackTraceSet_new() -> *mut StackTraceSet {
    match datadog_alloc::Box::try_new(StackTraceSet::new()) {
        Ok(boxed) => datadog_alloc::Box::into_raw(boxed),
        Err(_err) => ptr::null_mut(),
    }
}

/// Inserts a new stack trace into the set.
///
/// # Errors
///
///  1. Fails if `set` is null.
///  2. Fails if `stack_trace` is an invalid slice (note that there are still
///     safety conditions we cannot check at runtime that need to be ensured).
///  3. Fails if the `set` needs to grow and fails to allocate memory.
///  4. Fails if the `set` has grown too much and is full. It internally
///     compresses two `u32`s into a `u64`, so it fails if it grows too large.
///
/// # Safety
///
///  1. The `set` must be a valid reference if not null.
///  2. TODO There must be no label slices from
///     `ddog_prof_StackTraceSet_lookup` still alive when this is called.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_StackTraceSet_insert(
    set: *mut StackTraceSet,
    stack_trace: Slice<u64>,
) -> SliceSetInsertResult {
    let Some(set) = set.as_mut() else {
        return SliceSetInsertResult::Err(ProfileError::InvalidInput);
    };
    let Some(slice) = stack_trace.try_as_slice() else {
        return SliceSetInsertResult::Err(ProfileError::InvalidInput);
    };
    set.insert(slice).into()
}

/// Drops the set, and assigns the pointer to null to help avoid a double-free.
///
/// # Safety
///
///  1. The set must be a valid reference if it's not null.
///  2. TODO If slices from `ddog_prof_StackTraceSet_lookup` are alive, then
///     you cannot drop the set.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StackTraceSet_drop(
    set: *mut *mut StackTraceSet,
) {
    if let Some(ptr) = set.as_mut() {
        let inner_ptr = *ptr;
        if !inner_ptr.is_null() {
            drop(datadog_alloc::Box::from_raw(inner_ptr));
            *ptr = ptr::null_mut();
        }
    }
}
