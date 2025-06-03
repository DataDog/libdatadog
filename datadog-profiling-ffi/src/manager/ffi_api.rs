// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::manager::{
    profiler_manager::{ManagedSampleCallbacks, ProfilerManager},
    ManagedProfilerClient,
};
use crate::profiles::datatypes::{Period, ValueType};
use anyhow::Ok;
use datadog_profiling::internal;
use ddcommon_ffi::{
    wrap_with_ffi_result, wrap_with_void_ffi_result, Handle, Result as FFIResult, Slice, ToInner,
    VoidResult,
};
use function_name::named;
use tokio_util::sync::CancellationToken;

/// # Safety
/// - The caller is responsible for eventually calling the appropriate shutdown and cleanup
///   functions.
/// - The sample_callbacks must remain valid for the lifetime of the profiler.
/// - This function is not thread-safe.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_start(
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    upload_callback: extern "C" fn(*mut internal::Profile, &mut Option<CancellationToken>),
    sample_callbacks: ManagedSampleCallbacks,
) -> FFIResult<Handle<ManagedProfilerClient>> {
    wrap_with_ffi_result!({
        let sample_types_vec: Vec<_> = sample_types.into_slice().iter().map(Into::into).collect();
        let period_opt = period.map(Into::into);
        let client: ManagedProfilerClient = ProfilerManager::start(
            &sample_types_vec,
            period_opt,
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks,
        )?;
        Ok(Handle::from(client))
    })
}

/// # Safety
/// - The handle must have been returned by ddog_prof_ProfilerManager_start and not yet dropped.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_shutdown(
    handle: *mut Handle<ManagedProfilerClient>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let handle = handle.as_mut().context("Invalid handle")?;
        let client = handle
            .take()
            .context("Failed to take ownership of client")?;
        client
            .shutdown()
            .map_err(|e| anyhow::anyhow!("Failed to shutdown client: {:?}", e))?;
    })
}

/// # Safety
/// - The handle must have been returned by ddog_prof_ProfilerManager_start and not yet dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_drop(
    handle: *mut Handle<ManagedProfilerClient>,
) {
    if let Some(handle) = handle.as_mut() {
        let _ = handle.take();
    }
}
