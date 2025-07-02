// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_alloc::Box;
use datadog_profiling::{
    collections::{string_table::StringTable, SliceSet, Store},
    profiles::{
        Compressor, Endpoints, LabelsSet, ProfileBuilder, ProfileError,
        ProfileVoidResult, SampleManager,
    },
};
use datadog_profiling_protobuf::{Function, Location, Mapping};

// FFI interface

#[repr(C)]
pub enum ProfileBuilderNewResult {
    Ok(*mut ProfileBuilder),
    Err(ProfileError),
}

/// Creates a new ProfileBuilder.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_ProfileBuilder_new() -> ProfileBuilderNewResult {
    match Box::try_new(ProfileBuilder::new()) {
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
    builder: *mut ProfileBuilder,
    functions: *mut Store<Function>,
    strings: *mut StringTable,
    compressor: *mut Compressor,
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
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(
        builder.add_functions(functions, strings, compressor),
    )
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_locations(
    builder: *mut ProfileBuilder,
    locations: *mut Store<Location>,
    compressor: *mut Compressor,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(locations) = locations.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_locations(locations, compressor))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_mappings(
    builder: *mut ProfileBuilder,
    mappings: *mut Store<Mapping>,
    strings: *mut StringTable,
    compressor: *mut Compressor,
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
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_mappings(mappings, strings, compressor))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_samples(
    builder: *mut ProfileBuilder,
    samples: *mut SampleManager,
    labels_set: *mut LabelsSet,
    labels_strings: *mut StringTable,
    stack_traces: *mut SliceSet<u64>,
    endpoints: *mut Endpoints,
    compressor: *mut Compressor,
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
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_samples(
        samples,
        labels_set,
        labels_strings,
        stack_traces,
        endpoints,
        compressor,
    ))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_drop(
    builder: *mut *mut ProfileBuilder,
) {
    if !builder.is_null() && !(*builder).is_null() {
        drop(Box::from_raw(*builder));
        *builder = std::ptr::null_mut();
    }
}
