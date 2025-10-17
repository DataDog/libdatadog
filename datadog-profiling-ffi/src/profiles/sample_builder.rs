// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arc_handle::ArcHandle2;
use crate::profile_handle::ProfileHandle2;
use crate::profiles::{ensure_non_null_insert, ensure_non_null_out_parameter, Utf8Option};
use crate::ProfileStatus2;
use datadog_profiling2::profiles::collections::StringId2;
use datadog_profiling2::profiles::datatypes::{self, Link2, Profile2, ScratchPad, StackId2};
use datadog_profiling2::profiles::ProfileError;
use ddcommon_ffi::{CharSlice, Timespec};
use std::time::SystemTime;

pub struct SampleBuilder2 {
    builder: datatypes::SampleBuilder,
    profile: ProfileHandle2<Profile2>, // borrowed
}

/// Creates a `SampleBuilder` backed by the provided `ScratchPad`.
///
/// Use [`ddog_prof2_SampleBuilder_drop`] to free it, see it for more details.
///
/// # Safety
///
/// - `out` must be non-null and valid for writes of `SampleBuilderHandle`.
/// - `profile` handle must outlive the sample value, as it borrows it.
/// - `scratchpad` must be a live handle; its resource must outlive all uses of the returned builder
///   handle.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_new(
    out: *mut ProfileHandle2<SampleBuilder2>,
    profile: ProfileHandle2<Profile2>,
    scratchpad: ArcHandle2<ScratchPad>,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(out);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let sp = scratchpad.as_inner()?;
        let attributes = sp.attributes().try_clone()?;
        let links = sp.links().try_clone()?;
        let builder = datatypes::SampleBuilder::new(attributes, links);
        let ffi_builder = SampleBuilder2 { builder, profile };
        let handle = ProfileHandle2::try_new(ffi_builder)?;
        unsafe { out.write(handle) };
        Ok(())
    }())
}

/// Sets the stack id of the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable reference for the
///   duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_stack_id(
    mut handle: ProfileHandle2<SampleBuilder2>,
    stack_id: StackId2,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.set_stack_id(stack_id);
        Ok(())
    }())
}

/// Appends a value to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable reference for the
///   duration of the call.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_value(
    mut handle: ProfileHandle2<SampleBuilder2>,
    value: i64,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.push_value(value)?;
        Ok(())
    }())
}

/// Adds a string attribute to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable reference for the
///   duration of the call.
/// - `key`/`val` must follow the UTF-8 policy indicated by `utf8`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_attribute_str(
    mut handle: ProfileHandle2<SampleBuilder2>,
    key_id: StringId2,
    val: CharSlice<'_>,
    utf8: Utf8Option,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let val = unsafe { utf8.try_as_bytes_convert(val)? };
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder
            .builder
            .push_attribute_str(key_id, val.as_ref())?;
        Ok(())
    }())
}

/// Adds an integer attribute to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable reference for the
///   duration of the call.
/// - `key` must follow the UTF-8 policy indicated by `utf8`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_attribute_int(
    mut handle: ProfileHandle2<SampleBuilder2>,
    key_id: StringId2,
    val: i64,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.push_attribute_int(key_id, val)?;
        Ok(())
    }())
}

/// Sets the link on the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable reference for the
///   duration of the call.
/// - `link` must be non-null and point to a valid `Link` for the duration of the call.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_link(
    mut handle: ProfileHandle2<SampleBuilder2>,
    link: *const Link2,
) -> ProfileStatus2 {
    ensure_non_null_insert!(link);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        let link = unsafe { *link };
        ffi_builder.builder.set_link(link)?;
        Ok(())
    }())
}

/// Sets a timestamp (in nanoseconds) on the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable reference for the
///   duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_timestamp(
    mut handle: ProfileHandle2<SampleBuilder2>,
    timestamp: Timespec,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let timestamp = SystemTime::from(timestamp);
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.set_timestamp(timestamp);
        Ok(())
    }())
}

/// Build the sample, and insert it into the profile. Done as one operation to
/// avoid boxing and exposing the Sample to FFI, since it isn't FFI-safe.
///
/// This will steal the contents of the sample builder. It is safe to drop the
/// sample builder afterward, but it isn't necessary if it succeeds.
/// Builds a sample from the builder and inserts it into `profile`.
///
/// # Safety
///
/// - `builder` must point to a valid `ProfileHandle<SampleBuilder>`.
/// - After a successful build, the builder’s internal state is consumed and must not be used unless
///   rebuilt.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_finish(
    builder: *mut ProfileHandle2<SampleBuilder2>,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let builder_handle = builder.as_mut().ok_or(ProfileError::InvalidInput)?;
        // todo: safety
        let ffi_builder = unsafe { builder_handle.as_inner_mut()? };
        let sample = ffi_builder.builder.build()?;
        // todo: safety
        let prof = unsafe { ffi_builder.profile.as_inner_mut()? };
        prof.add_sample(sample)
    }())
}

/// Free the resource associated with the sample builder handle.
///
/// # Safety
///
/// - If non-null, `builder` must point to a valid `ProfileHandle<SampleBuilder>`.
/// - The underlying resource must be dropped at most once across all copies of the handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_SampleBuilder_drop(
    builder: *mut ProfileHandle2<SampleBuilder2>,
) {
    if let Some(h) = builder.as_mut() {
        drop(h.take());
    }
}
