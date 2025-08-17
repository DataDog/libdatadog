// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_alloc::Box;
use datadog_profiling::{
    collections::{string_table::StringTable, SliceSet, Store},
    profiles::{
        EncodedProfile, Endpoints, LabelsSet, PprofBuilder, ProfileError,
        ProfileVoidResult, SampleManager,
    },
};
use datadog_profiling_protobuf::{Function, Location, Mapping};
use ddcommon_ffi::Timespec;
use std::time::SystemTime;

// FFI interface

#[repr(C)]
pub enum ProfileBuilderNewResult<'a> {
    Ok(*mut PprofBuilder<'a>),
    Err(ProfileError),
}

/// Creates a new ProfileBuilder. All string tables added must outlive the
/// profile builder and should not be mutated until the builder is dropped.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_new(
    start_time: Timespec,
) -> ProfileBuilderNewResult<'static> {
    match Box::try_new(PprofBuilder::new(SystemTime::from(start_time))) {
        Ok(boxed) => ProfileBuilderNewResult::Ok(Box::into_raw(boxed)),
        Err(_) => ProfileBuilderNewResult::Err(ProfileError::OutOfMemory),
    }
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_functions(
    builder: *mut PprofBuilder,
    functions: *mut Store<Function>,
    strings: *mut StringTable,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(functions) = functions.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(strings) = strings.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_functions(functions, strings))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_locations(
    builder: *mut PprofBuilder,
    locations: *mut Store<Location>,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(locations) = locations.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_locations(locations))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_mappings(
    builder: *mut PprofBuilder,
    mappings: *mut Store<Mapping>,
    strings: *mut StringTable,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(mappings) = mappings.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(strings) = strings.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_mappings(mappings, strings))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_samples(
    builder: *mut PprofBuilder,
    samples: *mut SampleManager,
    labels_set: *mut LabelsSet,
    labels_strings: *mut StringTable,
    stack_traces: *mut SliceSet<u64>,
    endpoints: *mut Endpoints,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(samples) = samples.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(labels_set) = labels_set.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(labels_strings) = labels_strings.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(stack_traces) = stack_traces.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(endpoints) = endpoints.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_samples(
        samples,
        labels_set,
        labels_strings,
        stack_traces,
        endpoints,
    ))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_drop(
    builder: *mut *mut PprofBuilder,
) {
    if !builder.is_null() && !(*builder).is_null() {
        drop(Box::from_raw(*builder));
        *builder = std::ptr::null_mut();
    }
}

#[repr(C)]
pub enum ProfileBuilderBuildResult {
    Ok(*mut EncodedProfile),
    Err(ProfileError),
}

/// Builds the profile, consuming the builder and returning an EncodedProfile.
///
/// # Safety
/// The builder pointer must be valid and not null.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_build(
    builder: *mut *mut PprofBuilder,
    end_time: *const Timespec,
) -> ProfileBuilderBuildResult {
    let Some(builder_ptr) = builder.as_mut() else {
        return ProfileBuilderBuildResult::Err(ProfileError::InvalidInput);
    };
    if builder_ptr.is_null() {
        return ProfileBuilderBuildResult::Err(ProfileError::InvalidInput);
    }
    let builder = unsafe { Box::from_raw(*builder_ptr) };
    *builder_ptr = std::ptr::null_mut();

    let end = end_time.as_ref().map(SystemTime::from);

    // Unbox it so we can take ownership in .build() below.
    let builder = Box::into_inner(builder);
    match builder.build(end) {
        Ok(profile) => match Box::try_new(profile) {
            Ok(boxed) => ProfileBuilderBuildResult::Ok(Box::into_raw(boxed)),
            Err(_) => ProfileBuilderBuildResult::Err(ProfileError::OutOfMemory),
        },
        Err(err) => ProfileBuilderBuildResult::Err(err),
    }
}

/// # Safety
///
/// Only pass a pointer to a valid reference to a `ddog_prof_EncodedProfile`, or null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_EncodedProfile_drop(
    profile: *mut *mut EncodedProfile,
) {
    if !profile.is_null() && !(*profile).is_null() {
        drop(Box::from_raw(*profile));
        *profile = std::ptr::null_mut();
    }
}
