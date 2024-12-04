// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_crashtracker::rfc5_crash_info::CrashInfo;
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
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_upload_to_telemetry(
    mut info: *mut Handle<CrashInfo>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let _info = info.to_inner_mut()?;
    })
}
