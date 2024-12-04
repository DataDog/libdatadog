// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod datatypes;
pub use datatypes::*;

use crate::{option_from_char_slice, Result};
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice, Slice, Timespec};
use ddcommon_net1::Endpoint;

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
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        crashinfo.normalize_ips(pid)
    })()
    .context("ddog_crasht_CrashInfo_normalize_ips failed")
    .into()
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
pub unsafe extern "C" fn ddog_crasht_CrashInfo_add_counter(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
    val: i64,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let name = name.to_utf8_lossy();
        crashinfo.add_counter(&name, val)
    })()
    .context("ddog_crasht_CrashInfo_add_counter failed")
    .into()
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
pub unsafe extern "C" fn ddog_crasht_CrashInfo_add_file(
    crashinfo: *mut CrashInfo,
    filename: CharSlice,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let filename = filename.to_utf8_lossy();
        crashinfo.add_file(&filename)
    })()
    .context("ddog_crasht_CrashInfo_add_file failed")
    .into()
}

/// Adds the tag with given "key" and "value" to the crashinfo
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// `key` should be a valid reference to a utf8 encoded String.
/// `value` should be a valid reference to a utf8 encoded String.
/// The string is copied into the crashinfo, so it does not need to outlive this
/// call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_add_tag(
    crashinfo: *mut CrashInfo,
    key: CharSlice,
    value: CharSlice,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let key = key.to_utf8_lossy().to_string();
        let value = value.to_utf8_lossy().to_string();
        crashinfo.add_tag(key, value)
    })()
    .context("ddog_crasht_CrashInfo_add_tag failed")
    .into()
}

/// Sets the crashinfo metadata
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// All references inside `metadata` must be valid.
/// Strings are copied into the crashinfo, and do not need to outlive this call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_set_metadata(
    crashinfo: *mut CrashInfo,
    metadata: Metadata,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let metadata = metadata.try_into()?;
        crashinfo.set_metadata(metadata)
    })()
    .context("ddog_crasht_CrashInfo_set_metadata failed")
    .into()
}

/// Sets the crashinfo siginfo
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// All references inside `metadata` must be valid.
/// Strings are copied into the crashinfo, and do not need to outlive this call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_set_siginfo(
    crashinfo: *mut CrashInfo,
    siginfo: SigInfo,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let siginfo = siginfo.try_into()?;
        crashinfo.set_siginfo(siginfo)
    })()
    .context("ddog_crasht_CrashInfo_set_siginfo failed")
    .into()
}

/// If `thread_id` is empty, sets `stacktrace` as the default stacktrace.
/// Otherwise, adds an additional stacktrace with id "thread_id".
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// All references inside `stacktraces` must be valid.
/// Strings are copied into the crashinfo, and do not need to outlive this call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_set_stacktrace(
    crashinfo: *mut CrashInfo,
    thread_id: CharSlice,
    stacktrace: Slice<StackFrame>,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let thread_id = option_from_char_slice(thread_id)?;
        let mut stacktrace_vec = Vec::with_capacity(stacktrace.len());
        for s in stacktrace.iter() {
            stacktrace_vec.push(s.try_into()?)
        }
        crashinfo.set_stacktrace(thread_id, stacktrace_vec)
    })()
    .context("ddog_crasht_CrashInfo_set_stacktrace failed")
    .into()
}

/// Sets the timestamp to the given unix timestamp
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_set_timestamp(
    crashinfo: *mut CrashInfo,
    ts: Timespec,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        crashinfo.set_timestamp(ts.into())
    })()
    .context("ddog_crasht_CrashInfo_set_timestamp_to_now failed")
    .into()
}

/// Sets the timestamp to the current time
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_set_timestamp_to_now(
    crashinfo: *mut CrashInfo,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        crashinfo.set_timestamp_to_now()
    })()
    .context("ddog_crasht_CrashInfo_set_timestamp_to_now failed")
    .into()
}

/// Sets crashinfo procinfo
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_set_procinfo(
    crashinfo: *mut CrashInfo,
    procinfo: ProcInfo,
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let procinfo = procinfo.try_into()?;
        crashinfo.set_procinfo(procinfo)
    })()
    .context("ddog_crasht_CrashInfo_set_procinfo failed")
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
) -> Result {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let endpoint = endpoint.cloned();
        crashinfo.upload_to_endpoint(&endpoint)
    })()
    .context("ddog_crasht_CrashInfo_upload_to_endpoint failed")
    .into()
}
