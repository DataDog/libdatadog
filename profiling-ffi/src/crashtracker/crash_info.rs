// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::crashtracker::{crashinfo_ptr_to_inner,CrashInfo, CrashInfoNewResult, CrashtrackerResult};
use ddcommon_ffi::{slice::AsBytes, CharSlice, Error};

/// Create a new crashinfo, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashtracker_crashinfo_new() -> CrashInfoNewResult {
    CrashInfoNewResult::Ok(CrashInfo::new(datadog_crashtracker::CrashInfo::new()))
}

/// Create a new crashinfo, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashtracker_crashinfo_add_counter(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
    val: i64,
) -> CrashtrackerResult {
    match ddog_crashtracker_crashinfo_add_counter_impl(crashinfo, name, val) {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
            err.context("ddog_crashtracker_crashinfo_add_counter failed"),
        )),
    }
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
