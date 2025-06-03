// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::manager::{
    profiler_manager::{ManagedSampleCallbacks, ProfilerManager},
    ManagedProfilerClient,
};
use crate::profiles::datatypes::{Period, ValueType};
use crossbeam_channel::TryRecvError;
use datadog_profiling::internal;
use ddcommon_ffi::{
    wrap_with_ffi_result, wrap_with_void_ffi_result, Handle, Result as FFIResult, Slice, ToInner,
    VoidResult,
};
use function_name::named;
use std::ffi::c_void;
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
        anyhow::Ok(Handle::from(client))
    })
}

/// # Safety
/// - The handle must have been returned by ddog_prof_ProfilerManager_start and not yet dropped.
/// - The caller must ensure that:
///   1. The sample pointer is valid and points to a properly initialized sample
///   2. The caller transfers ownership of the sample to this function
///      - The sample is not being used by any other thread
///      - The sample must not be accessed by the caller after this call
///      - The manager will either free the sample or recycle it back
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_enqueue_sample(
    handle: *mut Handle<ManagedProfilerClient>,
    sample_ptr: *mut c_void,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let handle = handle.as_mut().context("Invalid handle")?;
        let client = handle.to_inner_mut()?;
        client
            .send_sample(sample_ptr)
            .map_err(|e| anyhow::anyhow!("Failed to send sample: {:?}", e))?;
    })
}

/// Attempts to receive a recycled sample from the profiler manager.
///
/// This function will:
/// - Return a valid sample pointer if a recycled sample is available
/// - Return a null pointer if the queue is empty (this is a valid success case)
/// - Return an error if the channel is disconnected
///
/// The caller should check if the returned pointer is null to determine if there were no samples
/// available.
///
/// # Safety
/// - The handle must have been returned by ddog_prof_ProfilerManager_start and not yet dropped.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_try_recv_recycled(
    handle: *mut Handle<ManagedProfilerClient>,
) -> FFIResult<*mut c_void> {
    wrap_with_ffi_result!({
        let handle = handle.as_mut().context("Invalid handle")?;
        let client = handle.to_inner_mut()?;
        match client.try_recv_recycled() {
            Ok(sample_ptr) => anyhow::Ok(sample_ptr),
            Err(TryRecvError::Empty) => anyhow::Ok(std::ptr::null_mut()),
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Channel disconnected")),
        }
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
// TODO: Do we want drop and shutdown to be separate functions? Or should it always be shutdown?
pub unsafe extern "C" fn ddog_prof_ProfilerManager_drop(
    handle: *mut Handle<ManagedProfilerClient>,
) {
    if let Some(handle) = handle.as_mut() {
        let _ = handle.take();
    }
}
