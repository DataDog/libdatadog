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
pub struct ManagedSample {
    data: *mut c_void,
    callback: extern "C" fn(*mut c_void, *mut internal::Profile),
}

// void add_sample(void *data, Profile *profile) {
//     Sample *sample = (Sample *)data;
//     Profile *profile = (Profile *)data;
//     profile->add_sample(sample);
// }

impl ManagedSample {
    /// Creates a new ManagedSample from a raw pointer and callback.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The pointer is valid and points to memory that will remain valid for the lifetime of the
    ///   ManagedSample
    /// - The pointer is properly aligned
    /// - The memory is properly initialized
    #[no_mangle]
    pub unsafe extern "C" fn new(
        data: *mut c_void,
        callback: extern "C" fn(*mut c_void, *mut internal::Profile),
    ) -> Self {
        Self { data, callback }
    }

    /// Returns the raw pointer to the underlying data.
    #[no_mangle]
    pub extern "C" fn as_ptr(&self) -> *mut c_void {
        self.data
    }

    #[allow(clippy::todo)]
    pub fn add(self, profile: &mut internal::Profile) {
        (self.callback)(self.data, profile);
    }
}

pub struct ProfilerManager {
    samples_receiver: Receiver<ManagedSample>,
    samples_sender: Sender<ManagedSample>,
    cpu_ticker: Receiver<Instant>,
    upload_ticker: Receiver<Instant>,
    profile: internal::Profile,
    cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
    sample_converter: extern "C" fn(ManagedSample) -> Sample<'static>,
    reset_callback: extern "C" fn(ManagedSample) -> bool,
}

impl ProfilerManager {
    pub fn new(
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
        cpu_sampler_callback: extern "C" fn(*mut internal::Profile),
        sample_converter: extern "C" fn(ManagedSample) -> Sample<'static>,
        reset_callback: extern "C" fn(ManagedSample) -> bool,
    ) -> Self {
        let (samples_sender, samples_receiver) = crossbeam_channel::bounded(10);
        let profile = internal::Profile::new(sample_types, period);
        let cpu_ticker = tick(Duration::from_millis(100));
        let upload_ticker = tick(Duration::from_secs(1));
        Self {
            cpu_ticker,
            upload_ticker,
            samples_receiver,
            samples_sender,
            profile,
            cpu_sampler_callback,
            sample_converter,
            reset_callback,
        }
    }

    #[allow(clippy::todo)]
    pub fn main(&mut self) -> anyhow::Result<()> {
        // This is just here to allow us to easily bail out.
        let done = tick(Duration::from_secs(5));
        loop {
            select! {
                recv(self.samples_receiver) -> sample => {
                    sample?.add(&mut self.profile);
                },
                recv(self.cpu_ticker) -> msg => {
                    (self.cpu_sampler_callback)(&mut self.profile);
                },
                recv(self.upload_ticker) -> msg => {
                    let old_profile = self.profile.reset_and_return_previous()?;
                    std::thread::spawn(move || {
                        if let Ok(encoded) = old_profile.serialize_into_compressed_pprof(None, None) {
                            println!("Successfully serialized profile");
                        }
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

    extern "C" fn test_cpu_sampler_callback(_: *mut datadog_profiling::internal::Profile) {}
    extern "C" fn test_sample_converter(_: ManagedSample) -> Sample<'static> {
        Sample {
            locations: ddcommon_ffi::Slice::empty(),
            values: ddcommon_ffi::Slice::empty(),
            labels: ddcommon_ffi::Slice::empty(),
        }
    }
    extern "C" fn test_reset_callback(_: ManagedSample) -> bool {
        false
    }

    #[test]
    fn test_the_thing() {
        let sample_types = [];
        let period = None;
        let mut profile_manager = ProfilerManager::new(
            &sample_types,
            period,
            test_cpu_sampler_callback,
            test_sample_converter,
            test_reset_callback,
        );
        println!("start");
        profile_manager.main().unwrap();
    }
}
