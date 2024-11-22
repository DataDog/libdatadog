// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::to_inner::ToInner;
use crate::Result;
use crate::StackFrame;
use ::function_name::named;
use anyhow::Context;
use ddcommon_ffi::Error;

/// Represents a StackTrace. Do not access its member for any reason, only use
/// the C API functions on this struct.
#[repr(C)]
pub struct StackTrace {
    // This may be null, but if not it will point to a valid StackTrace.
    inner: *mut datadog_crashtracker::rfc5_crash_info::StackTrace,
}

impl ToInner for StackTrace {
    type Inner = datadog_crashtracker::rfc5_crash_info::StackTrace;

    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut Self::Inner> {
        self.inner
            .as_mut()
            .context("inner pointer was null, indicates use after free")
    }
}

impl StackTrace {
    pub(super) fn new(crash_info: datadog_crashtracker::rfc5_crash_info::StackTrace) -> Self {
        StackTrace {
            inner: Box::into_raw(Box::new(crash_info)),
        }
    }

    pub(super) fn take(
        &mut self,
    ) -> Option<Box<datadog_crashtracker::rfc5_crash_info::StackTrace>> {
        // Leaving a null will help with double-free issues that can
        // arise in C. Of course, it's best to never get there in the
        // first place!
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());

        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }
}

impl Drop for StackTrace {
    fn drop(&mut self) {
        drop(self.take())
    }
}

/// Returned by [ddog_prof_Profile_new].
#[repr(C)]
pub enum StackTraceNewResult {
    Ok(StackTrace),
    #[allow(dead_code)]
    Err(Error),
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Wraps a C-FFI function in standard form
/// Expects the function to return a result type that implements into
/// and to be decorated with #[named].
macro_rules! wrap {
    ($body:expr) => {
        (|| $body)()
            .context(concat!(function_name!(), " failed"))
            .into()
    };
}

/// Create a new StackTrace, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_StackTrace_new() -> StackTraceNewResult {
    StackTraceNewResult::Ok(StackTrace::new(
        datadog_crashtracker::rfc5_crash_info::StackTrace::new(),
    ))
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_StackTrace_drop(trace: *mut StackTrace) {
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
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackTrace_push_frame(
    mut trace: *mut StackTrace,
    frame: *mut StackFrame,
) -> Result {
    wrap!({
        let trace = trace.to_inner_mut()?;
        let frame = *frame
            .as_mut()
            .context("Null frame pointer")?
            .take()
            .context("Frame had null inner pointer.  Use after free?")?;
        trace.frames.push(frame);
        anyhow::Ok(())
    })
}
