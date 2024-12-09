// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::datatypes::*;
use datadog_profiling::api;
use datadog_profiling::internal;
use datadog_profiling::internal::Profile;
use datadog_profiling::internal::ProfiledEndpointsStats;
use ddcommon_ffi::slice::{AsBytes, CharSlice, Slice};
use ddcommon_ffi::{wrap_with_ffi_result, Handle, Timespec};
use ddcommon_ffi::{wrap_with_void_ffi_result, ToInner, VoidResult};
use function_name::named;
use std::num::NonZeroI64;
use std::time::{Duration, SystemTime};

/// Create a new profile with the given sample types. Must call
/// `ddog_prof_Profile_drop` when you are done with the profile.
///
/// # Arguments
/// * `sample_types`
/// * `period` - Optional period of the profile. Passing None/null translates to zero values.
/// * `start_time` - Optional time the profile started at. Passing None/null will use the current
///   time.
///
/// # Safety
/// All slices must be have pointers that are suitably aligned for their type
/// and must have the correct number of elements for the slice.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_new(
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
    start_time: Option<&Timespec>,
) -> ddcommon_ffi::Result<Handle<Profile>> {
    wrap_with_ffi_result!({
        let types: Vec<api::ValueType> = sample_types.into_slice().iter().map(Into::into).collect();
        let start_time = start_time.map_or_else(SystemTime::now, SystemTime::from);
        let period = period.map(Into::into);

        anyhow::Ok(Profile::new(start_time, &types, period).into())
    })
}

/// # Safety
/// The `profile` can be null, but if non-null it must point to a Profile
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_drop(profile: *mut Handle<Profile>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !profile.is_null() {
        drop((*profile).take())
    }
}

/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module. All pointers inside the `sample` need to be valid for the duration
/// of this call.
///
/// If successful, it returns the Ok variant.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_add(
    mut profile: *mut Handle<Profile>,
    sample: Sample,
    timestamp: Option<NonZeroI64>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile
            .to_inner_mut()?
            .add_sample(sample.try_into()?, timestamp)?
    })
}

/// Associate an endpoint to a given local root span id.
/// During the serialization of the profile, an endpoint label will be added
/// to all samples that contain a matching local root span id label.
///
/// Note: calling this API causes the "trace endpoint" and "local root span id" strings
/// to be interned, even if no matching sample is found.
///
/// # Arguments
/// * `profile` - a reference to the profile that will contain the samples.
/// * `local_root_span_id`
/// * `endpoint` - the value of the endpoint label to add for matching samples.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is _NOT_ thread-safe.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_set_endpoint(
    mut profile: *mut Handle<Profile>,
    local_root_span_id: u64,
    endpoint: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile
            .to_inner_mut()?
            .add_endpoint(local_root_span_id, endpoint.to_utf8_lossy())?
    })
}

/// Count the number of times an endpoint has been seen.
///
/// # Arguments
/// * `profile` - a reference to the profile that will contain the samples.
/// * `endpoint` - the endpoint label for which the count will be incremented
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is _NOT_ thread-safe.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_add_endpoint_count(
    mut profile: *mut Handle<Profile>,
    endpoint: CharSlice,
    value: i64,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile
            .to_inner_mut()?
            .add_endpoint_count(endpoint.to_utf8_lossy(), value)?
    })
}

/// Add a poisson-based upscaling rule which will be use to adjust values and make them
/// closer to reality.
///
/// # Arguments
/// * `profile` - a reference to the profile that will contain the samples.
/// * `offset_values` - offset of the values
/// * `label_name` - name of the label used to identify sample(s)
/// * `label_value` - value of the label used to identify sample(s)
/// * `sum_value_offset` - offset of the value used as a sum (compute the average with
///   `count_value_offset`)
/// * `count_value_offset` - offset of the value used as a count (compute the average with
///   `sum_value_offset`)
/// * `sampling_distance` - this is the threshold for this sampling window. This value must not be
///   equal to 0
///
/// # Safety
/// This function must be called before serialize and must not be called after.
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_add_upscaling_rule_poisson(
    mut profile: *mut Handle<Profile>,
    offset_values: Slice<usize>,
    label_name: CharSlice,
    label_value: CharSlice,
    sum_value_offset: usize,
    count_value_offset: usize,
    sampling_distance: u64,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        anyhow::ensure!(sampling_distance != 0, "sampling_distance must not be 0");
        profile.to_inner_mut()?.add_upscaling_rule(
            offset_values.as_slice(),
            &label_name.try_to_string()?,
            &label_value.try_to_string()?,
            api::UpscalingInfo::Poisson {
                sum_value_offset,
                count_value_offset,
                sampling_distance,
            },
        )?
    })
}

/// Add a proportional-based upscaling rule which will be use to adjust values and make them
/// closer to reality.
///
/// # Arguments
/// * `profile` - a reference to the profile that will contain the samples.
/// * `offset_values` - offset of the values
/// * `label_name` - name of the label used to identify sample(s)
/// * `label_value` - value of the label used to identify sample(s)
/// * `total_sampled` - number of sampled event (found in the pprof). This value must not be equal
///   to 0
/// * `total_real` - number of events the profiler actually witnessed. This value must not be equal
///   to 0
///
/// # Safety
/// This function must be called before serialize and must not be called after.
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_add_upscaling_rule_proportional(
    mut profile: *mut Handle<Profile>,
    offset_values: Slice<usize>,
    label_name: CharSlice,
    label_value: CharSlice,
    total_sampled: u64,
    total_real: u64,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        anyhow::ensure!(total_sampled != 0, "total_sampled must not be 0");
        anyhow::ensure!(total_real != 0, "total_real must not be 0");
        profile.to_inner_mut()?.add_upscaling_rule(
            offset_values.as_slice(),
            &label_name.try_to_string()?,
            &label_value.try_to_string()?,
            api::UpscalingInfo::Proportional {
                scale: total_real as f64 / total_sampled as f64,
            },
        )?
    })
}

#[repr(C)]
pub struct EncodedProfile {
    start: Timespec,
    end: Timespec,
    buffer: ddcommon_ffi::Vec<u8>,
    endpoints_stats: Box<ProfiledEndpointsStats>,
}

/// # Safety
/// Only pass a reference to a valid `ddog_prof_EncodedProfile`, or null. A
/// valid reference also means that it hasn't already been dropped (do not
/// call this twice on the same object).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_EncodedProfile_drop(profile: Option<&mut EncodedProfile>) {
    if let Some(reference) = profile {
        // Safety: EncodedProfile's are repr(C), and not box allocated. If the
        // user has followed the safety requirements of this function, then
        // this is safe.
        std::ptr::drop_in_place(reference as *mut _)
    }
}

impl From<internal::EncodedProfile> for EncodedProfile {
    fn from(value: internal::EncodedProfile) -> Self {
        let start = value.start.into();
        let end = value.end.into();
        let buffer = value.buffer.into();
        let endpoints_stats = Box::new(value.endpoints_stats);

        Self {
            start,
            end,
            buffer,
            endpoints_stats,
        }
    }
}

/// Serialize the aggregated profile.
/// Drains the data, and then resets the profile for future use.
///
/// Don't forget to clean up the ok with `ddog_prof_EncodedProfile_drop` or
/// the error variant with `ddog_Error_drop` when you are done with them.
///
/// # Arguments
/// * `profile` - a reference to the profile being serialized.
/// * `end_time` - optional end time of the profile. If None/null is passed, the current time will
///   be used.
/// * `duration_nanos` - Optional duration of the profile. Passing None or a negative duration will
///   mean the duration will based on the end time minus the start time, but under anomalous
///   conditions this may fail as system clocks can be adjusted, or the programmer accidentally
///   passed an earlier time. The duration of the serialized profile will be set to zero for these
///   cases.
/// * `start_time` - Optional start time for the next profile.
///
/// # Safety
/// The `profile` must point to a valid profile object.
/// The `end_time` must be null or otherwise point to a valid TimeSpec object.
/// The `duration_nanos` must be null or otherwise point to a valid i64.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_serialize(
    mut profile: *mut Handle<Profile>,
    end_time: Option<&Timespec>,
    duration_nanos: Option<&i64>,
    start_time: Option<&Timespec>,
) -> ddcommon_ffi::Result<EncodedProfile> {
    wrap_with_ffi_result!({
        let old_profile = profile
            .to_inner_mut()?
            .reset_and_return_previous(start_time.map(SystemTime::from))?;
        let duration = match duration_nanos {
            None => None,
            Some(x) if *x < 0 => None,
            Some(x) => Some(Duration::from_nanos((*x) as u64)),
        };
        anyhow::Ok(
            old_profile
                .serialize_into_compressed_pprof(end_time.map(SystemTime::from), duration)?
                .into(),
        )
    })
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_Vec_U8_as_slice(vec: &ddcommon_ffi::Vec<u8>) -> Slice<u8> {
    vec.as_slice()
}

/// Resets all data in `profile` except the sample types and period. Returns
/// true if it successfully reset the profile and false otherwise. The profile
/// remains valid if false is returned.
///
/// # Arguments
/// * `profile` - A mutable reference to the profile to be reset.
/// * `start_time` - The time of the profile (after reset). Pass None/null to use the current time.
///
/// # Safety
/// The `profile` must meet all the requirements of a mutable reference to the profile. Given this
/// can be called across an FFI boundary, the compiler cannot enforce this.
/// If `time` is not null, it must point to a valid Timespec object.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_reset(
    mut profile: *mut Handle<Profile>,
    start_time: Option<&Timespec>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile
            .to_inner_mut()?
            .reset_and_return_previous(start_time.map(SystemTime::from))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctor_and_dtor() -> anyhow::Result<()> {
        unsafe {
            let sample_type: *const ValueType = &ValueType::new("samples", "count");
            let mut profile = anyhow::Result::from(ddog_prof_Profile_new(
                Slice::from_raw_parts(sample_type, 1),
                None,
                None,
            ))?;
            ddog_prof_Profile_drop(&mut profile);
            Ok(())
        }
    }

    #[test]
    fn add_failure() -> anyhow::Result<()> {
        unsafe {
            let sample_type: *const ValueType = &ValueType::new("samples", "count");
            let mut profile = anyhow::Result::from(ddog_prof_Profile_new(
                Slice::from_raw_parts(sample_type, 1),
                None,
                None,
            ))?;

            // wrong number of values (doesn't match sample types)
            let values: &[i64] = &[];

            let sample = Sample {
                locations: Slice::empty(),
                values: Slice::from(values),
                labels: Slice::empty(),
            };

            let result = anyhow::Result::from(ddog_prof_Profile_add(&mut profile, sample, None));
            result.unwrap_err();
            ddog_prof_Profile_drop(&mut profile);
            Ok(())
        }
    }

    #[test]
    // TODO FIX
    #[cfg_attr(miri, ignore)]

    fn aggregate_samples() -> anyhow::Result<()> {
        unsafe {
            let sample_type: *const ValueType = &ValueType::new("samples", "count");
            let mut profile = anyhow::Result::from(ddog_prof_Profile_new(
                Slice::from_raw_parts(sample_type, 1),
                None,
                None,
            ))?;

            let mapping = Mapping {
                filename: "php".into(),
                ..Default::default()
            };

            let locations = vec![Location {
                mapping,
                function: Function {
                    name: "{main}".into(),
                    system_name: "{main}".into(),
                    filename: "index.php".into(),
                    start_line: 0,
                },
                ..Default::default()
            }];
            let values: Vec<i64> = vec![1];
            let labels = vec![Label {
                key: Slice::from("pid"),
                num: 101,
                ..Default::default()
            }];

            let sample = Sample {
                locations: Slice::from(&locations),
                values: Slice::from(&values),
                labels: Slice::from(&labels),
            };

            anyhow::Result::from(ddog_prof_Profile_add(&mut profile, sample, None))?;
            assert_eq!(
                profile
                    .to_inner()?
                    .only_for_testing_num_aggregated_samples(),
                1
            );

            anyhow::Result::from(ddog_prof_Profile_add(&mut profile, sample, None))?;
            assert_eq!(
                profile
                    .to_inner()?
                    .only_for_testing_num_aggregated_samples(),
                1
            );

            ddog_prof_Profile_drop(&mut profile);
            Ok(())
        }
    }

    unsafe fn provide_distinct_locations_ffi() -> anyhow::Result<Handle<Profile>> {
        let sample_type: *const ValueType = &ValueType::new("samples", "count");
        let mut profile = Result::from(ddog_prof_Profile_new(
            Slice::from_raw_parts(sample_type, 1),
            None,
            None,
        ))
        .unwrap();

        let mapping = Mapping {
            filename: "php".into(),
            ..Default::default()
        };

        let main_locations = vec![Location {
            mapping,
            function: Function {
                name: "{main}".into(),
                system_name: "{main}".into(),
                filename: "index.php".into(),
                start_line: 0,
            },
            ..Default::default()
        }];
        let test_locations = vec![Location {
            mapping,
            function: Function {
                name: "test".into(),
                system_name: "test".into(),
                filename: "index.php".into(),
                start_line: 3,
            },
            line: 4,
            ..Default::default()
        }];
        let values: Vec<i64> = vec![1];
        let labels = vec![Label {
            key: Slice::from("pid"),
            str: Slice::from(""),
            num: 101,
            num_unit: Slice::from(""),
        }];

        let main_sample = Sample {
            locations: Slice::from(main_locations.as_slice()),
            values: Slice::from(values.as_slice()),
            labels: Slice::from(labels.as_slice()),
        };

        let test_sample = Sample {
            locations: Slice::from(test_locations.as_slice()),
            values: Slice::from(values.as_slice()),
            labels: Slice::from(labels.as_slice()),
        };

        anyhow::Result::from(ddog_prof_Profile_add(&mut profile, main_sample, None)).unwrap();
        assert_eq!(
            profile
                .to_inner()?
                .only_for_testing_num_aggregated_samples(),
            1
        );

        anyhow::Result::from(ddog_prof_Profile_add(&mut profile, test_sample, None)).unwrap();
        assert_eq!(
            profile
                .to_inner()?
                .only_for_testing_num_aggregated_samples(),
            2
        );

        Ok(profile)
    }

    #[test]
    fn distinct_locations_ffi() {
        unsafe {
            ddog_prof_Profile_drop(&mut provide_distinct_locations_ffi().unwrap());
        }
    }
}
