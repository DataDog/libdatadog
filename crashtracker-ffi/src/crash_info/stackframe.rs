// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::to_inner::ToInner;
use crate::Result;
use ::function_name::named;
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, wrap_with_ffi_result, CharSlice, Error};

/// Represents a StackFrame. Do not access its member for any reason, only use
/// the C API functions on this struct.
#[repr(C)]
pub struct StackFrame {
    // This may be null, but if not it will point to a valid StackFrame.
    inner: *mut datadog_crashtracker::rfc5_crash_info::StackFrame,
}

impl ToInner for StackFrame {
    type Inner = datadog_crashtracker::rfc5_crash_info::StackFrame;

    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut Self::Inner> {
        self.inner
            .as_mut()
            .context("inner pointer was null, indicates use after free")
    }
}

impl StackFrame {
    pub(super) fn new() -> Self {
        StackFrame {
            inner: Box::into_raw(Box::new(
                datadog_crashtracker::rfc5_crash_info::StackFrame::new(),
            )),
        }
    }

    pub(super) fn take(
        &mut self,
    ) -> Option<Box<datadog_crashtracker::rfc5_crash_info::StackFrame>> {
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

impl Drop for StackFrame {
    fn drop(&mut self) {
        drop(self.take())
    }
}

/// Returned by [ddog_prof_Profile_new].
#[repr(C)]
pub enum StackFrameNewResult {
    Ok(StackFrame),
    #[allow(dead_code)]
    Err(Error),
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Create a new StackFrame, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_StackFrame_new() -> StackFrameNewResult {
    StackFrameNewResult::Ok(StackFrame::new())
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_StackFrame_drop(frame: *mut StackFrame) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !frame.is_null() {
        drop((*frame).take())
    }
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_ip(
    mut frame: *mut StackFrame,
    ip: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.ip = ip.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_module_base_address(
    mut frame: *mut StackFrame,
    module_base_address: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.module_base_address = module_base_address.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_sp(
    mut frame: *mut StackFrame,
    sp: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.sp = sp.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_symbol_address(
    mut frame: *mut StackFrame,
    symbol_address: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.symbol_address = symbol_address.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_build_id(
    mut frame: *mut StackFrame,
    build_id: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.build_id = build_id.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_path(
    mut frame: *mut StackFrame,
    path: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.path = path.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_relative_address(
    mut frame: *mut StackFrame,
    relative_address: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.relative_address = relative_address.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_file(
    mut frame: *mut StackFrame,
    file: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.file = file.try_to_string_option()?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_function(
    mut frame: *mut StackFrame,
    function: CharSlice,
) -> Result {
    wrap_with_ffi_result!({
        let frame = frame.to_inner_mut()?;
        frame.function = function.try_to_string_option()?;
        anyhow::Ok(())
    })
}
