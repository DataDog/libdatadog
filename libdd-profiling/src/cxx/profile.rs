// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::dictionary::ProfileDictionary;
use super::errors::{EncodedProfileResult, ErrorStore, ProfileResult};
use super::ffi;
use super::ids::{dictionary_location_from_cxx, dictionary_string_id_from_cxx};
use crate::api;
use crate::api2;
use crate::internal;
use anyhow::Context;

pub struct Profile {
    pub(crate) inner: internal::Profile,
    errors: ErrorStore,
}

impl Profile {
    fn new(inner: internal::Profile) -> Self {
        Self {
            inner,
            errors: ErrorStore::new(),
        }
    }

    fn handle_result(&mut self, operation: &'static str, result: anyhow::Result<()>) -> bool {
        self.errors.handle_result(operation, result)
    }

    fn handle_error(&mut self, operation: &'static str, err: impl std::fmt::Display) -> bool {
        self.errors.handle_error(operation, err)
    }

    pub fn set_error_policy(&mut self, policy: ffi::ErrorPolicy) {
        self.errors.set_policy(policy);
    }

    pub fn error_policy(&self) -> ffi::ErrorPolicy {
        self.errors.policy()
    }

    pub fn errors(&self) -> Vec<ffi::Error> {
        self.errors.errors()
    }

    pub fn take_errors(&mut self) -> Vec<ffi::Error> {
        self.errors.take_errors()
    }

    pub fn clear_errors(&mut self) {
        self.errors.clear_errors();
    }

    pub fn create(sample_types: Vec<ffi::SampleType>, period: &ffi::Period) -> Box<ProfileResult> {
        ProfileResult::from_result(
            "Profile::create",
            (|| -> anyhow::Result<Box<Profile>> {
                // Convert (fallibly) from CXX types to API types
                let types: Vec<api::SampleType> = sample_types
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?;
                let period_value: api::Period = period.try_into()?;

                // Profile::try_new interns the strings
                let inner = internal::Profile::try_new(&types, Some(period_value))?;

                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn create_no_period(sample_types: Vec<ffi::SampleType>) -> Box<ProfileResult> {
        ProfileResult::from_result(
            "Profile::create_no_period",
            (|| -> anyhow::Result<Box<Profile>> {
                let types: Vec<api::SampleType> = sample_types
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?;
                let inner = internal::Profile::try_new(&types, None)?;
                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn create_with_dictionary(
        sample_types: Vec<ffi::SampleType>,
        period: &ffi::Period,
        dictionary: &ProfileDictionary,
    ) -> Box<ProfileResult> {
        ProfileResult::from_result(
            "Profile::create_with_dictionary",
            (|| -> anyhow::Result<Box<Profile>> {
                let types: Vec<api::SampleType> = sample_types
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?;
                let period_value: api::Period = period.try_into()?;
                let dictionary = dictionary
                    .inner
                    .try_clone()
                    .context("failed to clone ProfileDictionary for Profile")?;
                let inner = internal::Profile::try_new_with_dictionary(
                    &types,
                    Some(period_value),
                    dictionary,
                )
                .context("Profile::create_with_dictionary failed")?;
                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn add_sample(&mut self, sample: &ffi::Sample) -> bool {
        let api_sample = api::Sample {
            locations: sample.locations.iter().map(Into::into).collect(),
            values: sample.values,
            labels: sample.labels.iter().map(Into::into).collect(),
        };

        // Profile interns the strings
        let result = self
            .inner
            .try_add_sample(api_sample, None)
            .context("Profile::add_sample failed");
        self.handle_result("Profile::add_sample", result)
    }

    pub fn add_sample_with_timestamp(&mut self, sample: &ffi::Sample, endtime_ns: i64) -> bool {
        let result =
            match internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero") {
                Ok(timestamp) => {
                    let api_sample = api::Sample {
                        locations: sample.locations.iter().map(Into::into).collect(),
                        values: sample.values,
                        labels: sample.labels.iter().map(Into::into).collect(),
                    };

                    self.inner
                        .try_add_sample(api_sample, Some(timestamp))
                        .context("Profile::add_sample_with_timestamp failed")
                }
                Err(err) => Err(err),
            };
        self.handle_result("Profile::add_sample_with_timestamp", result)
    }

    /// Adds a dictionary-backed sample without an end timestamp.
    pub fn add_dictionary_sample(&mut self, sample: &ffi::DictionarySample) -> bool {
        self.add_dictionary_sample_impl(sample, None)
    }

    /// Adds a dictionary-backed sample with an end timestamp in nanoseconds.
    pub fn add_dictionary_sample_with_timestamp(
        &mut self,
        sample: &ffi::DictionarySample,
        endtime_ns: i64,
    ) -> bool {
        let result =
            match internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero") {
                Ok(timestamp) => self.add_dictionary_sample_result(sample, Some(timestamp)),
                Err(err) => Err(err),
            };
        self.handle_result("Profile::add_dictionary_sample", result)
    }

    fn add_dictionary_sample_impl(
        &mut self,
        sample: &ffi::DictionarySample,
        timestamp: Option<internal::Timestamp>,
    ) -> bool {
        let result = self.add_dictionary_sample_result(sample, timestamp);
        self.handle_result("Profile::add_dictionary_sample", result)
    }

    fn add_dictionary_sample_result(
        &mut self,
        sample: &ffi::DictionarySample,
        timestamp: Option<internal::Timestamp>,
    ) -> anyhow::Result<()> {
        let locations_iter = sample.locations.iter().map(|location| {
            // SAFETY: The CXX API contract requires all non-null dictionary ids in
            // sample to come from the same ProfileDictionary used to create
            // this Profile. Null/default ids represent unknown values.
            unsafe { dictionary_location_from_cxx(location) }
        });
        let labels_iter = sample
            .labels
            .iter()
            .map(|label| -> anyhow::Result<api2::Label<'_>> {
                Ok(api2::Label {
                    // SAFETY: The CXX API contract requires all non-null dictionary
                    // ids in sample to come from the same ProfileDictionary
                    // used to create this Profile. Null/default keys represent
                    // the empty string.
                    key: unsafe { dictionary_string_id_from_cxx(&label.key) },
                    str: label.str,
                    num: label.num,
                    num_unit: label.num_unit,
                })
            });

        // SAFETY: The CXX API contract requires all non-null dictionary ids in sample
        // to come from the same ProfileDictionary used to create this Profile.
        // Null/default ids represent empty or unknown values.
        unsafe {
            self.inner
                .try_add_sample2(locations_iter, sample.values, labels_iter, timestamp)
                .context("Profile::add_dictionary_sample failed")
        }
    }

    pub fn set_custom_sample_type(
        &mut self,
        slot: ffi::SampleType,
        type_: &str,
        unit: &str,
    ) -> bool {
        let result = match slot.try_into() {
            Ok(slot) => self
                .inner
                .set_custom_sample_type(slot, api::ValueType::new(type_, unit)),
            Err(err) => Err(err),
        };
        self.handle_result("Profile::set_custom_sample_type", result)
    }

    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: &str) -> bool {
        let result = self
            .inner
            .add_endpoint(local_root_span_id, std::borrow::Cow::Borrowed(endpoint));
        self.handle_result("Profile::add_endpoint", result)
    }

    pub fn add_endpoint_count(&mut self, endpoint: &str, value: i64) -> bool {
        let result = self
            .inner
            .add_endpoint_count(std::borrow::Cow::Borrowed(endpoint), value);
        self.handle_result("Profile::add_endpoint_count", result)
    }

    pub fn add_upscaling_rule_poisson(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> bool {
        let upscaling_info = api::UpscalingInfo::Poisson {
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        };
        let result =
            self.inner
                .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info);
        self.handle_result("Profile::add_upscaling_rule_poisson", result)
    }

    pub fn add_upscaling_rule_poisson_non_sample_type_count(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value: u64,
        sampling_distance: u64,
    ) -> bool {
        let upscaling_info = api::UpscalingInfo::PoissonNonSampleTypeCount {
            sum_value_offset,
            count_value,
            sampling_distance,
        };
        let result =
            self.inner
                .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info);
        self.handle_result(
            "Profile::add_upscaling_rule_poisson_non_sample_type_count",
            result,
        )
    }

    pub fn add_upscaling_rule_proportional(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        scale: f64,
    ) -> bool {
        let upscaling_info = api::UpscalingInfo::Proportional { scale };
        let result =
            self.inner
                .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info);
        self.handle_result("Profile::add_upscaling_rule_proportional", result)
    }

    pub fn reset(&mut self) -> bool {
        // Reset and discard the old profile
        let result = self.inner.reset_and_return_previous().map(|_| ());
        self.handle_result("Profile::reset", result)
    }

    pub fn serialize(&mut self) -> Box<EncodedProfileResult> {
        let result = (|| -> anyhow::Result<Box<EncodedProfile>> {
            // Reset the profile and get the old one to serialize.
            let old_profile = self.inner.reset_and_return_previous()?;
            let end_time = Some(std::time::SystemTime::now());
            let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;
            Ok(Box::new(EncodedProfile { inner: encoded }))
        })();
        if let Err(err) = &result {
            self.handle_error("Profile::serialize", err);
        }
        EncodedProfileResult::from_result("Profile::serialize", result)
    }

    pub fn serialize_to_vec(&mut self, out: &mut Vec<u8>) -> bool {
        match (|| -> anyhow::Result<Vec<u8>> {
            // Reset the profile and get the old one to serialize.
            let old_profile = self.inner.reset_and_return_previous()?;
            let end_time = Some(std::time::SystemTime::now());
            Ok(old_profile
                .serialize_into_compressed_pprof(end_time, None)?
                .buffer)
        })() {
            Ok(bytes) => {
                *out = bytes;
                true
            }
            Err(err) => {
                out.clear();
                self.handle_error("Profile::serialize_to_vec", err)
            }
        }
    }
}

// ============================================================================
// EncodedProfile - Wrapper around internal::EncodedProfile
// ============================================================================

pub struct EncodedProfile {
    pub(crate) inner: internal::EncodedProfile,
}

impl EncodedProfile {
    pub fn bytes(&self) -> Vec<u8> {
        self.inner.buffer.clone()
    }
}
