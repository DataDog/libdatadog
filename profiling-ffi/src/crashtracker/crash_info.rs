// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::crashtracker::{
    crashinfo_ptr_to_inner, option_from_char_slice, CrashInfo, CrashInfoNewResult,
    CrashtrackerResult, StackFrame,
};
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice, Slice};

use super::{CrashtrackerConfiguration, CrashtrackerMetadata, SigInfo};

/// Create a new crashinfo, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashinfo_new() -> CrashInfoNewResult {
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
pub unsafe extern "C" fn ddog_crashinfo_add_counter(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
    val: i64,
) -> CrashtrackerResult {
    unsafe fn inner(crashinfo: *mut CrashInfo, name: CharSlice, val: i64) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let name = name.to_utf8_lossy();
        crashinfo.add_counter(&name, val)?;
        Ok(())
    }
    inner(crashinfo, name, val)
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
    unsafe fn inner(crashinfo: *mut CrashInfo, name: CharSlice) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let name = name.to_utf8_lossy();
        crashinfo.add_file(&name)?;
        Ok(())
    }
    inner(crashinfo, name)
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
    unsafe fn inner(
        crashinfo: *mut CrashInfo,
        metadata: CrashtrackerMetadata,
    ) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let metadata = metadata.try_into()?;
        crashinfo.set_metadata(metadata)?;
        Ok(())
    }
    inner(crashinfo, metadata)
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
    unsafe fn inner(crashinfo: *mut CrashInfo, siginfo: SigInfo) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let siginfo = siginfo.try_into()?;
        crashinfo.set_siginfo(siginfo)?;
        Ok(())
    }
    inner(crashinfo, siginfo)
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
    unsafe fn inner(
        crashinfo: *mut CrashInfo,
        thread_id: CharSlice,
        stacktrace: Slice<StackFrame>,
    ) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let thread_id = option_from_char_slice(thread_id)?;
        let mut stacktrace_vec = Vec::with_capacity(stacktrace.len());
        for s in stacktrace.iter() {
            stacktrace_vec.push(s.try_into()?)
        }
        crashinfo.set_stacktrace(thread_id, stacktrace_vec)?;
        Ok(())
    }
    inner(crashinfo, thread_id, stacktrace)
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
    unsafe fn inner(
        crashinfo: *mut CrashInfo,
        config: CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let config = config.try_into()?;
        crashinfo.upload_to_telemetry(&config)?;
        Ok(())
    }
    inner(crashinfo, config)
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
    config: CrashtrackerConfiguration,
) -> CrashtrackerResult {
    unsafe fn inner(
        crashinfo: *mut CrashInfo,
        config: CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
        let config: datadog_crashtracker::CrashtrackerConfiguration = config.try_into()?;
        let endpoint = config.endpoint.context("Expected endpoint")?;
        crashinfo.upload_to_endpoint(endpoint, config.timeout)?;
        Ok(())
    }

    inner(crashinfo, config)
        .context("ddog_crashinfo_upload_to_endpoint failed")
        .into()
}
