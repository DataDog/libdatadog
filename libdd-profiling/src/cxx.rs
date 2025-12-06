// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CXX bindings for profiling module - provides a safe and idiomatic C++ API

#![allow(clippy::needless_lifetimes)]

use crate::api;
use crate::internal;

// ============================================================================
// CXX Bridge - C++ Bindings
// ============================================================================

#[cxx::bridge(namespace = "datadog::profiling")]
pub mod ffi {
    // Shared structs - CXX-friendly types
    struct ValueType<'a> {
        type_: &'a str,
        unit: &'a str,
    }

    struct Period<'a> {
        value_type: ValueType<'a>,
        value: i64,
    }

    struct Mapping<'a> {
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: &'a str,
        build_id: &'a str,
    }

    struct Function<'a> {
        name: &'a str,
        system_name: &'a str,
        filename: &'a str,
    }

    struct Location<'a> {
        mapping: Mapping<'a>,
        function: Function<'a>,
        address: u64,
        line: i64,
    }

    struct Label<'a> {
        key: &'a str,
        str: &'a str,
        num: i64,
        num_unit: &'a str,
    }

    struct Sample<'a> {
        locations: Vec<Location<'a>>,
        values: Vec<i64>,
        labels: Vec<Label<'a>>,
    }

    // Opaque Rust types
    extern "Rust" {
        type Profile;

        // Static factory methods
        #[Self = "Profile"]
        fn create(sample_types: Vec<ValueType>, period: &Period) -> Result<Box<Profile>>;

        // Profile methods
        fn add_sample(self: &mut Profile, sample: &Sample) -> Result<()>;
        fn add_endpoint(self: &mut Profile, local_root_span_id: u64, endpoint: &str) -> Result<()>;
        fn add_endpoint_count(self: &mut Profile, endpoint: &str, value: i64) -> Result<()>;

        // Upscaling rule methods (one for each variant)
        fn add_upscaling_rule_poisson(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value_offset: usize,
            sampling_distance: u64,
        ) -> Result<()>;

        fn add_upscaling_rule_poisson_non_sample_type_count(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value: u64,
            sampling_distance: u64,
        ) -> Result<()>;

        fn add_upscaling_rule_proportional(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            scale: f64,
        ) -> Result<()>;

        fn reset(self: &mut Profile) -> Result<()>;
        fn serialize_to_vec(self: &mut Profile) -> Result<Vec<u8>>;
    }
}

// ============================================================================
// From Implementations - Convert CXX types to API types
// ============================================================================

impl<'a> From<&ffi::ValueType<'a>> for api::ValueType<'a> {
    fn from(vt: &ffi::ValueType<'a>) -> Self {
        api::ValueType::new(vt.type_, vt.unit)
    }
}

impl<'a> From<&ffi::Period<'a>> for api::Period<'a> {
    fn from(period: &ffi::Period<'a>) -> Self {
        api::Period {
            r#type: (&period.value_type).into(),
            value: period.value,
        }
    }
}

impl<'a> From<&ffi::Mapping<'a>> for api::Mapping<'a> {
    fn from(mapping: &ffi::Mapping<'a>) -> Self {
        api::Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename,
            build_id: mapping.build_id,
        }
    }
}

impl<'a> From<&ffi::Function<'a>> for api::Function<'a> {
    fn from(func: &ffi::Function<'a>) -> Self {
        api::Function {
            name: func.name,
            system_name: func.system_name,
            filename: func.filename,
        }
    }
}

impl<'a> From<&ffi::Location<'a>> for api::Location<'a> {
    fn from(loc: &ffi::Location<'a>) -> Self {
        api::Location {
            mapping: (&loc.mapping).into(),
            function: (&loc.function).into(),
            address: loc.address,
            line: loc.line,
        }
    }
}

impl<'a> From<&ffi::Label<'a>> for api::Label<'a> {
    fn from(label: &ffi::Label<'a>) -> Self {
        api::Label {
            key: label.key,
            str: label.str,
            num: label.num,
            num_unit: label.num_unit,
        }
    }
}

// ============================================================================
// Profile - Wrapper around internal::Profile
// ============================================================================

pub struct Profile {
    inner: internal::Profile,
}

impl Profile {
    pub fn create(
        sample_types: Vec<ffi::ValueType>,
        period: &ffi::Period,
    ) -> anyhow::Result<Box<Profile>> {
        // Convert using From trait
        let types: Vec<api::ValueType> = sample_types.iter().map(Into::into).collect();
        let period_value: api::Period = period.into();

        // Profile::try_new interns the strings
        let inner = internal::Profile::try_new(&types, Some(period_value))?;

        Ok(Box::new(Profile { inner }))
    }

    pub fn add_sample(&mut self, sample: &ffi::Sample) -> anyhow::Result<()> {
        let api_sample = api::Sample {
            locations: sample.locations.iter().map(Into::into).collect(),
            values: &sample.values,
            labels: sample.labels.iter().map(Into::into).collect(),
        };

        // Profile interns the strings
        self.inner.try_add_sample(api_sample, None)?;
        Ok(())
    }

    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: &str) -> anyhow::Result<()> {
        self.inner
            .add_endpoint(local_root_span_id, std::borrow::Cow::Borrowed(endpoint))
    }

    pub fn add_endpoint_count(&mut self, endpoint: &str, value: i64) -> anyhow::Result<()> {
        self.inner
            .add_endpoint_count(std::borrow::Cow::Borrowed(endpoint), value)
    }

    pub fn add_upscaling_rule_poisson(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::Poisson {
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn add_upscaling_rule_poisson_non_sample_type_count(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value: u64,
        sampling_distance: u64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::PoissonNonSampleTypeCount {
            sum_value_offset,
            count_value,
            sampling_distance,
        };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn add_upscaling_rule_proportional(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        scale: f64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::Proportional { scale };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn reset(&mut self) -> anyhow::Result<()> {
        // Reset and discard the old profile
        self.inner.reset_and_return_previous()?;
        Ok(())
    }

    pub fn serialize_to_vec(&mut self) -> anyhow::Result<Vec<u8>> {
        // Reset the profile and get the old one to serialize
        let old_profile = self.inner.reset_and_return_previous()?;
        let end_time = Some(std::time::SystemTime::now());
        let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;
        Ok(encoded.buffer)
    }
}
