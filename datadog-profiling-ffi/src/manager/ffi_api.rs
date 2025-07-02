// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::manager::{
    profiler_manager::{
        ManagedSampleCallbacks, ManagerCallbacks, ProfilerManager, ProfilerManagerConfig,
    },
    ManagedProfilerClient,
};
use crate::profiles::datatypes::{Profile, ProfilePtrExt};
use crossbeam_channel::TryRecvError;
use datadog_profiling::internal;
use ddcommon_ffi::{
    wrap_with_ffi_result, wrap_with_void_ffi_result, Handle, Result as FFIResult, ToInner,
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
/// - This function takes ownership of the profile. The profile must not be used after this call.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_start(
    profile: *mut Profile,
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    upload_callback: extern "C" fn(*mut Handle<internal::Profile>, &mut Option<CancellationToken>),
    sample_callbacks: ManagedSampleCallbacks,
    config: ProfilerManagerConfig,
) -> FFIResult<Handle<ManagedProfilerClient>> {
    wrap_with_ffi_result!({
        let internal_profile = *profile.take()?;
        let callbacks = ManagerCallbacks {
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks,
        };
        let client = ProfilerManager::start(internal_profile, callbacks, config)?;
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
    mut handle: *mut Handle<ManagedProfilerClient>,
    sample_ptr: *mut c_void,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        handle
            .to_inner_mut()?
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
    mut handle: *mut Handle<ManagedProfilerClient>,
) -> FFIResult<*mut c_void> {
    wrap_with_ffi_result!({
        match handle.to_inner_mut()?.try_recv_recycled() {
            Ok(sample_ptr) => anyhow::Ok(sample_ptr),
            Err(TryRecvError::Empty) => anyhow::Ok(std::ptr::null_mut()),
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Channel disconnected")),
        }
    })
}

/// Pauses the global profiler manager, shutting down the current instance and storing the profile.
/// The manager can be restarted later using restart functions.
///
/// # Safety
/// - This function is thread-safe and can be called from any thread.
/// - The manager must be in running state.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_pause() -> VoidResult {
    wrap_with_void_ffi_result!({
        ProfilerManager::pause().context("Failed to pause global manager")?;
    })
}

/// Restarts the profiler manager in the parent process after a fork.
/// This preserves the profile data from before the pause.
///
/// # Safety
/// - This function is thread-safe and can be called from any thread.
/// - The manager must be in paused state.
/// - This should be called in the parent process after a fork.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_restart_in_parent(
) -> FFIResult<Handle<ManagedProfilerClient>> {
    wrap_with_ffi_result!({
        let client =
            ProfilerManager::restart_in_parent().context("Failed to restart manager in parent")?;
        anyhow::Ok(Handle::from(client))
    })
}

/// Restarts the profiler manager in the child process after a fork.
/// This discards the profile data from before the pause and starts fresh.
///
/// # Safety
/// - This function is thread-safe and can be called from any thread.
/// - The manager must be in paused state.
/// - This should be called in the child process after a fork.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_restart_in_child(
) -> FFIResult<Handle<ManagedProfilerClient>> {
    wrap_with_ffi_result!({
        let client =
            ProfilerManager::restart_in_child().context("Failed to restart manager in child")?;
        anyhow::Ok(Handle::from(client))
    })
}

/// Terminates the global profiler manager and returns the final profile.
/// This should be called when the profiler is no longer needed.
///
/// # Safety
/// - This function is thread-safe and can be called from any thread.
/// - The manager must be in running or paused state.
/// - The returned profile handle must be properly managed by the caller.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_terminate(
) -> FFIResult<Handle<internal::Profile>> {
    wrap_with_ffi_result!({
        let profile = ProfilerManager::terminate().context("Failed to terminate global manager")?;
        anyhow::Ok(Handle::from(profile))
    })
}

/// Drops a profiler client handle.
/// This only drops the client handle and does not affect the global manager state.
///
/// # Safety
/// - The handle must have been returned by ddog_prof_ProfilerManager_start and not yet dropped.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerClient_drop(
    mut handle: *mut Handle<ManagedProfilerClient>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        handle
            .take()
            .context("Failed to drop profiler client handle")?;
    })
}

/// Resets the global profiler manager state to uninitialized.
/// This is intended for testing purposes only and should not be used in production.
///
/// # Safety
/// - This function is thread-safe and can be called from any thread.
/// - This function will forcefully reset the state without proper cleanup.
/// - This should only be used in test environments.
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_ProfilerManager_reset_for_testing() -> VoidResult {
    wrap_with_void_ffi_result!({
        ProfilerManager::reset_for_testing().map_err(|msg| anyhow::anyhow!(msg))?;
    })
}
