// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling::profiles::collections::StringId;
use datadog_profiling::profiles::datatypes::MAX_SAMPLE_TYPES;
use ddcommon::error::FfiSafeErrorMessage;
use ddcommon_ffi::slice::CharSlice;
use std::ffi::CStr;
use std::num::NonZeroU64;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GroupByLabel<'a> {
    pub key: StringId,
    pub value: CharSlice<'a>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ProportionalUpscalingRule<'a> {
    /// The labels to group the sample values by. If it should apply to all
    /// samples and not group by label, then use the empty StringId and empty
    /// CharSlice.
    pub group_by_label: GroupByLabel<'a>,
    pub sampled: u64,
    pub real: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PoissonUpscalingRule {
    /// Which offset in the profile's sample is the sum. Must be disjoint from
    /// `count_offset`.
    pub sum_offset: u32,
    /// Which offset in the profile's sample is the count. Must be disjoint
    /// from `sum_offset`.
    pub count_offset: u32,
    pub sampling_distance: u64,
}

#[derive(Debug)]
pub enum PoissonUpscalingConversionError {
    SamplingDistance,
    SumOffset,
    CountOffset,
}

// SAFETY: all cases use Rust c-str literals.
unsafe impl FfiSafeErrorMessage for PoissonUpscalingConversionError {
    fn as_ffi_str(&self) -> &'static CStr {
        match self {
            PoissonUpscalingConversionError::SamplingDistance => c"PoissonUpscalingRule.sampling_distance cannot be zero",
            PoissonUpscalingConversionError::SumOffset => c"PoissonUpscalingRule.sum_offset must be less than MAX_SAMPLE_TYPES",
            PoissonUpscalingConversionError::CountOffset => c"PoissonUpscalingRule.count_offset must be less than MAX_SAMPLE_TYPES",
        }
    }
}

impl core::fmt::Display for PoissonUpscalingConversionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.as_rust_str().fmt(f)
    }
}

impl core::error::Error for PoissonUpscalingConversionError {}

impl TryFrom<PoissonUpscalingRule>
    for datadog_profiling::profiles::PoissonUpscalingRule
{
    type Error = PoissonUpscalingConversionError;

    fn try_from(value: PoissonUpscalingRule) -> Result<Self, Self::Error> {
        let Some(sampling_distance) = NonZeroU64::new(value.sampling_distance)
        else {
            return Err(PoissonUpscalingConversionError::SamplingDistance);
        };
        let sum_offset = value.count_offset as usize;
        let count_offset = value.count_offset as usize;
        if sum_offset >= MAX_SAMPLE_TYPES {
            return Err(PoissonUpscalingConversionError::SumOffset);
        }
        if count_offset >= MAX_SAMPLE_TYPES {
            return Err(PoissonUpscalingConversionError::CountOffset);
        }
        Ok(Self { sum_offset, count_offset, sampling_distance })
    }
}
