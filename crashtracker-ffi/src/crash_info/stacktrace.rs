// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ::function_name::named;
use datadog_crashtracker::{StackFrame, StackTrace};
use ddcommon_ffi::{wrap_with_void_ffi_result, Error, Handle, ToInner, VoidResult};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[repr(C)]
pub enum StackTraceNewResult {
    Ok(Handle<StackTrace>),
    Err(Error),
}

/// Create a new StackTrace, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_StackTrace_new() -> StackTraceNewResult {
    StackTraceNewResult::Ok(StackTrace::new_incomplete().into())
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_StackTrace_drop(trace: *mut Handle<StackTrace>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !trace.is_null() {
        drop((*trace).take())
    }
}

/// # Safety
/// The `stacktrace` can be null, but if non-null it must point to a StackTrace made by this module,
/// which has not previously been dropped.
/// The frame can be non-null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The frame is consumed, and does not need to be dropped after this operation.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackTrace_push_frame(
    mut trace: *mut Handle<StackTrace>,
    mut frame: *mut Handle<StackFrame>,
    incomplete: bool,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        trace
            .to_inner_mut()?
            .push_frame(*frame.take()?, incomplete)?;
    })
}

/// # Safety
/// The `stacktrace` can be null, but if non-null it must point to a StackTrace made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackTrace_set_complete(
    mut trace: *mut Handle<StackTrace>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        trace.to_inner_mut()?.set_complete()?;
    })
}
