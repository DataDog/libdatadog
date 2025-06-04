use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossbeam_channel::{select, tick, Receiver, Sender};
use datadog_profiling::internal;
use tokio_util::sync::CancellationToken;

use super::client::ManagedProfilerClient;
use super::samples::{SampleChannels, SendSample};
use crate::profiles::datatypes::Sample;

#[repr(C)]
#[derive(Clone)]
pub struct ManagedSampleCallbacks {
    // Static is probably the wrong type here, but worry about that later.
    converter: extern "C" fn(*mut c_void) -> Sample<'static>,
    // Resets the sample for reuse.
    reset: extern "C" fn(*mut c_void),
    // Called when a sample is dropped (not recycled)
    drop: extern "C" fn(*mut c_void),
}

impl ManagedSampleCallbacks {
    pub fn new(
        converter: extern "C" fn(*mut c_void) -> Sample<'static>,
        reset: extern "C" fn(*mut c_void),
        drop: extern "C" fn(*mut c_void),
    ) -> Self {
        Self {
            converter,
            reset,
            drop,
        }
    }
}

pub struct ProfilerManager {
    samples_receiver: Receiver<SendSample>,
    recycled_samples_sender: Sender<SendSample>,
    cpu_ticker: Receiver<Instant>,
    upload_ticker: Receiver<Instant>,
    shutdown_receiver: Receiver<()>,
    profile: internal::Profile,
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    upload_callback: extern "C" fn(*mut internal::Profile, &mut Option<CancellationToken>),
    sample_callbacks: ManagedSampleCallbacks,
    cancellation_token: Option<CancellationToken>,
}

impl ProfilerManager {
    pub fn start(
        profile: internal::Profile,
        cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
        upload_callback: extern "C" fn(*mut internal::Profile, &mut Option<CancellationToken>),
        sample_callbacks: ManagedSampleCallbacks,
    ) -> Result<ManagedProfilerClient> {
        let (channels, samples_receiver, recycled_samples_sender) = SampleChannels::new();
        let (shutdown_sender, shutdown_receiver) = crossbeam_channel::bounded(1);
        // For adaptive sampling, we need to be able to adjust this duration.  Look into how to do
        // this.
        let cpu_ticker = tick(Duration::from_millis(100));
        // one second for testing, make this 1 minute in production
        let upload_ticker = tick(Duration::from_secs(1));
        let manager = Self {
            cpu_ticker,
            upload_ticker,
            samples_receiver,
            recycled_samples_sender,
            shutdown_receiver,
            profile,
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks,
            cancellation_token: None,
        };

        let handle = std::thread::spawn(move || manager.main());

        Ok(ManagedProfilerClient::new(
            channels,
            handle,
            shutdown_sender,
        ))
    }

    fn handle_sample(
        &mut self,
        raw_sample: Result<SendSample, crossbeam_channel::RecvError>,
    ) -> Result<()> {
        let data = raw_sample?.as_ptr();
        let sample = (self.sample_callbacks.converter)(data);
        self.profile.add_sample(sample.try_into()?, None)?;
        (self.sample_callbacks.reset)(data);
        // SAFETY: The sample pointer is valid because it came from the samples channel
        // and was just processed by the converter and reset callbacks. We have exclusive
        // access to it since we're the only thread that can receive from the samples channel.
        if self
            .recycled_samples_sender
            .send(unsafe { SendSample::new(data) })
            .is_err()
        {
            (self.sample_callbacks.drop)(data);
        }
        Ok(())
    }

    fn handle_cpu_tick(&mut self) {
        (self.cpu_sampler_callback)(&mut self.profile);
    }

    fn handle_upload_tick(&mut self) -> Result<()> {
        let mut old_profile = self.profile.reset_and_return_previous()?;
        let upload_callback = self.upload_callback;
        // Create a new cancellation token for this upload
        let token = CancellationToken::new();
        // Store a clone of the token in the manager
        self.cancellation_token = Some(token.clone());
        let mut cancellation_token = Some(token);
        std::thread::spawn(move || {
            (upload_callback)(&mut old_profile, &mut cancellation_token);
            // TODO: make sure we cleanup the profile.
        });
        Ok(())
    }

    fn handle_shutdown(&mut self) -> Result<()> {
        // TODO: a mechanism to force threads to wait to write to the channel.
        // Drain any remaining samples and drop them
        while let Ok(sample) = self.samples_receiver.try_recv() {
            (self.sample_callbacks.drop)(sample.as_ptr());
        }
        // Cancel any ongoing upload and drop the token
        if let Some(token) = self.cancellation_token.take() {
            token.cancel();
        }
        // TODO: cleanup the recycled samples.
        Ok(())
    }

    /// # Safety
    /// - The caller must ensure that the callbacks remain valid for the lifetime of the profiler.
    /// - The callbacks must be thread-safe.
    pub fn main(mut self) -> Result<internal::Profile> {
        loop {
            select! {
                recv(self.samples_receiver) -> raw_sample => {
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
