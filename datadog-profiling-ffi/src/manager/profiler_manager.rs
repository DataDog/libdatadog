use std::sync::atomic::AtomicBool;
use std::sync::Arc;
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
pub struct ManagerCallbacks {
    pub cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    pub upload_callback:
        extern "C" fn(*mut Handle<internal::Profile>, &mut Option<CancellationToken>),
    pub sample_callbacks: ManagedSampleCallbacks,
}

#[repr(C)]
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
#[derive(Clone)]
pub struct ManagedSampleCallbacks {
    // Static is probably the wrong type here, but worry about that later.
    converter: extern "C" fn(&SendSample) -> Sample,
    // Resets the sample for reuse.
    reset: extern "C" fn(&mut SendSample),
    // Called when a sample is dropped (not recycled)
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
    pub fn start(
        profile: internal::Profile,
        callbacks: ManagerCallbacks,
        config: ProfilerManagerConfig,
    ) -> Result<ManagedProfilerClient> {
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

        Ok(ManagedProfilerClient::new(
            client_channels,
            handle,
            shutdown_sender,
            is_shutdown,
        ))
    }

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
            .map_err(|e| anyhow::anyhow!("Failed to send profile for upload: {}", e))?;
        Ok(())
    }

    /// # Safety
    /// - The caller must ensure that the callbacks remain valid for the lifetime of the profiler.
    /// - The callbacks must be thread-safe.
    pub fn handle_shutdown(mut self) -> Result<internal::Profile> {
        // Mark as shutdown
        self.is_shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);

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

        // Cancel any ongoing upload
        self.cancellation_token.cancel();

        // Drop the sender to signal the upload thread that no more messages will be sent
        // This is necessary to allow the upload thread to exit its message processing loop
        drop(self.upload_sender);

        // Wait for the upload thread to finish
        if let Err(e) = self.upload_thread.join() {
            eprintln!("Error joining upload thread: {:?}", e);
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
                        .map_err(|e| eprintln!("Failed to process sample: {}", e));
                },
                recv(self.cpu_ticker) -> msg => {
                    self.handle_cpu_tick();
                },
                recv(self.upload_ticker) -> msg => {
                    let _ = self.handle_upload_tick()
                        .map_err(|e| eprintln!("Failed to handle upload: {}", e));
                },
                recv(self.shutdown_receiver) -> _ => {
                    return self.handle_shutdown();
                },
            }
        }
    }
}
