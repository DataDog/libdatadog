// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::num::NonZeroI64;

use super::datatypes::{profile_ptr_to_inner, Profile};
use datadog_profiling::{
    api::ManagedStringId,
    collections::identifiable::StringId,
    internal::{
        interning_api::{Generation, GenerationalId},
        FunctionId, LabelId, LabelSetId, LocationId, MappingId, StackTraceId,
    },
};
use ddcommon_ffi::{
    slice::AsBytes, wrap_with_ffi_result, wrap_with_void_ffi_result, CharSlice, MutSlice, Result,
    Slice, VoidResult,
};
use function_name::named;

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_function(
    profile: *mut Profile,
    name: GenerationalId<StringId>,
    system_name: GenerationalId<StringId>,
    filename: GenerationalId<StringId>,
    start_line: i64,
) -> Result<GenerationalId<FunctionId>> {
    wrap_with_ffi_result!({
        profile_ptr_to_inner(profile)?.intern_function(name, system_name, filename, start_line)
    })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_label_num(
    profile: *mut Profile,
    key: GenerationalId<StringId>,
    val: i64,
) -> Result<GenerationalId<LabelId>> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.intern_label_num(key, val, None) })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_label_num_with_unit(
    profile: *mut Profile,
    key: GenerationalId<StringId>,
    val: i64,
    unit: GenerationalId<StringId>,
) -> Result<GenerationalId<LabelId>> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.intern_label_num(key, val, Some(unit)) })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_label_str(
    profile: *mut Profile,
    key: GenerationalId<StringId>,
    val: GenerationalId<StringId>,
) -> Result<GenerationalId<LabelId>> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.intern_label_str(key, val) })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_labelset(
    profile: *mut Profile,
    labels: Slice<GenerationalId<LabelId>>,
) -> Result<GenerationalId<LabelSetId>> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.intern_labelset(labels.as_slice()) })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_location(
    profile: *mut Profile,
    mapping_id: GenerationalId<MappingId>,
    function_id: GenerationalId<FunctionId>,
    address: u64,
    line: i64,
) -> Result<GenerationalId<LocationId>> {
    wrap_with_ffi_result!({
        profile_ptr_to_inner(profile)?.intern_location(mapping_id, function_id, address, line)
    })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_managed_string(
    profile: *mut Profile,
    s: ManagedStringId,
) -> Result<GenerationalId<StringId>> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.intern_managed_string(s) })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_managed_strings(
    profile: *mut Profile,
    strings: Slice<ManagedStringId>,
    mut out: MutSlice<GenerationalId<StringId>>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        anyhow::ensure!(strings.len() == out.len());
        profile_ptr_to_inner(profile)?
            .intern_managed_strings(strings.as_slice(), out.as_mut_slice())?;
    })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_mapping(
    profile: *mut Profile,
    memory_start: u64,
    memory_limit: u64,
    file_offset: u64,
    filename: GenerationalId<StringId>,
    build_id: GenerationalId<StringId>,
) -> Result<GenerationalId<MappingId>> {
    wrap_with_ffi_result!({
        profile_ptr_to_inner(profile)?.intern_mapping(
            memory_start,
            memory_limit,
            file_offset,
            filename,
            build_id,
        )
    })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_sample(
    profile: *mut Profile,
    stacktrace: GenerationalId<StackTraceId>,
    values: Slice<i64>,
    labels: GenerationalId<LabelSetId>,
    timestamp: Option<NonZeroI64>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // TODO, this to_vec might not be necessary.
        profile_ptr_to_inner(profile)?.intern_sample(
            stacktrace,
            values.as_slice(),
            labels,
            timestamp,
        )?;
    })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_stacktrace(
    profile: *mut Profile,
    locations: Slice<GenerationalId<LocationId>>,
) -> Result<GenerationalId<StackTraceId>> {
    wrap_with_ffi_result!({
        profile_ptr_to_inner(profile)?.intern_stacktrace(locations.as_slice())
    })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_string(
    profile: *mut Profile,
    s: CharSlice,
) -> Result<GenerationalId<StringId>> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.intern_string(s.try_to_utf8()?) })
}

/// This functions interns its argument into the profiler.
/// If successful, it an opaque interning ID.
/// This ID is valid for use on this profiler, until the profiler is reset.
/// It is an error to use this id after the profiler has been reset, or on a different profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// All other arguments must remain valid for the length of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_intern_strings(
    profile: *mut Profile,
    strings: Slice<CharSlice>,
    mut out: MutSlice<GenerationalId<StringId>>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        anyhow::ensure!(strings.len() == out.len());
        let mut v = Vec::with_capacity(strings.len());
        for s in strings.iter() {
            v.push(s.try_to_utf8()?);
        }
        profile_ptr_to_inner(profile)?.intern_strings(&v, out.as_mut_slice())?;
    })
}

/// This functions returns the current generation of the profiler.
/// On error, it holds an error message in the error variant.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_get_generation(
    profile: *mut Profile,
) -> Result<Generation> {
    wrap_with_ffi_result!({ profile_ptr_to_inner(profile)?.get_generation() })
}

/// This functions returns whether the given generations are equal.
///
/// # Safety: No safety requirements
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_generations_are_equal(
    a: Generation,
    b: Generation,
) -> bool {
    a == b
}

/// This functions ends the current sample and allows the profiler exporter to continue, if it was
/// blocked.
/// It must have been paired with exactly one `sample_start`.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is probably thread-safe, but I haven't confirmed this.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_sample_end(profile: *mut Profile) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile_ptr_to_inner(profile)?.sample_end()?;
    })
}

/// This functions starts a sample and blocks the exporter from continuing.
///
/// # Safety
/// The `profile` ptr must point to a valid Profile object created by this
/// module.
/// This call is probably thread-safe, but I haven't confirmed this.
#[must_use]
#[no_mangle]
#[named]
pub unsafe extern "C" fn ddog_prof_Profile_sample_start(profile: *mut Profile) -> VoidResult {
    wrap_with_void_ffi_result!({
        profile_ptr_to_inner(profile)?.sample_start()?;
    })
}
