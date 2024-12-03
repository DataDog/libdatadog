// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod datatypes;
pub use datatypes::*;
mod stackframe;
pub use stackframe::*;
mod stacktrace;
pub use stacktrace::*;
mod builder;
pub mod to_inner;
pub use builder::*;
mod metadata;
pub use metadata::*;
mod os_info;
pub use os_info::*;
mod proc_info;
pub use proc_info::*;
mod thread_data;
pub use thread_data::*;

use anyhow::Context;
use ddcommon::Endpoint;
use ddcommon_ffi::VoidResult;

/// Create a new crashinfo, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_new() -> CrashInfoNewResult {
    CrashInfoNewResult::Ok(CrashInfo::new(datadog_crashtracker::CrashInfo::new()))
}

/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a CrashInfo
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_drop(crashinfo: *mut CrashInfo) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !crashinfo.is_null() {
        drop((*crashinfo).take())
    }
}

/// Best effort attempt to normalize all `ip` on the stacktrace.
/// `pid` must be the pid of the currently active process where the ips came from.
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[cfg(unix)]
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_normalize_ips(
    crashinfo: *mut CrashInfo,
    pid: u32,
) -> VoidResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        crashinfo.normalize_ips(pid)
    })()
    .context("ddog_crasht_CrashInfo_normalize_ips failed")
    .into()
}

/// Exports `crashinfo` to the backend at `endpoint`
/// Note that we support the "file://" endpoint for local file output.
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_upload_to_endpoint(
    crashinfo: *mut CrashInfo,
    endpoint: Option<&Endpoint>,
) -> VoidResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let endpoint = endpoint.cloned();
        crashinfo.upload_to_endpoint(&endpoint)
    })()
    .context("ddog_crasht_CrashInfo_upload_to_endpoint failed")
    .into()
}
