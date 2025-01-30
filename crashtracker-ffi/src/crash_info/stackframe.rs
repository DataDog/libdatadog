// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ::function_name::named;
use datadog_crashtracker::{BuildIdType, FileType, StackFrame};
use ddcommon_ffi::{
    slice::AsBytes, wrap_with_void_ffi_result, CharSlice, Handle, Result, ToInner, VoidResult,
};

use ddcommon_ffi::ToHexStr;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Create a new StackFrame, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_StackFrame_new() -> Result<Handle<StackFrame>> {
    ddcommon_ffi::Result::Ok(StackFrame::new().into())
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_StackFrame_drop(frame: *mut Handle<StackFrame>) {
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
    mut frame: *mut Handle<StackFrame>,
    ip: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.ip = Some(ip.to_hex_str());
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
    mut frame: *mut Handle<StackFrame>,
    module_base_address: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.module_base_address = Some(module_base_address.to_hex_str());
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
    mut frame: *mut Handle<StackFrame>,
    sp: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.sp = Some(sp.to_hex_str());
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
    mut frame: *mut Handle<StackFrame>,
    symbol_address: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.symbol_address = Some(symbol_address.to_hex_str());
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
    mut frame: *mut Handle<StackFrame>,
    build_id: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.build_id = build_id.try_to_string_option()?;
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The BuildIdType must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_build_id_type(
    mut frame: *mut Handle<StackFrame>,
    build_id_type: BuildIdType,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.build_id_type = Some(build_id_type);
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
/// The FileType must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_file_type(
    mut frame: *mut Handle<StackFrame>,
    file_type: FileType,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.file_type = Some(file_type);
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
    mut frame: *mut Handle<StackFrame>,
    path: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.path = path.try_to_string_option()?;
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
    mut frame: *mut Handle<StackFrame>,
    relative_address: usize,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.relative_address = Some(relative_address.to_hex_str());
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_column(
    mut frame: *mut Handle<StackFrame>,
    column: u32,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.column = Some(column);
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
    mut frame: *mut Handle<StackFrame>,
    file: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.file = file.try_to_string_option()?;
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
    mut frame: *mut Handle<StackFrame>,
    function: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.function = function.try_to_string_option()?;
    })
}

/// # Safety
/// The `frame` can be null, but if non-null it must point to a Frame made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_StackFrame_with_line(
    mut frame: *mut Handle<StackFrame>,
    line: u32,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        frame.to_inner_mut()?.line = Some(line);
    })
}
