// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::string_storage::{get_inner_string_storage, ManagedStringStorage};
use crate::{ensure_non_null_out_parameter, ArcHandle, ProfileError, ProfileStatus};
use anyhow::Context;
use function_name::named;
use libdd_common_ffi::slice::{AsBytes, ByteSlice, CharSlice, Slice};
use libdd_common_ffi::{wrap_with_ffi_result, Error, Handle, Timespec, ToInner};
use libdd_profiling::api::{self, ManagedStringId};
use libdd_profiling::profiles::datatypes::{ProfilesDictionary, StringId2};
use libdd_profiling::{api2, internal};
use std::num::NonZeroI64;
use std::str::Utf8Error;
use std::time::SystemTime;

/// Represents a profile. Do not access its member for any reason, only use
/// the C API functions on this struct.
#[repr(C)]
pub struct Profile {
    // This may be null, but if not it will point to a valid Profile.
    inner: *mut internal::Profile,
}

impl Profile {
    fn new(profile: internal::Profile) -> Self {
        Profile {
            inner: Box::into_raw(Box::new(profile)),
        }
    }

    fn take(&mut self) -> Option<Box<internal::Profile>> {
        // Leaving a null will help with double-free issues that can
        // arise in C. Of course, it's best to never get there in the
        // first place!
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());

        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        drop(self.take())
    }
}

/// A generic result type for when a profiling operation may fail, but there's
/// nothing to return in the case of success.
#[allow(dead_code)]
#[repr(C)]
pub enum ProfileResult {
    Ok(
        /// Do not use the value of Ok. This value only exists to overcome
        /// Rust -> C code generation.
        bool,
    ),
    Err(Error),
}

impl From<anyhow::Result<()>> for ProfileResult {
    fn from(value: anyhow::Result<()>) -> Self {
        match value {
            Ok(_) => Self::Ok(true),
            Err(err) => Self::Err(err.into()),
        }
    }
}

/// Returned by [ddog_prof_Profile_new].
#[allow(dead_code)]
#[repr(C)]
pub enum ProfileNewResult {
    Ok(Profile),
    #[allow(dead_code)]
    Err(Error),
}

#[allow(dead_code)]
#[repr(C)]
pub enum SerializeResult {
    Ok(Handle<internal::EncodedProfile>),
    Err(Error),
}

impl From<anyhow::Result<internal::EncodedProfile>> for SerializeResult {
    fn from(value: anyhow::Result<internal::EncodedProfile>) -> Self {
        match value {
            Ok(e) => Self::Ok(e.into()),
            Err(err) => Self::Err(err.into()),
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ValueType<'a> {
    pub type_: CharSlice<'a>,
    pub unit: CharSlice<'a>,
}

impl<'a> ValueType<'a> {
    pub fn new(type_: &'a str, unit: &'a str) -> Self {
        Self {
            type_: type_.into(),
            unit: unit.into(),
        }
    }
}

#[repr(C)]
pub struct Period<'a> {
    pub type_: ValueType<'a>,
    pub value: i64,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Label<'a> {
    pub key: CharSlice<'a>,
    pub key_id: ManagedStringId,

    /// At most one of the following must be present
    pub str: CharSlice<'a>,
    pub str_id: ManagedStringId,
    pub num: i64,

    /// Should only be present when num is present.
    /// Specifies the units of num.
    /// Use arbitrary string (for example, "requests") as a custom count unit.
    /// If no unit is specified, consumer may apply heuristic to deduce the unit.
    /// Consumers may also  interpret units like "bytes" and "kilobytes" as memory
    /// units and units like "seconds" and "nanoseconds" as time units,
    /// and apply appropriate unit conversions to these.
    pub num_unit: CharSlice<'a>,
    pub num_unit_id: ManagedStringId,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Function<'a> {
    /// Name of the function, in human-readable form if available.
    pub name: CharSlice<'a>,
    pub name_id: ManagedStringId,

    /// Name of the function, as identified by the system.
    /// For instance, it can be a C++ mangled name.
    pub system_name: CharSlice<'a>,
    pub system_name_id: ManagedStringId,

    /// Source file containing the function.
    pub filename: CharSlice<'a>,
    pub filename_id: ManagedStringId,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Location<'a> {
    /// todo: how to handle unknown mapping?
    pub mapping: Mapping<'a>,
    pub function: Function<'a>,

    /// The instruction address for this location, if available.  It
    /// should be within [Mapping.memory_start...Mapping.memory_limit]
    /// for the corresponding mapping. A non-leaf address may be in the
    /// middle of a call instruction. It is up to display tools to find
    /// the beginning of the instruction if necessary.
    pub address: u64,
    pub line: i64,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Mapping<'a> {
    /// Address at which the binary (or DLL) is loaded into memory.
    pub memory_start: u64,

    /// The limit of the address range occupied by this mapping.
    pub memory_limit: u64,

    /// Offset in the binary that corresponds to the first mapped address.
    pub file_offset: u64,

    /// The object this entry is loaded from.  This can be a filename on
    /// disk for the main binary and shared libraries, or virtual
    /// abstractions like "[vdso]".
    pub filename: CharSlice<'a>,
    pub filename_id: ManagedStringId,

    /// A string that uniquely identifies a particular program version
    /// with high probability. E.g., for binaries generated by GNU tools,
    /// it could be the contents of the .note.gnu.build-id field.
    pub build_id: CharSlice<'a>,
    pub build_id_id: ManagedStringId,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Sample<'a> {
    /// The leaf is at locations[0].
    pub locations: Slice<'a, Location<'a>>,

    /// The type and unit of each value is defined by the corresponding
    /// entry in Profile.sample_type. All samples must have the same
    /// number of values, the same as the length of Profile.sample_type.
    /// When aggregating multiple samples into a single sample, the
    /// result has a list of values that is the element-wise sum of the
    /// lists of the originals.
    pub values: Slice<'a, i64>,

    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: Slice<'a, Label<'a>>,
}

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct Label2<'a> {
    pub key: StringId2,

    /// At most one of `.str` and `.num` should not be empty.
    pub str: CharSlice<'a>,
    pub num: i64,

    /// Should only be present when num is present.
    /// Specifies the units of num.
    /// Use arbitrary string (for example, "requests") as a custom count unit.
    /// If no unit is specified, consumer may apply heuristic to deduce the unit.
    /// Consumers may also  interpret units like "bytes" and "kilobytes" as memory
    /// units and units like "seconds" and "nanoseconds" as time units,
    /// and apply appropriate unit conversions to these.
    pub num_unit: CharSlice<'a>,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Sample2<'a> {
    /// The leaf is at locations[0].
    pub locations: Slice<'a, api2::Location2>,

    /// The type and unit of each value is defined by the corresponding
    /// entry in Profile.sample_type. All samples must have the same
    /// number of values, the same as the length of Profile.sample_type.
    /// When aggregating multiple samples into a single sample, the
    /// result has a list of values that is the element-wise sum of the
    /// lists of the originals.
    pub values: Slice<'a, i64>,

    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: Slice<'a, Label2<'a>>,
}

impl<'a> TryFrom<&'a Mapping<'a>> for api::Mapping<'a> {
    type Error = Utf8Error;

    fn try_from(mapping: &'a Mapping<'a>) -> Result<Self, Self::Error> {
        let filename = mapping.filename.try_to_utf8()?;
        let build_id = mapping.build_id.try_to_utf8()?;
        Ok(Self {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename,
            build_id,
        })
    }
}

impl<'a> From<&'a Mapping<'a>> for api::StringIdMapping {
    fn from(mapping: &'a Mapping<'a>) -> Self {
        Self {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename_id,
            build_id: mapping.build_id_id,
        }
    }
}

impl<'a> From<&'a ValueType<'a>> for api::ValueType<'a> {
    fn from(vt: &'a ValueType<'a>) -> Self {
        Self::new(
            vt.type_.try_to_utf8().unwrap_or(""),
            vt.unit.try_to_utf8().unwrap_or(""),
        )
    }
}

impl<'a> From<&'a Period<'a>> for api::Period<'a> {
    fn from(period: &'a Period<'a>) -> Self {
        Self {
            r#type: api::ValueType::from(&period.type_),
            value: period.value,
        }
    }
}

impl<'a> TryFrom<&'a Function<'a>> for api::Function<'a> {
    type Error = Utf8Error;

    fn try_from(function: &'a Function<'a>) -> Result<Self, Self::Error> {
        let name = function.name.try_to_utf8()?;
        let system_name = function.system_name.try_to_utf8()?;
        let filename = function.filename.try_to_utf8()?;
        Ok(Self {
            name,
            system_name,
            filename,
        })
    }
}

impl<'a> From<&'a Function<'a>> for api::StringIdFunction {
    fn from(function: &'a Function<'a>) -> Self {
        Self {
            name: function.name_id,
            system_name: function.system_name_id,
            filename: function.filename_id,
        }
    }
}

impl<'a> TryFrom<&'a Location<'a>> for api::Location<'a> {
    type Error = Utf8Error;

    fn try_from(location: &'a Location<'a>) -> Result<Self, Self::Error> {
        let mapping = api::Mapping::try_from(&location.mapping)?;
        let function = api::Function::try_from(&location.function)?;
        Ok(Self {
            mapping,
            function,
            address: location.address,
            line: location.line,
        })
    }
}

impl<'a> From<&'a Location<'a>> for api::StringIdLocation {
    fn from(location: &'a Location<'a>) -> Self {
        Self {
            mapping: api::StringIdMapping::from(&location.mapping),
            function: api::StringIdFunction::from(&location.function),
            address: location.address,
            line: location.line,
        }
    }
}

impl<'a> TryFrom<&'a Label<'a>> for api::Label<'a> {
    type Error = Utf8Error;

    fn try_from(label: &'a Label<'a>) -> Result<Self, Self::Error> {
        let key = label.key.try_to_utf8()?;
        let str = label.str.try_to_utf8()?;
        let num_unit = label.num_unit.try_to_utf8()?;

        Ok(Self {
            key,
            str,
            num: label.num,
            num_unit,
        })
    }
}

impl<'a> From<&'a Label<'a>> for api::StringIdLabel {
    fn from(label: &'a Label<'a>) -> Self {
        let key = label.key_id;
        let str = label.str_id;
        let num_unit = label.num_unit_id;

        Self {
            key,
            str,
            num: label.num,
            num_unit,
        }
    }
}

impl<'a> TryFrom<Sample<'a>> for api::Sample<'a> {
    type Error = Utf8Error;

    fn try_from(sample: Sample<'a>) -> Result<Self, Self::Error> {
        let mut locations: Vec<api::Location> = Vec::with_capacity(sample.locations.len());

        for location in sample.locations.as_slice().iter() {
            locations.push(location.try_into()?)
        }

        let values = sample.values.into_slice();

        let mut labels: Vec<api::Label> = Vec::with_capacity(sample.labels.len());
        for label in sample.labels.as_slice().iter() {
            labels.push(label.try_into()?);
        }

        Ok(Self {
            locations,
            values,
            labels,
        })
    }
}

impl<'a> From<Sample<'a>> for api::StringIdSample<'a> {
    fn from(sample: Sample<'a>) -> Self {
        Self {
            locations: sample.locations.as_slice().iter().map(Into::into).collect(),
            values: sample.values.into_slice(),
            labels: sample.labels.as_slice().iter().map(Into::into).collect(),
        }
    }
}

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
pub unsafe extern "C" fn ddog_prof_Profile_new(
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
) -> ProfileNewResult {
    profile_new(sample_types, period, None)
}

/// Create a new profile with the given sample types. Must call
/// `ddog_prof_Profile_drop` when you are done with the profile.
///
/// # Arguments
/// * `out` - a non-null pointer to an uninitialized Profile.
/// * `dict`: a valid reference to a ProfilesDictionary handle.
/// * `sample_types`
/// * `period` - Optional period of the profile. Passing None/null translates to zero values.
///
/// # Safety
/// All slices must have pointers that are suitably aligned for their type
/// and must have the correct number of elements for the slice.
///
/// The `dict` reference must be to a valid ProfilesDictionary handle. It may
/// be an empty handle, but it must be a valid handle.
///
/// The `out` pointer must be non-null and suitable for pointer writes,
/// including that it has the correct size and alignment.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Profile_with_dictionary(
    out: *mut Profile,
    dict: &ArcHandle<ProfilesDictionary>,
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out);
    match profile_with_dictionary(dict, sample_types, period) {
        // SAFETY: checked that it isn't null above, the rest comes from this
        // function's own safety conditions. Technically, our safety conditions
        // don't require a null check, but we're being safe there.
        Ok(profile) => unsafe {
            out.write(profile);
            ProfileStatus::OK
        },
        Err(e) => ProfileStatus::from(e),
    }
}

unsafe fn profile_with_dictionary(
    dict: &ArcHandle<ProfilesDictionary>,
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
) -> Result<Profile, ProfileError> {
    let sample_types = sample_types.try_as_slice()?;
    let dict = dict.try_clone_into_arc()?;

    let mut types = Vec::new();
    types.try_reserve_exact(sample_types.len())?;
    types.extend(sample_types.iter().map(api::ValueType::from));
    let period = period.map(Into::into);

    match internal::Profile::try_new_with_dictionary(&types, period, dict) {
        Ok(ok) => Ok(Profile::new(ok)),
        Err(err) => Err(ProfileError::from(err)),
    }
}

/// Same as `ddog_profile_new` but also configures a `string_storage` for the profile.
#[no_mangle]
#[must_use]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_Profile_with_string_storage(
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
    string_storage: ManagedStringStorage,
) -> ProfileNewResult {
    profile_new(sample_types, period, Some(string_storage))
}

unsafe fn profile_new(
    sample_types: Slice<ValueType>,
    period: Option<&Period>,
    string_storage: Option<ManagedStringStorage>,
) -> ProfileNewResult {
    let types: Vec<api::ValueType> = sample_types.into_slice().iter().map(Into::into).collect();
    let period = period.map(Into::into);

    let result = match string_storage {
        None => internal::Profile::try_new(&types, period)
            .context("failed to initialize a profile without managed string storage"),
        Some(s) => {
            let string_storage = match get_inner_string_storage(s, true) {
                Ok(string_storage) => string_storage,
                Err(err) => return ProfileNewResult::Err(err.into()),
            };
            internal::Profile::try_with_string_storage(&types, period, string_storage)
                .context("failed to initialize a profile with managed string storage")
        }
    };
    match result {
        Ok(internal_profile) => {
            let ffi_profile = Profile::new(internal_profile);
            ProfileNewResult::Ok(ffi_profile)
        }
        Err(err) => ProfileNewResult::Err(err.into()),
    }
}

/// # Safety
/// The `profile` can be null, but if non-null it must point to a Profile
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_drop(profile: *mut Profile) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !profile.is_null() {
        drop((*profile).take())
    }
}

#[cfg(test)]
impl From<ProfileResult> for Result<(), Error> {
    fn from(result: ProfileResult) -> Self {
        match result {
            ProfileResult::Ok(_) => Ok(()),
            ProfileResult::Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
impl From<ProfileNewResult> for Result<Profile, Error> {
    fn from(result: ProfileNewResult) -> Self {
        match result {
            ProfileNewResult::Ok(p) => Ok(p),
            ProfileNewResult::Err(err) => Err(err),
        }
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
pub unsafe extern "C" fn ddog_prof_Profile_add(
    profile: *mut Profile,
    sample: Sample,
    timestamp: Option<NonZeroI64>,
) -> ProfileResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;
        let uses_string_ids = sample
            .labels
            .first()
            .is_some_and(|label| label.key.is_empty() && label.key_id.value > 0);

        if uses_string_ids {
            profile.add_string_id_sample(sample.into(), timestamp)
        } else {
            profile.try_add_sample(sample.try_into()?, timestamp)
        }
    })()
    .context("ddog_prof_Profile_add failed")
    .into()
}
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module. All pointers inside the `sample` need to be valid for the duration
/// of this call.
///
/// If successful, it returns the Ok variant.
/// On error, it holds an error message in the error variant.
///
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add2(
    profile: *mut Profile,
    sample: Sample2,
    timestamp: Option<NonZeroI64>,
) -> ProfileStatus {
    ProfileStatus::from((|| {
        let profile = profile_ptr_to_inner(profile)?;

        let locations = sample.locations.try_as_slice()?;
        let values = sample.values.try_as_slice()?;
        let labels = sample.labels.try_as_slice()?;

        let labels_iter = labels.iter().map(|label| -> anyhow::Result<api2::Label> {
            Ok(api2::Label {
                key: label.key,
                str: core::str::from_utf8(label.str.try_as_bytes()?)?,
                num: label.num,
                num_unit: core::str::from_utf8(label.str.try_as_bytes()?)?,
            })
        });
        profile
            .try_add_sample2(locations, values, labels_iter, timestamp)
            .context("ddog_prof_Profile_add failed")
    })())
}

pub(crate) unsafe fn profile_ptr_to_inner<'a>(
    profile_ptr: *mut Profile,
) -> anyhow::Result<&'a mut internal::Profile> {
    match profile_ptr.as_mut() {
        None => anyhow::bail!("profile pointer was null"),
        Some(inner_ptr) => match inner_ptr.inner.as_mut() {
            Some(profile) => Ok(profile),
            None => anyhow::bail!("profile's inner pointer was null (indicates use-after-free)"),
        },
    }
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
pub unsafe extern "C" fn ddog_prof_Profile_set_endpoint(
    profile: *mut Profile,
    local_root_span_id: u64,
    endpoint: CharSlice,
) -> ProfileResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;
        let endpoint = endpoint.to_utf8_lossy();
        profile.add_endpoint(local_root_span_id, endpoint)
    })()
    .context("ddog_prof_Profile_set_endpoint failed")
    .into()
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
pub unsafe extern "C" fn ddog_prof_Profile_add_endpoint_count(
    profile: *mut Profile,
    endpoint: CharSlice,
    value: i64,
) -> ProfileResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;
        let endpoint = endpoint.to_utf8_lossy();
        profile.add_endpoint_count(endpoint, value)
    })()
    .context("ddog_prof_Profile_set_endpoint failed")
    .into()
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
pub unsafe extern "C" fn ddog_prof_Profile_add_upscaling_rule_poisson(
    profile: *mut Profile,
    offset_values: Slice<usize>,
    label_name: CharSlice,
    label_value: CharSlice,
    sum_value_offset: usize,
    count_value_offset: usize,
    sampling_distance: u64,
) -> ProfileResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;
        anyhow::ensure!(sampling_distance != 0, "sampling_distance must not be 0");
        let upscaling_info = api::UpscalingInfo::Poisson {
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        };
        add_upscaling_rule(
            profile,
            offset_values,
            label_name,
            label_value,
            upscaling_info,
        )
    })()
    .context("ddog_prof_Profile_add_upscaling_rule_proportional failed")
    .into()
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
pub unsafe extern "C" fn ddog_prof_Profile_add_upscaling_rule_proportional(
    profile: *mut Profile,
    offset_values: Slice<usize>,
    label_name: CharSlice,
    label_value: CharSlice,
    total_sampled: u64,
    total_real: u64,
) -> ProfileResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;
        anyhow::ensure!(total_sampled != 0, "total_sampled must not be 0");
        anyhow::ensure!(total_real != 0, "total_real must not be 0");
        let upscaling_info = api::UpscalingInfo::Proportional {
            scale: total_real as f64 / total_sampled as f64,
        };
        add_upscaling_rule(
            profile,
            offset_values,
            label_name,
            label_value,
            upscaling_info,
        )
    })()
    .context("ddog_prof_Profile_add_upscaling_rule_proportional failed")
    .into()
}

unsafe fn add_upscaling_rule(
    profile: &mut internal::Profile,
    offset_values: Slice<usize>,
    label_name: CharSlice,
    label_value: CharSlice,
    upscaling_info: api::UpscalingInfo,
) -> anyhow::Result<()> {
    let label_name_n = label_name.to_utf8_lossy();
    let label_value_n = label_value.to_utf8_lossy();
    profile.add_upscaling_rule(
        offset_values.as_slice(),
        label_name_n.as_ref(),
        label_value_n.as_ref(),
        upscaling_info,
    )
}

/// # Safety
/// Only pass a reference to a valid `ddog_prof_EncodedProfile`, or null. A
/// valid reference also means that it hasn't already been dropped or exported (do not
/// call this twice on the same object).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_EncodedProfile_drop(
    profile: *mut Handle<internal::EncodedProfile>,
) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !profile.is_null() {
        drop((*profile).take())
    }
}

/// Given an EncodedProfile, get a slice representing the bytes in the pprof.
/// This slice is valid for use until the encoded_profile is modified in any way (e.g. dropped or
/// consumed).
/// # Safety
/// Only pass a reference to a valid `ddog_prof_EncodedProfile`.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_EncodedProfile_bytes<'a>(
    mut encoded_profile: *mut Handle<internal::EncodedProfile>,
) -> libdd_common_ffi::Result<ByteSlice<'a>> {
    wrap_with_ffi_result!({
        let slice = encoded_profile.to_inner_mut()?.buffer.as_slice();
        // Rountdtrip through raw pointers to avoid Rust complaining about lifetimes.
        let byte_slice = ByteSlice::from_raw_parts(slice.as_ptr(), slice.len());
        anyhow::Ok(byte_slice)
    })
}

/// Serialize the aggregated profile.
/// Drains the data, and then resets the profile for future use.
///
/// Don't forget to clean up the ok with `ddog_prof_EncodedProfile_drop` or
/// the error variant with `ddog_Error_drop` when you are done with them.
///
/// # Arguments
/// * `profile` - a reference to the profile being serialized.
/// * `start_time` - optional start time for the serialized profile. If None/null is passed, the
///   time of profile creation will be used.
/// * `end_time` - optional end time of the profile. If None/null is passed, the current time will
///   be used.
///
/// # Safety
/// The `profile` must point to a valid profile object.
/// The `start_time` and `end_time` must be null or otherwise point to a valid TimeSpec object.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_serialize(
    profile: *mut Profile,
    start_time: Option<&Timespec>,
    end_time: Option<&Timespec>,
) -> SerializeResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;

        let mut old_profile = profile.reset_and_return_previous()?;
        if let Some(start_time) = start_time {
            old_profile.set_start_time(start_time.into())?;
        }

        let end_time = end_time.map(SystemTime::from);
        old_profile.serialize_into_compressed_pprof(end_time, None)
    })()
    .context("ddog_prof_Profile_serialize failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_Vec_U8_as_slice(vec: &libdd_common_ffi::Vec<u8>) -> Slice<'_, u8> {
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
pub unsafe extern "C" fn ddog_prof_Profile_reset(profile: *mut Profile) -> ProfileResult {
    (|| {
        let profile = profile_ptr_to_inner(profile)?;
        profile.reset_and_return_previous()?;
        anyhow::Ok(())
    })()
    .context("ddog_prof_Profile_reset failed")
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctor_and_dtor() -> Result<(), Error> {
        unsafe {
            let sample_type: *const ValueType = &ValueType::new("samples", "count");
            let mut profile = Result::from(ddog_prof_Profile_new(
                Slice::from_raw_parts(sample_type, 1),
                None,
            ))?;
            ddog_prof_Profile_drop(&mut profile);
            Ok(())
        }
    }

    #[test]
    fn add_failure() -> Result<(), Error> {
        unsafe {
            let sample_type: *const ValueType = &ValueType::new("samples", "count");
            let mut profile = Result::from(ddog_prof_Profile_new(
                Slice::from_raw_parts(sample_type, 1),
                None,
            ))?;

            // wrong number of values (doesn't match sample types)
            let values: &[i64] = &[];

            let sample = Sample {
                locations: Slice::empty(),
                values: Slice::from(values),
                labels: Slice::empty(),
            };

            let result = Result::from(ddog_prof_Profile_add(&mut profile, sample, None));
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
            let mut profile = Result::from(ddog_prof_Profile_new(
                Slice::from_raw_parts(sample_type, 1),
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
                    name_id: ManagedStringId { value: 0 },
                    system_name: "{main}".into(),
                    system_name_id: ManagedStringId { value: 0 },
                    filename: "index.php".into(),
                    filename_id: ManagedStringId { value: 0 },
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

            Result::from(ddog_prof_Profile_add(&mut profile, sample, None))?;
            assert_eq!(
                profile
                    .inner
                    .as_ref()
                    .unwrap()
                    .only_for_testing_num_aggregated_samples(),
                1
            );

            Result::from(ddog_prof_Profile_add(&mut profile, sample, None))?;
            assert_eq!(
                profile
                    .inner
                    .as_ref()
                    .unwrap()
                    .only_for_testing_num_aggregated_samples(),
                1
            );

            ddog_prof_Profile_drop(&mut profile);
            Ok(())
        }
    }

    unsafe fn provide_distinct_locations_ffi() -> Profile {
        let sample_type: *const ValueType = &ValueType::new("samples", "count");
        let mut profile = Result::from(ddog_prof_Profile_new(
            Slice::from_raw_parts(sample_type, 1),
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
                name_id: ManagedStringId { value: 0 },
                system_name: "{main}".into(),
                system_name_id: ManagedStringId { value: 0 },
                filename: "index.php".into(),
                filename_id: ManagedStringId { value: 0 },
            },
            ..Default::default()
        }];
        let test_locations = vec![Location {
            mapping,
            function: Function {
                name: "test".into(),
                name_id: ManagedStringId { value: 0 },
                system_name: "test".into(),
                system_name_id: ManagedStringId { value: 0 },
                filename: "index.php".into(),
                filename_id: ManagedStringId { value: 0 },
            },
            line: 4,
            ..Default::default()
        }];
        let values: Vec<i64> = vec![1];
        let labels = vec![Label {
            key: Slice::from("pid"),
            key_id: ManagedStringId { value: 0 },
            str: Slice::from(""),
            str_id: ManagedStringId { value: 0 },
            num: 101,
            num_unit: Slice::from(""),
            num_unit_id: ManagedStringId { value: 0 },
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

        Result::from(ddog_prof_Profile_add(&mut profile, main_sample, None)).unwrap();
        assert_eq!(
            profile
                .inner
                .as_ref()
                .unwrap()
                .only_for_testing_num_aggregated_samples(),
            1
        );

        Result::from(ddog_prof_Profile_add(&mut profile, test_sample, None)).unwrap();
        assert_eq!(
            profile
                .inner
                .as_ref()
                .unwrap()
                .only_for_testing_num_aggregated_samples(),
            2
        );

        profile
    }

    #[test]
    fn distinct_locations_ffi() {
        unsafe {
            ddog_prof_Profile_drop(&mut provide_distinct_locations_ffi());
        }
    }
}
