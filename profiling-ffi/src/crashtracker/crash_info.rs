// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::crashtracker::{
    crashinfo_ptr_to_inner, CrashInfo, CrashInfoNewResult, CrashtrackerResult,
};
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice};

use super::CrashtrackerMetadata;

/// Create a new crashinfo, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashtracker_crashinfo_new() -> CrashInfoNewResult {
    CrashInfoNewResult::Ok(CrashInfo::new(datadog_crashtracker::CrashInfo::new()))
}

/// Adds a "counter" variable, with the given value.  Useful for determining if
/// "interesting" operations were occurring when the crash did.
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// `name` should be a valid reference to a utf8 encoded String.
/// The string is copied into the crashinfo, so it does not need to outlive this
/// call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashtracker_crashinfo_add_counter(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
    val: i64,
) -> CrashtrackerResult {
    ddog_crashtracker_crashinfo_add_counter_impl(crashinfo, name, val)
        .context("ddog_crashtracker_crashinfo_add_counter failed")
        .into()
}

unsafe fn ddog_crashtracker_crashinfo_add_counter_impl(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
    val: i64,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let name = name.to_utf8_lossy();
    crashinfo.add_counter(&name, val)?;
    Ok(())
}

/// Adds the contents of "file" to the crashinfo
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// `name` should be a valid reference to a utf8 encoded String.
/// The string is copied into the crashinfo, so it does not need to outlive this
/// call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashtracker_crashinfo_add_file(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
) -> CrashtrackerResult {
    ddog_crashtracker_crashinfo_add_file_impl(crashinfo, name)
        .context("ddog_crashtracker_crashinfo_add_file failed")
        .into()
}

unsafe fn ddog_crashtracker_crashinfo_add_file_impl(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let name = name.to_utf8_lossy();
    crashinfo.add_file(&name)?;
    Ok(())
}

/// Adds the contents of "file" to the crashinfo
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// `name` should be a valid reference to a utf8 encoded String.
/// The string is copied into the crashinfo, so it does not need to outlive this
/// call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashtracker_crashinfo_set_metadata(
    crashinfo: *mut CrashInfo,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    ddog_crashtracker_crashinfo_set_metadata_impl(crashinfo, metadata)
        .context("ddog_crashtracker_crashinfo_set_metadata failed")
        .into()
}

unsafe fn ddog_crashtracker_crashinfo_set_metadata_impl(
    crashinfo: *mut CrashInfo,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let metadata = metadata.try_into()?;
    crashinfo.set_metadata(metadata)?;
    Ok(())
}

// unsafe fn ddog_crashtracker_crashinfo_set_stacktrace(
//     crashinfo: *mut CrashInfo,
//     metadata: CrashtrackerMetadata,
// )
