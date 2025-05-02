// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_crashtracker::CrashInfo;
use ddcommon::Endpoint;
use ddcommon_ffi::{wrap_with_void_ffi_result, Handle, ToInner, VoidResult};
use function_name::named;

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_drop(builder: *mut Handle<CrashInfo>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !builder.is_null() {
        drop((*builder).take())
    }
}

/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
#[cfg(unix)]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_normalize_ips(
    mut crash_info: *mut Handle<CrashInfo>,
    pid: u32,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        crash_info.to_inner_mut()?.normalize_ips(pid)?;
    })
}

/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
#[cfg(unix)]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_resolve_names(
    mut crash_info: *mut Handle<CrashInfo>,
    pid: u32,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        crash_info.to_inner_mut()?.resolve_names(pid)?;
    })
}

/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_upload_to_endpoint(
    mut crash_info: *mut Handle<CrashInfo>,
    endpoint: Option<&Endpoint>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        crash_info
            .to_inner_mut()?
            .upload_to_endpoint(&endpoint.cloned())?;
    })
}
