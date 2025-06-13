#![allow(clippy::unwrap_used)]

use super::client::ManagedProfilerClient;
use super::profiler_manager::{
    ManagedSampleCallbacks, ManagerCallbacks, ProfilerManager, ProfilerManagerConfig,
};
use anyhow::{Context, Result};
use datadog_profiling::internal::Profile;
use std::sync::Mutex;

// Global state for the profile manager
static MANAGER: Mutex<Option<ProfilerManager>> = Mutex::new(None);
static PROFILE: Mutex<Option<Profile>> = Mutex::new(None);
static CONFIG: Mutex<Option<ProfilerManagerConfig>> = Mutex::new(None);
static CALLBACKS: Mutex<Option<ManagerCallbacks>> = Mutex::new(None);

/// Stores the current profile manager state globally.
/// This should be called before forking to ensure clean state.
pub fn store_manager_state(
    manager: ProfilerManager,
    config: ProfilerManagerConfig,
    cpu_sampler_callback: extern "C" fn(*mut Profile),
    upload_callback: extern "C" fn(
        *mut ddcommon_ffi::Handle<Profile>,
        &mut Option<tokio_util::sync::CancellationToken>,
    ),
    sample_callbacks: ManagedSampleCallbacks,
) -> Result<()> {
    // Store the manager
    *MANAGER.lock().unwrap() = Some(manager);

    // Store the config and callbacks separately
    *CONFIG.lock().unwrap() = Some(config);
    *CALLBACKS.lock().unwrap() = Some(ManagerCallbacks {
        cpu_sampler_callback,
        upload_callback,
        sample_callbacks,
    });

    Ok(())
}

/// Shuts down the stored profile manager and stores its profile.
/// This should be called before forking to ensure clean state.
pub fn shutdown_stored_manager() -> Result<()> {
    let manager = MANAGER
        .lock()
        .unwrap()
        .take()
        .context("No profile manager stored")?;

    let profile = manager.handle_shutdown()?;

    // Store the profile
    *PROFILE.lock().unwrap() = Some(profile);

    Ok(())
}

/// Restarts the profile manager in the parent process with the stored profile.
/// This should be called after fork in the parent process.
pub fn restart_manager_in_parent() -> Result<ManagedProfilerClient> {
    let profile = PROFILE
        .lock()
        .unwrap()
        .take()
        .context("No profile stored")?;

    let config = CONFIG.lock().unwrap().take().context("No config stored")?;

    let callbacks = CALLBACKS
        .lock()
        .unwrap()
        .take()
        .context("No callbacks stored")?;

    ProfilerManager::start(profile, callbacks, config)
}

/// Restarts the profile manager in the child process with a fresh profile.
/// This should be called after fork in the child process.
pub fn restart_manager_in_child() -> Result<ManagedProfilerClient> {
    let config = CONFIG.lock().unwrap().take().context("No config stored")?;

    let callbacks = CALLBACKS
        .lock()
        .unwrap()
        .take()
        .context("No callbacks stored")?;

    let mut profile = PROFILE
        .lock()
        .unwrap()
        .take()
        .context("No profile stored")?;

    // Reset the profile, discarding the previous one
    let _ = profile.reset_and_return_previous()?;

    ProfilerManager::start(profile, callbacks, config)
}
