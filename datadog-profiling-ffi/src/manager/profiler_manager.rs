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
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    upload_callback: extern "C" fn(*mut Handle<internal::Profile>, &mut Option<CancellationToken>),
    sample_callbacks: ManagedSampleCallbacks,
    cancellation_token: CancellationToken,
    upload_sender: Sender<internal::Profile>,
    upload_thread: JoinHandle<()>,
}

impl ProfilerManager {
    pub fn start(
        profile: internal::Profile,
        cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
        upload_callback: extern "C" fn(
            *mut Handle<internal::Profile>,
            &mut Option<CancellationToken>,
        ),
        sample_callbacks: ManagedSampleCallbacks,
        config: ProfilerManagerConfig,
    ) -> Result<ManagedProfilerClient> {
        let (client_channels, manager_channels) = ClientSampleChannels::new(config.channel_depth);
        let (shutdown_sender, shutdown_receiver) = crossbeam_channel::bounded(1);
        let (upload_sender, upload_receiver) = crossbeam_channel::bounded(2);

        let cpu_ticker = tick(Duration::from_millis(config.cpu_sampling_interval_ms));
        let upload_ticker = tick(Duration::from_millis(config.upload_interval_ms));

        // Create a single cancellation token for all uploads
        let cancellation_token = CancellationToken::new();

        // Spawn the upload thread
        let mut token = Some(cancellation_token.clone());
        let upload_thread = std::thread::spawn(move || {
            while let Ok(profile) = upload_receiver.recv() {
                let mut handle = Handle::from(profile);
                (upload_callback)(&mut handle, &mut token);
            }
        });

        let manager = Self {
            channels: manager_channels,
            cpu_ticker,
            upload_ticker,
            shutdown_receiver,
            profile,
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks,
            cancellation_token,
            upload_sender,
            upload_thread,
        };

        let handle = std::thread::spawn(move || manager.main());

        Ok(ManagedProfilerClient::new(
            client_channels,
            handle,
            shutdown_sender,
        ))
    }

    fn handle_sample(
        &mut self,
        raw_sample: Result<SendSample, crossbeam_channel::RecvError>,
    ) -> Result<()> {
        let mut sample = raw_sample?;
        let converted_sample = (self.sample_callbacks.converter)(&sample);
        let add_result = self.profile.add_sample(converted_sample.try_into()?, None);
        (self.sample_callbacks.reset)(&mut sample);
        self.channels
            .recycled_samples_sender
            .send(sample)
            .map_or_else(|e| (self.sample_callbacks.drop)(e.0), |_| ());
        add_result
    }

    fn handle_cpu_tick(&mut self) {
        (self.cpu_sampler_callback)(&mut self.profile);
    }

    fn handle_upload_tick(&mut self) -> Result<()> {
        let old_profile = self.profile.reset_and_return_previous()?;
        self.upload_sender
            .send(old_profile)
            .map_err(|e| anyhow::anyhow!("Failed to send profile for upload: {}", e))?;
        Ok(())
    }

    fn handle_shutdown(&mut self) -> Result<()> {
        // Try to process any remaining samples before dropping them
        while let Ok(sample) = self.channels.samples_receiver.try_recv() {
            let converted_sample = (self.sample_callbacks.converter)(&sample);
            if let Ok(converted_sample) = converted_sample.try_into() {
                if let Err(e) = self.profile.add_sample(converted_sample, None) {
                    eprintln!("Failed to add sample during shutdown: {}", e);
                }
            } else {
                eprintln!("Failed to convert sample during shutdown");
            }
            (self.sample_callbacks.drop)(sample);
        }

        // Drain any recycled samples
        while let Ok(sample) = self.channels.recycled_samples_receiver.try_recv() {
            (self.sample_callbacks.drop)(sample);
        }

        // Cancel any ongoing upload
        self.cancellation_token.cancel();

        // Take ownership of the upload thread and sender
        let upload_thread = std::mem::replace(&mut self.upload_thread, std::thread::spawn(|| {}));
        let _ = std::mem::replace(&mut self.upload_sender, crossbeam_channel::bounded(1).0);

        // Wait for the upload thread to finish
        if let Err(e) = upload_thread.join() {
            eprintln!("Error joining upload thread: {:?}", e);
        }

        Ok(())
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
                    self.handle_shutdown()?;
                    break;
                },
            }
        }
        Ok(self.profile)
    }
}
