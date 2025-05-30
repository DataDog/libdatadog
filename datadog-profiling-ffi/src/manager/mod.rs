// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

pub mod uploader;

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

pub struct ProfilerManager {
    samples_receiver: Receiver<*mut c_void>,
    samples_sender: Sender<*mut c_void>,
    recycled_samples_receiver: Receiver<*mut c_void>,
    recycled_samples_sender: Sender<*mut c_void>,
    cpu_ticker: Receiver<Instant>,
    upload_ticker: Receiver<Instant>,
    profile: internal::Profile,
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    upload_callback: extern "C" fn(*mut internal::Profile),
    sample_callbacks: ManagedSampleCallbacks,
}

impl ProfilerManager {
    pub fn new(
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
        cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
        upload_callback: extern "C" fn(*mut internal::Profile),
        sample_converter: extern "C" fn(*mut c_void) -> Sample<'static>,
        reset_callback: extern "C" fn(*mut c_void),
        drop_callback: extern "C" fn(*mut c_void),
    ) -> Self {
        let (samples_sender, samples_receiver) = crossbeam_channel::bounded(10);
        let (recycled_samples_sender, recycled_samples_receiver) = crossbeam_channel::bounded(10);
        let profile = internal::Profile::new(sample_types, period);
        let cpu_ticker = tick(Duration::from_millis(100));
        let upload_ticker = tick(Duration::from_secs(1));
        Self {
            cpu_ticker,
            upload_ticker,
            samples_receiver,
            samples_sender,
            recycled_samples_receiver,
            recycled_samples_sender,
            profile,
            cpu_sampler_callback,
            upload_callback,
            sample_callbacks: ManagedSampleCallbacks::new(sample_converter, reset_callback, drop_callback),
        }
    }

    pub fn main(&mut self) -> anyhow::Result<()> {
        // This is just here to allow us to easily bail out.
        let done = tick(Duration::from_secs(5));
        loop {
            select! {
                recv(self.samples_receiver) -> raw_sample => {
                    let data = raw_sample?;
                    let sample = (self.sample_callbacks.converter)(data);
                    self.profile.add_sample(sample.try_into()?, None)?;
                    (self.sample_callbacks.reset)(data);
                    if self.recycled_samples_sender.send(data).is_err() {
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
        let mut profile_manager = ProfilerManager::new(
            &sample_types,
            period,
            test_cpu_sampler_callback,
            test_upload_callback,
            test_sample_converter,
            test_reset_callback,
            test_drop_callback,
        );
        println!("start");
        profile_manager.main().unwrap();
    }
}
