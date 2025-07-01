#![allow(clippy::unwrap_used)]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{select, tick, Receiver, Sender};
use datadog_profiling::internal;
use ddcommon_ffi::Handle;
use tokio_util::sync::CancellationToken;

use super::client::ManagedProfilerClient;
use super::samples::{ClientSampleChannels, ManagerSampleChannels, SendSample};
use crate::profiles::datatypes::Sample;

/// Holds the callbacks needed to restart the profile manager
#[derive(Copy, Clone)]
pub struct ManagerCallbacks {
    pub cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    pub upload_callback:
        extern "C" fn(*mut Handle<internal::Profile>, &mut Option<CancellationToken>),
    pub sample_callbacks: ManagedSampleCallbacks,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ProfilerManagerConfig {
    pub channel_depth: usize,
    pub cpu_sampling_interval_ms: u64,
    pub upload_interval_ms: u64,
}

impl Default for ProfilerManagerConfig {
    fn default() -> Self {
        Self {
            channel_depth: 10,
            cpu_sampling_interval_ms: 100, // 100ms
            upload_interval_ms: 60000,     // 1 minute
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ManagedSampleCallbacks {
    converter: extern "C" fn(&SendSample) -> Sample,
    reset: extern "C" fn(&mut SendSample),
    drop: extern "C" fn(SendSample),
}

impl ManagedSampleCallbacks {
    pub fn new(
        converter: extern "C" fn(&SendSample) -> Sample,
        reset: extern "C" fn(&mut SendSample),
        drop: extern "C" fn(SendSample),
    ) -> Self {
        Self {
            converter,
            reset,
            drop,
        }
    }
}

/// Controller for managing the profiler manager lifecycle
pub struct ManagedProfilerController {
    handle: JoinHandle<Result<internal::Profile>>,
    shutdown_sender: Sender<()>,
    is_shutdown: Arc<AtomicBool>,
}

impl ManagedProfilerController {
    pub fn new(
        handle: JoinHandle<Result<internal::Profile>>,
        shutdown_sender: Sender<()>,
        is_shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            handle,
            shutdown_sender,
            is_shutdown,
        }
    }

    pub fn shutdown(self) -> Result<internal::Profile> {
        anyhow::ensure!(
            !self.is_shutdown.load(std::sync::atomic::Ordering::SeqCst),
            "Profiler manager is already shutdown"
        );
        self.shutdown_sender.send(())?;
        self.handle
            .join()
            .map_err(|e| anyhow::anyhow!("Failed to join manager thread: {:?}", e))?
    }
}

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
        profile: Box<internal::Profile>,
        config: ProfilerManagerConfig,
        callbacks: ManagerCallbacks,
    },
}

// Global state for the profile manager
static MANAGER_STATE: Mutex<ManagerState> = Mutex::new(ManagerState::Uninitialized);

pub struct ProfilerManager {
    channels: ManagerSampleChannels,
    cpu_ticker: Receiver<Instant>,
    upload_ticker: Receiver<Instant>,
    shutdown_receiver: Receiver<()>,
    profile: internal::Profile,
    callbacks: ManagerCallbacks,
    cancellation_token: CancellationToken,
    upload_sender: Sender<internal::Profile>,
    upload_thread: JoinHandle<()>,
    is_shutdown: Arc<AtomicBool>,
}

impl ProfilerManager {
    // --- Member functions (instance methods) ---
    fn handle_sample(
        &mut self,
        raw_sample: Result<SendSample, crossbeam_channel::RecvError>,
    ) -> Result<()> {
        let mut sample = raw_sample?;
        let converted_sample = (self.callbacks.sample_callbacks.converter)(&sample);
        let add_result = self.profile.add_sample(converted_sample.try_into()?, None);
        (self.callbacks.sample_callbacks.reset)(&mut sample);
        self.channels
            .recycled_samples_sender
            .send(sample)
            .map_or_else(|e| (self.callbacks.sample_callbacks.drop)(e.0), |_| ());
        add_result
    }

    fn handle_cpu_tick(&mut self) {
        (self.callbacks.cpu_sampler_callback)(&mut self.profile);
    }

    fn handle_upload_tick(&mut self) -> Result<()> {
        let old_profile = self.profile.reset_and_return_previous()?;
        self.upload_sender
            .send(old_profile)
            .map_err(|e| anyhow::anyhow!("Failed to send profile for upload: {e}"))?;
        Ok(())
    }

    /// # Safety
    /// - The caller must ensure that the callbacks remain valid for the lifetime of the profiler.
    /// - The callbacks must be thread-safe.
    pub fn handle_shutdown(mut self) -> Result<internal::Profile> {
        // Mark as shutdown
        self.is_shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Cancel any ongoing upload
        self.cancellation_token.cancel();

        // Drop the sender to signal the upload thread that no more messages will be sent
        // This is necessary to allow the upload thread to exit its message processing loop
        drop(self.upload_sender);

        // Process any remaining samples
        while let Ok(sample) = self.channels.samples_receiver.try_recv() {
            let converted_sample = (self.callbacks.sample_callbacks.converter)(&sample);
            if let Ok(s) = converted_sample.try_into() {
                let _ = self.profile.add_sample(s, None);
            }
            (self.callbacks.sample_callbacks.drop)(sample);
        }

        // Drain recycled samples
        while let Ok(sample) = self.channels.recycled_samples_receiver.try_recv() {
            (self.callbacks.sample_callbacks.drop)(sample);
        }

        // Wait for the upload thread to finish
        if let Err(e) = self.upload_thread.join() {
            eprintln!("Error joining upload thread: {e:?}");
        }

        Ok(self.profile)
    }

    /// # Safety
    /// - The caller must ensure that the callbacks remain valid for the lifetime of the profiler.
    /// - The callbacks must be thread-safe.
    pub fn main(mut self) -> Result<internal::Profile> {
        loop {
            select! {
                recv(self.channels.samples_receiver) -> raw_sample => {
                    let _ = self.handle_sample(raw_sample)
                        .map_err(|e| eprintln!("Failed to process sample: {e}"));
                },
                recv(self.cpu_ticker) -> msg => {
                    self.handle_cpu_tick();
                },
                recv(self.upload_ticker) -> msg => {
                    let _ = self.handle_upload_tick()
                        .map_err(|e| eprintln!("Failed to handle upload: {e}"));
                },
                recv(self.shutdown_receiver) -> _ => {
                    return self.handle_shutdown();
                },
            }
        }
    }
}

impl ProfilerManager {
    // --- Global functions (static methods) ---
    /// Starts a new profile manager and stores the global state.
    /// Returns the client for external use.
    pub fn start(
        profile: internal::Profile,
        callbacks: ManagerCallbacks,
        config: ProfilerManagerConfig,
    ) -> Result<ManagedProfilerClient> {
        let mut state = MANAGER_STATE.lock().map_err(|e| anyhow::anyhow!("{}", e))?;

        // Check if manager is already initialized
        anyhow::ensure!(
            matches!(&*state, ManagerState::Uninitialized),
            "Manager is already initialized"
        );

        let (client_channels, manager_channels) = ClientSampleChannels::new(config.channel_depth);
        let (shutdown_sender, shutdown_receiver) = crossbeam_channel::bounded(1);
        let (upload_sender, upload_receiver) = crossbeam_channel::bounded(2);

        let cpu_ticker = tick(Duration::from_millis(config.cpu_sampling_interval_ms));
        let upload_ticker = tick(Duration::from_millis(config.upload_interval_ms));

        // Create a single cancellation token for all uploads
        let cancellation_token = CancellationToken::new();

        // Create shared shutdown state
        let is_shutdown = Arc::new(AtomicBool::new(false));

        // Spawn the upload thread
        let mut token = Some(cancellation_token.clone());
        let upload_thread = std::thread::spawn(move || {
            while let Ok(profile) = upload_receiver.recv() {
                let mut handle = Handle::from(profile);
                (callbacks.upload_callback)(&mut handle, &mut token);
            }
        });

        let manager = Self {
            channels: manager_channels,
            cpu_ticker,
            upload_ticker,
            shutdown_receiver,
            profile,
            callbacks,
            cancellation_token,
            upload_sender,
            upload_thread,
            is_shutdown: is_shutdown.clone(),
        };

        let handle = std::thread::spawn(move || manager.main());

        let client = ManagedProfilerClient::new(client_channels, is_shutdown.clone());

        let controller = ManagedProfilerController::new(handle, shutdown_sender, is_shutdown);

        *state = ManagerState::Running {
            client: client.clone(),
            controller,
            config,
            callbacks,
        };

        Ok(client)
    }

    pub fn pause() -> Result<()> {
        let mut state = MANAGER_STATE.lock().map_err(|e| anyhow::anyhow!("{}", e))?;

        // Check if manager is in running state
        anyhow::ensure!(
            matches!(&*state, ManagerState::Running { .. }),
            "Manager is not in running state"
        );

        // Extract the running state and replace with uninitialized
        let running_state = match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
            ManagerState::Running {
                client: _,
                controller,
                config,
                callbacks,
            } => (controller, config, callbacks),
            _ => unreachable!(), // We already checked above
        };

        let (controller, config, callbacks) = running_state;
        let profile = controller.shutdown()?;

        *state = ManagerState::Paused {
            profile: Box::new(profile),
            config,
            callbacks,
        };

        Ok(())
    }

    pub fn restart_in_parent() -> Result<ManagedProfilerClient> {
        let mut state = MANAGER_STATE.lock().map_err(|e| anyhow::anyhow!("{}", e))?;

        let (profile, config, callbacks) =
            match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
                ManagerState::Paused {
                    profile,
                    config,
                    callbacks,
                } => (*profile, config, callbacks),
                _ => anyhow::bail!("Manager is not in paused state"),
            };

        Self::start(profile, callbacks, config)
    }

    pub fn restart_in_child() -> Result<ManagedProfilerClient> {
        let mut state = MANAGER_STATE.lock().map_err(|e| anyhow::anyhow!("{}", e))?;

        let (mut profile, config, callbacks) =
            match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
                ManagerState::Paused {
                    profile,
                    config,
                    callbacks,
                } => (*profile, config, callbacks),
                _ => anyhow::bail!("Manager is not in paused state"),
            };

        // Reset the profile, discarding the previous one
        let _ = profile.reset_and_return_previous()?;

        Self::start(profile, callbacks, config)
    }

    /// Terminates the global profile manager and returns the final profile.
    /// This should be called when the profiler is no longer needed.
    pub fn terminate() -> Result<internal::Profile> {
        let mut state = MANAGER_STATE.lock().map_err(|e| anyhow::anyhow!("{}", e))?;

        // Check if manager is in running or paused state
        anyhow::ensure!(
            matches!(
                &*state,
                ManagerState::Running { .. } | ManagerState::Paused { .. }
            ),
            "Manager is not in running or paused state"
        );

        // Extract the profile and replace with uninitialized
        let profile = match std::mem::replace(&mut *state, ManagerState::Uninitialized) {
            ManagerState::Running {
                client: _,
                controller,
                config: _,
                callbacks: _,
            } => {
                // Shutdown the controller and get the profile
                controller.shutdown()?
            }
            ManagerState::Paused {
                profile,
                config: _,
                callbacks: _,
            } => {
                // Return the stored profile
                *profile
            }
            _ => unreachable!(), // We already checked above
        };

        Ok(profile)
    }
}
