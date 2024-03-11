// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{CrashtrackerConfiguration, CrashtrackerMetadata, SigInfo};
use crate::crashtracker::{
    crashinfo_ptr_to_inner, option_from_char_slice, CrashInfo, CrashInfoNewResult,
    CrashtrackerResult, StackFrame,
};
use crate::exporter::{self, Endpoint};
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice, Slice};
use std::time::Duration;

/// Create a new crashinfo, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashinfo_new() -> CrashInfoNewResult {
    CrashInfoNewResult::Ok(CrashInfo::new(datadog_crashtracker::CrashInfo::new()))
}

/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a CrashInfo
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crashinfo_drop(crashinfo: *mut CrashInfo) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !crashinfo.is_null() {
        drop((*crashinfo).take())
    }
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
pub unsafe extern "C" fn ddog_crashinfo_add_counter(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
    val: i64,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let name = name.to_utf8_lossy();
        crashinfo.add_counter(&name, val)
    })()
    .context("ddog_crashinfo_add_counter failed")
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
pub unsafe extern "C" fn ddog_crashinfo_add_file(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let name = name.to_utf8_lossy();
        crashinfo.add_file(&name)
    })()
    .context("ddog_crashinfo_add_file failed")
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
pub unsafe extern "C" fn ddog_crashinfo_set_metadata(
    crashinfo: *mut CrashInfo,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let metadata = metadata.try_into()?;
        crashinfo.set_metadata(metadata)
    })()
    .context("ddog_crashinfo_set_metadata failed")
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
pub unsafe extern "C" fn ddog_crashinfo_set_siginfo(
    crashinfo: *mut CrashInfo,
    siginfo: SigInfo,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let siginfo = siginfo.try_into()?;
        crashinfo.set_siginfo(siginfo)
    })()
    .context("ddog_crashinfo_set_siginfo failed")
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
pub unsafe extern "C" fn ddog_crashinfo_set_stacktrace(
    crashinfo: *mut CrashInfo,
    thread_id: CharSlice,
    stacktrace: Slice<StackFrame>,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let thread_id = option_from_char_slice(thread_id)?;
        let mut stacktrace_vec = Vec::with_capacity(stacktrace.len());
        for s in stacktrace.iter() {
            stacktrace_vec.push(s.try_into()?)
        }
        crashinfo.set_stacktrace(thread_id, stacktrace_vec)
    })()
    .context("ddog_crashinfo_set_metadata failed")
    .into()
}

/// Exports `crashinfo` to the Instrumentation Telemetry backend
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashinfo_upload_to_telemetry(
    crashinfo: *mut CrashInfo,
    config: CrashtrackerConfiguration,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let config = config.try_into()?;
        crashinfo.upload_to_telemetry(&config)
    })()
    .context("ddog_crashinfo_upload_to_telemetry failed")
    .into()
}

/// Exports `crashinfo` to the profiling backend at `endpoint`
/// Note that we support the "file://" endpoint for local file output.
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashinfo_upload_to_endpoint(
    crashinfo: *mut CrashInfo,
    endpoint: Endpoint,
    timeout_secs: u64,
) -> CrashtrackerResult {
    (|| {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let endpoint = exporter::try_to_endpoint(endpoint)?;
        let timeout = Duration::from_secs(timeout_secs);
        crashinfo.upload_to_endpoint(endpoint, timeout)?;
        anyhow::Ok(())
    })()
    .context("ddog_crashinfo_upload_to_endpoint failed")
    .into()
}
