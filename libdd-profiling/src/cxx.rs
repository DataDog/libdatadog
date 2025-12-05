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

    // Enums
    #[derive(Debug)]
    #[repr(u32)]
    enum SampleType {
        Cpu = 0,
        Wall = 1,
        Exception = 2,
        LockAcquire = 3,
        LockRelease = 4,
        Allocation = 5,
        Heap = 6,
        GpuTime = 7,
        GpuMemory = 8,
        GpuFlops = 9,
    }

    // Opaque Rust types
    extern "Rust" {
        type Profile;
        type OwnedSample;
        type SamplePool;

        // Profile static factory
        #[Self = "Profile"]
        fn create(sample_types: Vec<ValueType>, period: &Period) -> Result<Box<Profile>>;

        // Profile methods
        fn add_sample(self: &mut Profile, sample: &Sample) -> Result<()>;
        fn add_owned_sample(self: &mut Profile, sample: &OwnedSample) -> Result<()>;
        fn add_endpoint(self: &mut Profile, local_root_span_id: u64, endpoint: &str) -> Result<()>;
        fn add_endpoint_count(self: &mut Profile, endpoint: &str, value: i64) -> Result<()>;

        // Upscaling rule methods
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

        // OwnedSample methods
        #[Self = "OwnedSample"]
        fn create(sample_types: Vec<SampleType>) -> Result<Box<OwnedSample>>;
        
        fn set_value(self: &mut OwnedSample, sample_type: SampleType, value: i64) -> Result<()>;
        fn get_value(self: &OwnedSample, sample_type: SampleType) -> Result<i64>;
        
        #[Self = "OwnedSample"]
        fn is_timeline_enabled() -> bool;
        #[Self = "OwnedSample"]
        fn set_timeline_enabled(enabled: bool);
        
        fn set_endtime_ns(self: &mut OwnedSample, endtime_ns: i64) -> i64;
        fn set_endtime_ns_now(self: &mut OwnedSample) -> Result<i64>;
        fn endtime_ns(self: &OwnedSample) -> i64;
        
        #[cfg(unix)]
        fn set_endtime_from_monotonic_ns(self: &mut OwnedSample, monotonic_ns: i64) -> Result<i64>;
        
        fn add_location(self: &mut OwnedSample, location: &Location);
        fn add_label(self: &mut OwnedSample, label: &Label);
        fn num_locations(self: &OwnedSample) -> usize;
        fn num_labels(self: &OwnedSample) -> usize;
        fn reset_sample(self: &mut OwnedSample);

        // SamplePool methods
        #[Self = "SamplePool"]
        fn create(sample_types: Vec<SampleType>, capacity: usize) -> Result<Box<SamplePool>>;
        
        fn get_sample(self: &mut SamplePool) -> Box<OwnedSample>;
        fn return_sample(self: &mut SamplePool, sample: Box<OwnedSample>);
        fn pool_len(self: &SamplePool) -> usize;
        fn pool_capacity(self: &SamplePool) -> usize;
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

    pub fn add_owned_sample(&mut self, sample: &OwnedSample) -> anyhow::Result<()> {
        // Convert OwnedSample to API Sample
        let api_sample = sample.inner.as_sample();
        
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

// ============================================================================
// OwnedSample - Wrapper around owned_sample::OwnedSample
// ============================================================================

use crate::owned_sample;
use std::sync::Arc;

pub struct OwnedSample {
    inner: owned_sample::OwnedSample,
}

impl OwnedSample {
    pub fn create(sample_types: Vec<ffi::SampleType>) -> anyhow::Result<Box<OwnedSample>> {
        // Convert CXX SampleType to owned_sample::SampleType
        let types: Vec<owned_sample::SampleType> = sample_types
            .into_iter()
            .map(ffi_sample_type_to_owned)
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Create indices internally
        let indices = Arc::new(owned_sample::SampleTypeIndices::new(types)?);
        let inner = owned_sample::OwnedSample::new(indices);
        Ok(Box::new(OwnedSample { inner }))
    }

    pub fn set_value(&mut self, sample_type: ffi::SampleType, value: i64) -> anyhow::Result<()> {
        let st = ffi_sample_type_to_owned(sample_type)?;
        self.inner.set_value(st, value)
    }

    pub fn get_value(&self, sample_type: ffi::SampleType) -> anyhow::Result<i64> {
        let st = ffi_sample_type_to_owned(sample_type)?;
        self.inner.get_value(st)
    }

    pub fn is_timeline_enabled() -> bool {
        owned_sample::OwnedSample::is_timeline_enabled()
    }

    pub fn set_timeline_enabled(enabled: bool) {
        owned_sample::OwnedSample::set_timeline_enabled(enabled);
    }

    pub fn set_endtime_ns(&mut self, endtime_ns: i64) -> i64 {
        self.inner.set_endtime_ns(endtime_ns)
    }

    pub fn set_endtime_ns_now(&mut self) -> anyhow::Result<i64> {
        self.inner.set_endtime_ns_now()
    }

    pub fn endtime_ns(&self) -> i64 {
        self.inner.endtime_ns()
            .map(|nz| nz.get())
            .unwrap_or(0)
    }

    #[cfg(unix)]
    pub fn set_endtime_from_monotonic_ns(&mut self, monotonic_ns: i64) -> anyhow::Result<i64> {
        self.inner.set_endtime_from_monotonic_ns(monotonic_ns)
    }

    pub fn add_location(&mut self, location: &ffi::Location) {
        let api_location: api::Location = location.into();
        self.inner.add_location(api_location);
    }

    pub fn add_label(&mut self, label: &ffi::Label) {
        let api_label: api::Label = label.into();
        self.inner.add_label(api_label);
    }

    pub fn num_locations(&self) -> usize {
        self.inner.num_locations()
    }

    pub fn num_labels(&self) -> usize {
        self.inner.num_labels()
    }

    pub fn reset_sample(&mut self) {
        self.inner.reset();
    }
}

// ============================================================================
// SamplePool - Wrapper around owned_sample::SamplePool
// ============================================================================

pub struct SamplePool {
    inner: owned_sample::SamplePool,
}

impl SamplePool {
    pub fn create(sample_types: Vec<ffi::SampleType>, capacity: usize) -> anyhow::Result<Box<SamplePool>> {
        // Convert CXX SampleType to owned_sample::SampleType
        let types: Vec<owned_sample::SampleType> = sample_types
            .into_iter()
            .map(ffi_sample_type_to_owned)
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Create indices internally
        let indices = Arc::new(owned_sample::SampleTypeIndices::new(types)?);
        let inner = owned_sample::SamplePool::new(indices, capacity);
        Ok(Box::new(SamplePool { inner }))
    }

    pub fn get_sample(&mut self) -> Box<OwnedSample> {
        let inner = self.inner.get();
        Box::new(OwnedSample { inner: *inner })
    }

    #[allow(clippy::boxed_local)]
    pub fn return_sample(&mut self, sample: Box<OwnedSample>) {
        self.inner.put(Box::new(sample.inner));
    }

    pub fn pool_len(&self) -> usize {
        self.inner.len()
    }

    pub fn pool_capacity(&self) -> usize {
        self.inner.capacity()
    }
}

// Note: We must redeclare SampleType in the CXX bridge because CXX doesn't support
// using external Rust enums. This conversion function maps between the two.
fn ffi_sample_type_to_owned(st: ffi::SampleType) -> anyhow::Result<owned_sample::SampleType> {
    match st {
        ffi::SampleType::Cpu => Ok(owned_sample::SampleType::Cpu),
        ffi::SampleType::Wall => Ok(owned_sample::SampleType::Wall),
        ffi::SampleType::Exception => Ok(owned_sample::SampleType::Exception),
        ffi::SampleType::LockAcquire => Ok(owned_sample::SampleType::LockAcquire),
        ffi::SampleType::LockRelease => Ok(owned_sample::SampleType::LockRelease),
        ffi::SampleType::Allocation => Ok(owned_sample::SampleType::Allocation),
        ffi::SampleType::Heap => Ok(owned_sample::SampleType::Heap),
        ffi::SampleType::GpuTime => Ok(owned_sample::SampleType::GpuTime),
        ffi::SampleType::GpuMemory => Ok(owned_sample::SampleType::GpuMemory),
        ffi::SampleType::GpuFlops => Ok(owned_sample::SampleType::GpuFlops),
        _ => anyhow::bail!("Unknown SampleType variant: {:?}", st),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_type_enum_sync() {
        // Ensure ffi::SampleType and owned_sample::SampleType stay in sync
        // This will fail to compile if variants don't match
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::Cpu).unwrap() as usize, owned_sample::SampleType::Cpu as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::Wall).unwrap() as usize, owned_sample::SampleType::Wall as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::Exception).unwrap() as usize, owned_sample::SampleType::Exception as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::LockAcquire).unwrap() as usize, owned_sample::SampleType::LockAcquire as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::LockRelease).unwrap() as usize, owned_sample::SampleType::LockRelease as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::Allocation).unwrap() as usize, owned_sample::SampleType::Allocation as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::Heap).unwrap() as usize, owned_sample::SampleType::Heap as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::GpuTime).unwrap() as usize, owned_sample::SampleType::GpuTime as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::GpuMemory).unwrap() as usize, owned_sample::SampleType::GpuMemory as usize);
        assert_eq!(ffi_sample_type_to_owned(ffi::SampleType::GpuFlops).unwrap() as usize, owned_sample::SampleType::GpuFlops as usize);
    }
}
