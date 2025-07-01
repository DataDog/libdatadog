#![allow(clippy::unwrap_used)]

use super::client::{ManagedProfilerClient, ManagedProfilerController};
use super::profiler_manager::{
    ManagedSampleCallbacks, ManagerCallbacks, ProfilerManager, ProfilerManagerConfig,
};
use anyhow::Result;
use datadog_profiling::internal::Profile;
use std::sync::Mutex;

/// Global state for the profile manager during fork operations
enum ManagerState {
    /// Manager has not been initialized yet
    Uninitialized,
    /// Manager is running with active profiler client and controller
    Running {
        client: ManagedProfilerClient,
        controller: ManagedProfilerController,
        config: ProfilerManagerConfig,
        callbacks: ManagerCallbacks,
    },
    /// Manager is paused (shutdown) with stored profile
    Paused {
        profile: Box<datadog_profiling::internal::Profile>,
        config: ProfilerManagerConfig,
        callbacks: ManagerCallbacks,
    },
}

// Global state for the profile manager
static MANAGER_STATE: Mutex<ManagerState> = Mutex::new(ManagerState::Uninitialized);

/// Starts a new profile manager and stores the global state.
/// Returns the client for external use.
pub fn start(
    profile: datadog_profiling::internal::Profile,
    config: ProfilerManagerConfig,
    cpu_sampler_callback: extern "C" fn(*mut Profile),
    upload_callback: extern "C" fn(
        *mut ddcommon_ffi::Handle<Profile>,
        &mut Option<tokio_util::sync::CancellationToken>,
    ),
    sample_callbacks: ManagedSampleCallbacks,
) -> Result<ManagedProfilerClient> {
    let (client, controller) = ProfilerManager::start(
        profile,
        ManagerCallbacks {
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks: sample_callbacks.clone(),
        },
        config,
    )?;

    let mut state = MANAGER_STATE.lock().unwrap();
    *state = ManagerState::Running {
        client: client.clone(),
        controller,
        config,
        callbacks: ManagerCallbacks {
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks,
        },
    };

    Ok(client)
}

/// Shuts down the stored profile manager and stores its profile.
/// This should be called before forking to ensure clean state.
pub fn shutdown_global_manager() -> Result<()> {
    let mut state = MANAGER_STATE.lock().unwrap();
    match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
        ManagerState::Running {
            client: _,
            controller,
            config,
            callbacks,
        } => {
            let profile = controller.shutdown()?;
            *state = ManagerState::Paused {
                profile: Box::new(profile),
                config,
                callbacks,
            };
            Ok(())
        }
        _ => Err(anyhow::anyhow!("Manager is not in running state")),
    }
}

/// Restarts the profile manager in the parent process with the stored profile.
/// This should be called after fork in the parent process.
pub fn restart_manager_in_parent() -> Result<(ManagedProfilerClient, ManagedProfilerController)> {
    let mut state = MANAGER_STATE.lock().unwrap();

    let (profile, config, callbacks) =
        match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
            ManagerState::Paused {
                profile,
                config,
                callbacks,
            } => (*profile, config, callbacks),
            _ => return Err(anyhow::anyhow!("Manager is not in paused state")),
        };

    ProfilerManager::start(profile, callbacks, config)
}

/// Restarts the profile manager in the child process with a fresh profile.
/// This should be called after fork in the child process.
pub fn restart_manager_in_child() -> Result<(ManagedProfilerClient, ManagedProfilerController)> {
    let mut state = MANAGER_STATE.lock().unwrap();

    let (mut profile, config, callbacks) =
        match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
            ManagerState::Paused {
                profile,
                config,
                callbacks,
            } => (*profile, config, callbacks),
            _ => return Err(anyhow::anyhow!("Manager is not in paused state")),
        };

    // Reset the profile, discarding the previous one
    let _ = profile.reset_and_return_previous()?;

    ProfilerManager::start(profile, callbacks, config)
}
