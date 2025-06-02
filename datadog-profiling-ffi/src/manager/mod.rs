// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

use std::{
    ffi::c_void,
    num::NonZeroI64,
    time::{Duration, Instant},
};

use crate::profiles::datatypes::{ProfileResult, Sample};
use anyhow::Context;
use crossbeam_channel::{select, tick, Receiver, Sender};
use datadog_profiling::{api, internal};

#[repr(C)]
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

#[repr(transparent)]
pub struct SendSample(*mut c_void);

// SAFETY: This type is used to transfer ownership of a sample between threads via channels.
// The sample is only accessed by one thread at a time, and ownership is transferred along
// with the SendSample wrapper. The sample is either processed by the manager thread or
// recycled back to the original thread.
unsafe impl Send for SendSample {}

impl SendSample {
    pub fn new(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

pub struct SampleChannels {
    samples_sender: Sender<SendSample>,
    recycled_samples_receiver: Receiver<SendSample>,
}

impl SampleChannels {
    pub fn new() -> (Self, Receiver<SendSample>, Sender<SendSample>) {
        let (samples_sender, samples_receiver) = crossbeam_channel::bounded(10);
        let (recycled_samples_sender, recycled_samples_receiver) = crossbeam_channel::bounded(10);
        (
            Self {
                samples_sender,
                recycled_samples_receiver,
            },
            samples_receiver,
            recycled_samples_sender,
        )
    }

    /// # Safety
    /// The caller must ensure that:
    /// 1. The sample pointer is valid and points to a properly initialized sample
    /// 2. The caller transfers ownership of the sample to this function
    ///    - The sample is not being used by any other thread
    ///    - The sample must not be accessed by the caller after this call
    ///    - The manager will either free the sample or recycle it back
    /// 3. The sample will be properly cleaned up if it cannot be sent
    pub unsafe fn send_sample(
        &self,
        sample: *mut c_void,
    ) -> Result<(), crossbeam_channel::SendError<SendSample>> {
        self.samples_sender.send(SendSample::new(sample))
    }

    pub fn try_recv_recycled(&self) -> Result<*mut c_void, crossbeam_channel::TryRecvError> {
        self.recycled_samples_receiver.try_recv().map(|s| s.as_ptr())
    }
}

pub struct ProfilerManager {
    samples_receiver: Receiver<SendSample>,
    recycled_samples_sender: Sender<SendSample>,
    cpu_ticker: Receiver<Instant>,
    upload_ticker: Receiver<Instant>,
    profile: internal::Profile,
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    upload_callback: extern "C" fn(*mut internal::Profile),
    sample_callbacks: ManagedSampleCallbacks,
}

impl ProfilerManager {
    pub fn start(
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
        cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
        upload_callback: extern "C" fn(*mut internal::Profile),
        sample_callbacks: ManagedSampleCallbacks,
    ) -> SampleChannels {
        let (channels, samples_receiver, recycled_samples_sender) = SampleChannels::new();
        let profile = internal::Profile::new(sample_types, period);
        let cpu_ticker = tick(Duration::from_millis(100));
        let upload_ticker = tick(Duration::from_secs(1));
        let mut manager = Self {
            cpu_ticker,
            upload_ticker,
            samples_receiver,
            recycled_samples_sender,
            profile,
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks,
        };

        std::thread::spawn(move || {
            if let Err(e) = manager.main() {
                eprintln!("ProfilerManager error: {}", e);
            }
        });

        channels
    }

    fn main(&mut self) -> anyhow::Result<()> {
        // This is just here to allow us to easily bail out.
        let done = tick(Duration::from_secs(5));
        loop {
            select! {
                recv(self.samples_receiver) -> raw_sample => {
                    let data = raw_sample?.as_ptr();
                    let sample = (self.sample_callbacks.converter)(data);
                    self.profile.add_sample(sample.try_into()?, None)?;
                    (self.sample_callbacks.reset)(data);
                    if self.recycled_samples_sender.send(SendSample::new(data)).is_err() {
                        (self.sample_callbacks.drop)(data);
                    }
                },
                recv(self.cpu_ticker) -> msg => {
                    (self.cpu_sampler_callback)(&mut self.profile);
                },
                recv(self.upload_ticker) -> msg => {
                    let mut old_profile = self.profile.reset_and_return_previous()?;
                    let upload_callback = self.upload_callback;
                    std::thread::spawn(move || {
                        (upload_callback)(&mut old_profile);
                        // TODO: make sure we cleanup the profile.
                    });
                },
                recv(done) -> msg => return Ok(()),
            }
        }
    }
}

/// # Safety
/// The `profile` ptr must point to a valid internal::Profile object.
/// All pointers inside the `sample` need to be valid for the duration of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add_internal(
    profile: *mut internal::Profile,
    sample: Sample,
    timestamp: Option<NonZeroI64>,
) -> ProfileResult {
    (|| {
        let profile = profile
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("profile pointer was null"))?;
        let uses_string_ids = sample
            .labels
            .first()
            .is_some_and(|label| label.key.is_empty() && label.key_id.value > 0);

        if uses_string_ids {
            profile.add_string_id_sample(sample.into(), timestamp)
        } else {
            profile.add_sample(sample.try_into()?, timestamp)
        }
    })()
    .context("ddog_prof_Profile_add_internal failed")
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn test_cpu_sampler_callback(_: *mut datadog_profiling::internal::Profile) {
        println!("cpu sampler callback");
    }
    extern "C" fn test_upload_callback(_: *mut datadog_profiling::internal::Profile) {
        println!("upload callback");
    }
    extern "C" fn test_sample_converter(_: *mut c_void) -> Sample<'static> {
        println!("sample converter");
        Sample {
            locations: ddcommon_ffi::Slice::empty(),
            values: ddcommon_ffi::Slice::empty(),
            labels: ddcommon_ffi::Slice::empty(),
        }
    }
    extern "C" fn test_reset_callback(_: *mut c_void) {
        println!("reset callback");
    }
    extern "C" fn test_drop_callback(_: *mut c_void) {
        println!("drop callback");
    }

    #[test]
    fn test_the_thing() {
        let sample_types = [];
        let period = None;
        let sample_callbacks = ManagedSampleCallbacks::new(
            test_sample_converter,
            test_reset_callback,
            test_drop_callback,
        );
        let _channels = ProfilerManager::start(
            &sample_types,
            period,
            test_cpu_sampler_callback,
            test_upload_callback,
            sample_callbacks,
        );
        println!("start");
        std::thread::sleep(Duration::from_secs(5));
    }
}
