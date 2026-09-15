// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::dictionary::ProfileDictionary;
use super::errors::{EncodedProfileResult, ErrorStore, Operation, ProfileResult};
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

fn sample_from_cxx<'a>(sample: &ffi::Sample<'a>) -> api::Sample<'a> {
    api::Sample {
        locations: sample.locations.iter().map(Into::into).collect(),
        values: sample.values,
        labels: sample.labels.iter().map(Into::into).collect(),
    }
}

impl Profile {
    fn new(inner: internal::Profile) -> Self {
        Self {
            inner,
            errors: ErrorStore::new(),
        }
    }

    fn handle_result(&mut self, operation: Operation, result: anyhow::Result<()>) -> bool {
        self.errors.handle_result(operation, result)
    }

    pub fn set_error_policy(&mut self, policy: ffi::ErrorPolicy) {
        self.errors.set_policy(policy);
    }

    pub fn take_errors(&mut self) -> Vec<ffi::Error> {
        self.errors.take_errors()
    }

    pub fn create(sample_types: Vec<ffi::SampleType>, period: &ffi::Period) -> Box<ProfileResult> {
        ProfileResult::from_result(
            Operation::CreateProfile,
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

    pub fn create_with_dictionary(
        sample_types: Vec<ffi::SampleType>,
        period: &ffi::Period,
        dictionary: &ProfileDictionary,
    ) -> Box<ProfileResult> {
        ProfileResult::from_result(
            Operation::CreateProfileWithDictionary,
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
                )?;
                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn add_sample(&mut self, sample: &ffi::Sample) -> bool {
        // Profile interns the strings.
        let result = self.inner.try_add_sample(sample_from_cxx(sample), None);
        self.handle_result(Operation::AddSample, result)
    }

    pub fn add_sample_with_timestamp(&mut self, sample: &ffi::Sample, endtime_ns: i64) -> bool {
        let result = (|| {
            let timestamp =
                internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero")?;
            self.inner
                .try_add_sample(sample_from_cxx(sample), Some(timestamp))
        })();
        self.handle_result(Operation::AddSampleWithTimestamp, result)
    }

    /// Adds a dictionary-backed sample without an end timestamp.
    ///
    /// # Safety
    /// All non-null ids in sample must have been produced by the
    /// ProfileDictionary used to create this Profile.
    pub unsafe fn add_dictionary_sample(&mut self, sample: &ffi::DictionarySample) -> bool {
        // SAFETY: The caller guarantees all non-null ids in sample came from
        // the ProfileDictionary used to create this Profile.
        unsafe { self.add_dictionary_sample_impl(sample, None) }
    }

    /// Adds a dictionary-backed sample with an end timestamp in nanoseconds.
    ///
    /// # Safety
    /// All non-null ids in sample must have been produced by the
    /// ProfileDictionary used to create this Profile.
    pub unsafe fn add_dictionary_sample_with_timestamp(
        &mut self,
        sample: &ffi::DictionarySample,
        endtime_ns: i64,
    ) -> bool {
        let result = (|| {
            let timestamp =
                internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero")?;
            // SAFETY: The caller guarantees all non-null ids in sample came
            // from the ProfileDictionary used to create this Profile.
            unsafe { self.add_dictionary_sample_result(sample, Some(timestamp)) }
        })();
        self.handle_result(Operation::AddDictionarySample, result)
    }

    unsafe fn add_dictionary_sample_impl(
        &mut self,
        sample: &ffi::DictionarySample,
        timestamp: Option<internal::Timestamp>,
    ) -> bool {
        // SAFETY: The caller guarantees all non-null ids in sample came from
        // the ProfileDictionary used to create this Profile.
        let result = unsafe { self.add_dictionary_sample_result(sample, timestamp) };
        self.handle_result(Operation::AddDictionarySample, result)
    }

    unsafe fn add_dictionary_sample_result(
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
        }
    }

    pub fn set_custom_sample_type(
        &mut self,
        slot: ffi::SampleType,
        type_: &str,
        unit: &str,
    ) -> bool {
        let result = (|| {
            let slot = slot.try_into()?;
            self.inner
                .set_custom_sample_type(slot, api::ValueType::new(type_, unit))
        })();
        self.handle_result(Operation::SetCustomSampleType, result)
    }

    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: &str) -> bool {
        let result = self
            .inner
            .add_endpoint(local_root_span_id, std::borrow::Cow::Borrowed(endpoint));
        self.handle_result(Operation::AddEndpoint, result)
    }

    pub fn add_endpoint_count(&mut self, endpoint: &str, value: i64) -> bool {
        let result = self
            .inner
            .add_endpoint_count(std::borrow::Cow::Borrowed(endpoint), value);
        self.handle_result(Operation::AddEndpointCount, result)
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
        self.handle_result(Operation::AddUpscalingRulePoisson, result)
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
        self.handle_result(Operation::AddUpscalingRulePoissonNonSampleTypeCount, result)
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
        self.handle_result(Operation::AddUpscalingRuleProportional, result)
    }

    pub fn serialize(&mut self) -> Box<EncodedProfileResult> {
        let result = (|| -> anyhow::Result<Box<EncodedProfile>> {
            // Reset the profile and get the old one to serialize.
            let old_profile = self.inner.reset_and_return_previous()?;
            let end_time = Some(std::time::SystemTime::now());
            let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;
            Ok(Box::new(EncodedProfile { inner: encoded }))
        })();
        EncodedProfileResult::from_result(Operation::SerializeProfile, result)
    }
}

// ============================================================================
// EncodedProfile - Wrapper around internal::EncodedProfile
// ============================================================================

pub struct EncodedProfile {
    pub(crate) inner: internal::EncodedProfile,
}

impl EncodedProfile {
    pub fn bytes(&self) -> &[u8] {
        &self.inner.buffer
    }
}
