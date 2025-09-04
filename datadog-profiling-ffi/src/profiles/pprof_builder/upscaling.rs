// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling::profiles;
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::slice::CharSlice;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UpscalingProportional {
    scale: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct UpscalingPoisson {
    sampling_distance: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct UpscalingPoissonNonSampleTypeCount {
    count_value: u64,
    sampling_distance: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum UpscalingInfo {
    Proportional(UpscalingProportional),
    Poisson(UpscalingPoisson),
    PoissonNonSampleTypeCount(UpscalingPoissonNonSampleTypeCount),
}

impl TryFrom<UpscalingInfo> for profiles::UpscalingInfo {
    type Error = ProfileError;

    fn try_from(info: UpscalingInfo) -> Result<Self, Self::Error> {
        Ok(match info {
            UpscalingInfo::Proportional(UpscalingProportional { scale }) => {
                profiles::UpscalingInfo::Proportional { scale }
            }
            UpscalingInfo::Poisson (UpscalingPoisson { sampling_distance }) => {
                profiles::UpscalingInfo::Poisson {
                    sampling_distance: sampling_distance
                        .try_into()
                        .map_err(|_| ProfileError::other("invalid input: upscaling Poisson sampling distance was zero"))?,
                }
            }
            UpscalingInfo::PoissonNonSampleTypeCount(UpscalingPoissonNonSampleTypeCount{
                count_value,
                sampling_distance,
            }) => profiles::UpscalingInfo::PoissonNonSampleTypeCount {
                count_value: count_value.try_into()
                    .map_err(|_| ProfileError::other("invalid input: upscaling PoissonNonSampleTypeCount count value was zero"))?,
                sampling_distance: sampling_distance.try_into()
                    .map_err(|_| ProfileError::other("invalid input: upscaling PoissonNonSampleTypeCount sampling distance was zero"))?,
            },
        })
    }
}

#[repr(C)]
pub struct UpscalingRule<'a> {
    pub group_by_label_key: CharSlice<'a>, // todo: this one is interned already
    pub group_by_label_value: CharSlice<'a>, // "OutOfBoundsException", "0 - 9ms"
    pub upscaling_info: UpscalingInfo,
}
