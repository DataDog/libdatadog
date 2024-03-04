// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::crashtracker::{
    crashinfo_ptr_to_inner, CrashInfo, CrashInfoNewResult, CrashtrackerResult, StackFrame,
};
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice, Slice};

use super::{CrashtrackerConfiguration, CrashtrackerMetadata};

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
    ddog_crashinfo_add_counter_impl(crashinfo, name, val)
        .context("ddog_crashinfo_add_counter failed")
        .into()
}

unsafe fn ddog_crashinfo_add_counter_impl(
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
pub unsafe extern "C" fn ddog_crashinfo_add_file(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
) -> CrashtrackerResult {
    ddog_crashinfo_add_file_impl(crashinfo, name)
        .context("ddog_crashinfo_add_file failed")
        .into()
}

unsafe fn ddog_crashinfo_add_file_impl(
    crashinfo: *mut CrashInfo,
    name: CharSlice,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let name = name.to_utf8_lossy();
    crashinfo.add_file(&name)?;
    Ok(())
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
    ddog_crashinfo_set_metadata_impl(crashinfo, metadata)
        .context("ddog_crashinfo_set_metadata failed")
        .into()
}

unsafe fn ddog_crashinfo_set_metadata_impl(
    crashinfo: *mut CrashInfo,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let metadata = metadata.try_into()?;
    crashinfo.set_metadata(metadata)?;
    Ok(())
}

/// Sets `stacktrace` as the default stacktrace on `crashinfo`.
///
/// # Safety
/// `crashinfo` must be a valid pointer to a `CrashInfo` object.
/// All references inside `stacktraces` must be valid.
/// Strings are copied into the crashinfo, and do not need to outlive this call.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crashinfo_set_stacktrace(
    crashinfo: *mut CrashInfo,
    stacktrace: Slice<StackFrame>,
) -> CrashtrackerResult {
    ddog_crashinfo_set_stacktrace_impl(crashinfo, stacktrace)
        .context("ddog_crashinfo_set_metadata failed")
        .into()
}

unsafe fn ddog_crashinfo_set_stacktrace_impl(
    crashinfo: *mut CrashInfo,
    stacktrace: Slice<StackFrame>,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let mut stacktrace_vec = Vec::with_capacity(stacktrace.len());
    for s in stacktrace.iter() {
        stacktrace_vec.push(s.try_into()?)
    }
    crashinfo.set_stacktrace(stacktrace_vec)?;
    Ok(())
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
    ddog_crashinfo_upload_to_telemetry_impl(crashinfo, config)
        .context("ddog_crashinfo_upload_to_telemetry failed")
        .into()
}

unsafe fn ddog_crashinfo_upload_to_telemetry_impl(
    crashinfo: *mut CrashInfo,
    config: CrashtrackerConfiguration,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let config = config.try_into()?;
    crashinfo.upload_to_telemetry(&config)?;
    Ok(())
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
    ddog_crashinfo_upload_to_endpoint_impl(crashinfo, config)
        .context("ddog_crashinfo_upload_to_endpoint failed")
        .into()
}

unsafe fn ddog_crashinfo_upload_to_endpoint_impl(
    crashinfo: *mut CrashInfo,
    config: CrashtrackerConfiguration,
) -> anyhow::Result<()> {
    let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
    let config: datadog_crashtracker::CrashtrackerConfiguration = config.try_into()?;
    let endpoint = config.endpoint.context("Expected endpoint")?;
    crashinfo.upload_to_endpoint(endpoint, config.timeout)?;
    Ok(())
}
