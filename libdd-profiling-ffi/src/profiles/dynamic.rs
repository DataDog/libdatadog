// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::datatypes::{
    ddog_prof_EncodedProfile_drop, Period, ProfileResult, SampleType, SerializeResult,
};
use crate::arc_handle::ArcHandle;
use anyhow::Context;
use function_name::named;
use libdd_common_ffi::slice::{AsBytes, CharSlice, Slice};
use libdd_common_ffi::{wrap_with_ffi_result, Error, Result, Timespec};
use libdd_profiling::dynamic::{
    DynamicFunction as InnerDynamicFunction, DynamicFunctionIndex as InnerDynamicFunctionIndex,
    DynamicLabel as InnerDynamicLabel, DynamicLocation as InnerDynamicLocation,
    DynamicProfile as InnerDynamicProfile,
    DynamicProfilesDictionary as InnerDynamicProfilesDictionary,
    DynamicSample as InnerDynamicSample, DynamicStackTraceIndex as InnerDynamicStackTraceIndex,
    DynamicStringIndex as InnerDynamicStringIndex,
};
use std::str;
use std::time::Duration;

/// Represents a dynamic profile. Do not access its member directly.
#[repr(C)]
pub struct DynamicProfile {
    inner: *mut InnerDynamicProfile,
}

impl DynamicProfile {
    fn new(profile: InnerDynamicProfile) -> Self {
        Self {
            inner: Box::into_raw(Box::new(profile)),
        }
    }

    fn take(&mut self) -> Option<Box<InnerDynamicProfile>> {
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());
        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }
}

impl Drop for DynamicProfile {
    fn drop(&mut self) {
        drop(self.take())
    }
}

#[allow(dead_code)]
#[repr(C)]
pub enum DynamicProfileNewResult {
    Ok(DynamicProfile),
    Err(Error),
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DynamicStringIndex {
    pub value: u32,
}

impl From<InnerDynamicStringIndex> for DynamicStringIndex {
    fn from(value: InnerDynamicStringIndex) -> Self {
        Self { value: value.value }
    }
}

impl From<DynamicStringIndex> for InnerDynamicStringIndex {
    fn from(value: DynamicStringIndex) -> Self {
        Self { value: value.value }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DynamicFunctionIndex {
    pub value: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DynamicFunction {
    pub name: DynamicStringIndex,
    pub filename: DynamicStringIndex,
}

impl From<InnerDynamicFunction> for DynamicFunction {
    fn from(value: InnerDynamicFunction) -> Self {
        Self {
            name: value.name.into(),
            filename: value.filename.into(),
        }
    }
}

impl From<DynamicFunction> for InnerDynamicFunction {
    fn from(value: DynamicFunction) -> Self {
        Self {
            name: value.name.into(),
            filename: value.filename.into(),
        }
    }
}

impl From<InnerDynamicFunctionIndex> for DynamicFunctionIndex {
    fn from(value: InnerDynamicFunctionIndex) -> Self {
        Self { value: value.value }
    }
}

impl From<DynamicFunctionIndex> for InnerDynamicFunctionIndex {
    fn from(value: DynamicFunctionIndex) -> Self {
        Self { value: value.value }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DynamicStackTraceIndex {
    pub value: u32,
}

impl From<InnerDynamicStackTraceIndex> for DynamicStackTraceIndex {
    fn from(value: InnerDynamicStackTraceIndex) -> Self {
        Self { value: value.value }
    }
}

impl From<DynamicStackTraceIndex> for InnerDynamicStackTraceIndex {
    fn from(value: DynamicStackTraceIndex) -> Self {
        Self { value: value.value }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DynamicLocation {
    pub function: DynamicFunctionIndex,
    pub line: u32,
}

impl From<DynamicLocation> for InnerDynamicLocation {
    fn from(value: DynamicLocation) -> Self {
        Self {
            function: value.function.into(),
            line: value.line,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct DynamicLabel<'a> {
    pub key: DynamicStringIndex,
    pub str: CharSlice<'a>,
    pub num: i64,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct DynamicSample<'a> {
    pub values: Slice<'a, i64>,
    pub labels: Slice<'a, DynamicLabel<'a>>,
}

#[cfg(test)]
impl From<DynamicProfileNewResult> for std::result::Result<DynamicProfile, Error> {
    fn from(value: DynamicProfileNewResult) -> Self {
        match value {
            DynamicProfileNewResult::Ok(profile) => Ok(profile),
            DynamicProfileNewResult::Err(err) => Err(err),
        }
    }
}

unsafe fn dynamic_profile_ptr_to_inner<'a>(
    profile_ptr: *mut DynamicProfile,
) -> anyhow::Result<&'a mut InnerDynamicProfile> {
    match profile_ptr.as_mut() {
        None => anyhow::bail!("dynamic profile pointer was null"),
        Some(inner_ptr) => match inner_ptr.inner.as_mut() {
            Some(profile) => Ok(profile),
            None => {
                anyhow::bail!("dynamic profile inner pointer was null (indicates use-after-free)")
            }
        },
    }
}

fn ffi_labels_to_inner<'a>(
    labels: &'a [DynamicLabel<'a>],
) -> anyhow::Result<Vec<InnerDynamicLabel<'a>>> {
    labels
        .iter()
        .map(|label| {
            Ok(InnerDynamicLabel {
                key: label.key.into(),
                str: str::from_utf8(label.str.try_as_bytes()?)?,
                num: label.num,
            })
        })
        .collect()
}

fn ffi_locations_to_inner(locations: &[DynamicLocation]) -> Vec<InnerDynamicLocation> {
    locations.iter().copied().map(Into::into).collect()
}

fn ffi_sample_to_parts<'a>(
    sample: DynamicSample<'a>,
) -> anyhow::Result<(&'a [i64], Vec<InnerDynamicLabel<'a>>)> {
    let labels = ffi_labels_to_inner(sample.labels.try_as_slice()?)?;
    let values = sample.values.try_as_slice()?;
    Ok((values, labels))
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_with_dictionary(
    out: *mut DynamicProfile,
    dict: &ArcHandle<InnerDynamicProfilesDictionary>,
    sample_types: Slice<SampleType>,
    period: Option<&Period>,
    start_time: Option<&Timespec>,
) -> crate::profile_status::ProfileStatus {
    crate::ensure_non_null_out_parameter!(out);
    match unsafe { dynamic_profile_with_dictionary(dict, sample_types, period, start_time) } {
        Ok(profile) => unsafe {
            out.write(profile);
            crate::profile_status::ProfileStatus::OK
        },
        Err(err) => crate::profile_status::ProfileStatus::from(err),
    }
}

unsafe fn dynamic_profile_with_dictionary(
    dict: &ArcHandle<InnerDynamicProfilesDictionary>,
    sample_types: Slice<SampleType>,
    period: Option<&Period>,
    start_time: Option<&Timespec>,
) -> std::result::Result<DynamicProfile, crate::ProfileError> {
    let sample_types = sample_types.try_as_slice()?;
    let period = period.copied();
    let start_time = start_time.map(Into::into);
    let dict = dict.try_clone_into_arc()?;
    let profile =
        InnerDynamicProfile::try_new_with_dictionary(sample_types, period, start_time, dict)
            .map_err(crate::ProfileError::from_display)?;
    Ok(DynamicProfile::new(profile))
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_drop(profile: *mut DynamicProfile) {
    if !profile.is_null() {
        drop((*profile).take())
    }
}

#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_intern_stacktrace(
    profile: *mut DynamicProfile,
    locations: Slice<DynamicLocation>,
) -> Result<DynamicStackTraceIndex> {
    wrap_with_ffi_result!({
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let locations = ffi_locations_to_inner(locations.try_as_slice()?);
        Ok::<DynamicStackTraceIndex, anyhow::Error>(
            profile.intern_stacktrace(locations.as_slice())?.into(),
        )
    })
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_add_sample_by_stacktrace(
    profile: *mut DynamicProfile,
    stacktrace: DynamicStackTraceIndex,
    sample: DynamicSample,
    timestamp_ns: i64,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let (values, labels) = ffi_sample_to_parts(sample)?;
        let sample = InnerDynamicSample {
            values,
            labels: labels.as_slice(),
        };
        profile.add_sample_by_stacktrace(stacktrace.into(), sample, timestamp_ns)?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_add_sample_by_stacktrace failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_add_sample_by_locations(
    profile: *mut DynamicProfile,
    locations: Slice<DynamicLocation>,
    sample: DynamicSample,
    timestamp_ns: i64,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let locations = ffi_locations_to_inner(locations.try_as_slice()?);
        let (values, labels) = ffi_sample_to_parts(sample)?;
        let sample = InnerDynamicSample {
            values,
            labels: labels.as_slice(),
        };
        profile.add_sample_by_locations(locations.as_slice(), sample, timestamp_ns)?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_add_sample_by_locations failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_set_endpoint(
    profile: *mut DynamicProfile,
    local_root_span_id: u64,
    endpoint: CharSlice,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let endpoint = str::from_utf8(endpoint.try_as_bytes()?)?;
        profile.set_endpoint(local_root_span_id, endpoint)?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_set_endpoint failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_add_endpoint_count(
    profile: *mut DynamicProfile,
    endpoint: CharSlice,
    value: i64,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let endpoint = str::from_utf8(endpoint.try_as_bytes()?)?;
        profile.add_endpoint_count(endpoint, value)?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_add_endpoint_count failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_add_upscaling_rule_poisson(
    profile: *mut DynamicProfile,
    offset_values: Slice<usize>,
    label_key: DynamicStringIndex,
    label_value: CharSlice,
    sum_value_offset: usize,
    count_value_offset: usize,
    sampling_distance: u64,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let label_value = str::from_utf8(label_value.try_as_bytes()?)?;
        profile.add_upscaling_rule_poisson(
            offset_values.as_slice(),
            label_key.into(),
            label_value,
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        )?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_add_upscaling_rule_poisson failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_add_upscaling_rule_poisson_non_sample_type_count(
    profile: *mut DynamicProfile,
    offset_values: Slice<usize>,
    label_key: DynamicStringIndex,
    label_value: CharSlice,
    sum_value_offset: usize,
    count_value: u64,
    sampling_distance: u64,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let label_value = str::from_utf8(label_value.try_as_bytes()?)?;
        profile.add_upscaling_rule_poisson_non_sample_type_count(
            offset_values.as_slice(),
            label_key.into(),
            label_value,
            sum_value_offset,
            count_value,
            sampling_distance,
        )?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_add_upscaling_rule_poisson_non_sample_type_count failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_add_upscaling_rule_proportional(
    profile: *mut DynamicProfile,
    offset_values: Slice<usize>,
    label_key: DynamicStringIndex,
    label_value: CharSlice,
    scale: f64,
) -> ProfileResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let label_value = str::from_utf8(label_value.try_as_bytes()?)?;
        profile.add_upscaling_rule_proportional(
            offset_values.as_slice(),
            label_key.into(),
            label_value,
            scale,
        )?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_add_upscaling_rule_proportional failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_serialize_and_clear_period_local_data(
    profile: *mut DynamicProfile,
    end_time: Option<&Timespec>,
    duration_nanos: i64,
) -> SerializeResult {
    (|| {
        let profile = dynamic_profile_ptr_to_inner(profile)?;
        let end_time = end_time.map(Into::into);
        let duration = if duration_nanos <= 0 {
            None
        } else {
            Some(Duration::from_nanos(duration_nanos as u64))
        };
        Ok::<libdd_profiling::internal::EncodedProfile, anyhow::Error>(
            profile.serialize_and_clear_period_local_data(end_time, duration)?,
        )
    })()
    .context("ddog_prof_DynamicProfile_serialize_and_clear_period_local_data failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_clear_period_local_data(
    profile: *mut DynamicProfile,
) -> ProfileResult {
    (|| {
        dynamic_profile_ptr_to_inner(profile)?.clear_period_local_data()?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_clear_period_local_data failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfile_clear_all_data(
    profile: *mut DynamicProfile,
) -> ProfileResult {
    (|| {
        dynamic_profile_ptr_to_inner(profile)?.clear_all_data()?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_DynamicProfile_clear_all_data failed")
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common_ffi::ToInner;
    use libdd_profiling_protobuf::prost_impls::Message;

    fn decode_profile(
        encoded: &mut libdd_common_ffi::Handle<libdd_profiling::internal::EncodedProfile>,
    ) -> libdd_profiling_protobuf::prost_impls::Profile {
        let bytes = unsafe { encoded.to_inner_mut() }
            .expect("encoded")
            .buffer
            .clone();
        let decoded = zstd::stream::decode_all(std::io::Cursor::new(bytes)).expect("decompress");
        libdd_profiling_protobuf::prost_impls::Profile::decode(decoded.as_slice()).expect("decode")
    }

    #[test]
    fn ffi_roundtrip_serializes_dynamic_profile() -> std::result::Result<(), Error> {
        unsafe {
            let sample_type = SampleType::WallTime;
            let mut dict = ArcHandle::<InnerDynamicProfilesDictionary>::default();
            std::result::Result::<(), _>::from(
                crate::profiles::dynamic_profiles_dictionary::ddog_prof_DynamicProfilesDictionary_new(
                    &mut dict,
                ),
            )
            .unwrap();
            let dict_ref = dict.as_inner().ok();

            let mut function_name = DynamicStringIndex::default();
            std::result::Result::<(), _>::from(
                crate::profiles::dynamic_profiles_dictionary::ddog_prof_DynamicProfilesDictionary_insert_str(
                    &mut function_name,
                    dict_ref,
                    CharSlice::from("ruby_func"),
                    crate::profiles::utf8::Utf8Option::Validate,
                ),
            )
            .unwrap();
            let mut filename = DynamicStringIndex::default();
            std::result::Result::<(), _>::from(
                crate::profiles::dynamic_profiles_dictionary::ddog_prof_DynamicProfilesDictionary_insert_str(
                    &mut filename,
                    dict_ref,
                    CharSlice::from("file.rb"),
                    crate::profiles::utf8::Utf8Option::Validate,
                ),
            )
            .unwrap();
            let mut label_key = DynamicStringIndex::default();
            std::result::Result::<(), _>::from(
                crate::profiles::dynamic_profiles_dictionary::ddog_prof_DynamicProfilesDictionary_insert_str(
                    &mut label_key,
                    dict_ref,
                    CharSlice::from("thread id"),
                    crate::profiles::utf8::Utf8Option::Validate,
                ),
            )
            .unwrap();
            let mut function = DynamicFunctionIndex::default();
            let ffi_function = DynamicFunction {
                name: function_name,
                filename,
            };
            std::result::Result::<(), _>::from(
                crate::profiles::dynamic_profiles_dictionary::ddog_prof_DynamicProfilesDictionary_insert_function(
                    &mut function,
                    dict_ref,
                    &ffi_function,
                ),
            )
            .unwrap();

            let mut profile = DynamicProfile {
                inner: std::ptr::null_mut(),
            };
            std::result::Result::<(), _>::from(ddog_prof_DynamicProfile_with_dictionary(
                &mut profile,
                &dict,
                Slice::from_raw_parts(&sample_type, 1),
                None,
                None,
            ))
            .unwrap();

            let locations = [DynamicLocation { function, line: 27 }];
            let labels = [DynamicLabel {
                key: label_key,
                str: CharSlice::empty(),
                num: 99,
            }];
            let values = [123_i64];
            let sample = DynamicSample {
                values: Slice::from(&values[..]),
                labels: Slice::from(&labels[..]),
            };

            std::result::Result::<(), Error>::from(
                ddog_prof_DynamicProfile_add_sample_by_locations(
                    &mut profile,
                    Slice::from(&locations[..]),
                    sample,
                    0,
                ),
            )?;

            let mut encoded = match ddog_prof_DynamicProfile_serialize_and_clear_period_local_data(
                &mut profile,
                None,
                0,
            ) {
                SerializeResult::Ok(encoded) => encoded,
                SerializeResult::Err(err) => return Err(err),
            };
            let decoded = decode_profile(&mut encoded);

            assert!(decoded
                .string_table
                .iter()
                .any(|value| value == "ruby_func"));
            assert!(decoded.string_table.iter().any(|value| value == "file.rb"));
            assert!(decoded
                .samples
                .iter()
                .any(|sample| sample.values == vec![123]));

            ddog_prof_EncodedProfile_drop(&mut encoded);
            ddog_prof_DynamicProfile_drop(&mut profile);
            crate::profiles::dynamic_profiles_dictionary::ddog_prof_DynamicProfilesDictionary_drop(
                &mut dict,
            );
            Ok(())
        }
    }
}
