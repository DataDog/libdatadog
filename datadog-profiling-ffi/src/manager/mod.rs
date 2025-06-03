// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

use std::num::NonZeroI64;

use crate::profiles::datatypes::{ProfileResult, Sample};
use anyhow::Context;
use datadog_profiling::internal;

mod client;
mod profiler_manager;
mod samples;

pub use client::ManagedProfilerClient;
pub use profiler_manager::{ManagedSampleCallbacks, ProfilerManager};
pub use samples::{SampleChannels, SendSample};

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
    use std::{ffi::c_void, time::Duration};

    use super::*;
    use tokio_util::sync::CancellationToken;

    extern "C" fn test_cpu_sampler_callback(_: *mut datadog_profiling::internal::Profile) {
        println!("cpu sampler callback");
    }
    extern "C" fn test_upload_callback(
        _: *mut datadog_profiling::internal::Profile,
        _: &mut Option<CancellationToken>,
    ) {
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
        let handle = ProfilerManager::start(
            &sample_types,
            period,
            test_cpu_sampler_callback,
            test_upload_callback,
            sample_callbacks,
        );
        println!("start");
        std::thread::sleep(Duration::from_secs(5));
        handle.shutdown().unwrap();
    }
}
