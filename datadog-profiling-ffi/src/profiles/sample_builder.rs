// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arc_handle::ArcHandle;
use crate::profile_handle::ProfileHandle;
use crate::profiles::{
    ensure_non_null_insert, ensure_non_null_out_parameter, Utf8Option,
};
use crate::ProfileStatus;
use datadog_profiling::profiles::collections::StringId;
use datadog_profiling::profiles::datatypes::{
    self, Link, Profile, ScratchPad, StackId,
};
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::{CharSlice, Timespec};
use std::time::SystemTime;

pub struct SampleBuilder {
    builder: datatypes::SampleBuilder,
    profile: ProfileHandle<Profile>, // borrowed
}

/// Creates a `SampleBuilder` backed by the provided `ScratchPad`.
///
/// Use [`ddog_prof_SampleBuilder_drop`] to free it, see it for more details.
///
/// # Safety
///
/// - `out` must be non-null and valid for writes of `SampleBuilderHandle`.
/// - `profile` handle must outlive the sample value, as it borrows it.
/// - `scratchpad` must be a live handle; its resource must outlive all uses of
///   the returned builder handle.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_new(
    out: *mut ProfileHandle<SampleBuilder>,
    profile: ProfileHandle<Profile>,
    scratchpad: ArcHandle<ScratchPad>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let sp = scratchpad.as_inner()?;
        let attributes = sp.attributes().try_clone()?;
        let links = sp.links().try_clone()?;
        let builder = datatypes::SampleBuilder::new(attributes, links);
        let ffi_builder = SampleBuilder { builder, profile };
        let handle = ProfileHandle::try_new(ffi_builder)?;
        unsafe { out.write(handle) };
        Ok(())
    }())
}

/// Sets the stack id of the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_stack_id(
    mut handle: ProfileHandle<SampleBuilder>,
    stack_id: StackId,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.set_stack_id(stack_id);
        Ok(())
    }())
}

/// Appends a value to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_value(
    mut handle: ProfileHandle<SampleBuilder>,
    value: i64,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.push_value(value)?;
        Ok(())
    }())
}

/// Adds a string attribute to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
/// - `key`/`val` must follow the UTF-8 policy indicated by `utf8`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_attribute_str(
    mut handle: ProfileHandle<SampleBuilder>,
    key_id: StringId,
    val: CharSlice<'_>,
    utf8: Utf8Option,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let val = unsafe { utf8.try_as_bytes_convert(val)? };
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.push_attribute_str(key_id, val.as_ref())?;
        Ok(())
    }())
}

/// Adds an integer attribute to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
/// - `key` must follow the UTF-8 policy indicated by `utf8`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_attribute_int(
    mut handle: ProfileHandle<SampleBuilder>,
    key_id: StringId,
    val: i64,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let ffi_builder = unsafe { handle.as_inner_mut()? };
        ffi_builder.builder.push_attribute_int(key_id, val)?;
        Ok(())
    }())
}

/// Sets the link on the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
/// - `link` must be non-null and point to a valid `Link` for the duration of
///   the call.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_link(
    mut handle: ProfileHandle<SampleBuilder>,
    link: *const Link,
) -> ProfileStatus {
    ensure_non_null_insert!(link);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
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
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_timestamp(
    mut handle: ProfileHandle<SampleBuilder>,
    timestamp: Timespec,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
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
/// - After a successful build, the builder’s internal state is consumed and
///   must not be used unless rebuilt.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_finish(
    builder: *mut ProfileHandle<SampleBuilder>,
) -> ProfileStatus {
    let mut builder = {
        let Some(h) = builder.as_mut() else {
            return ProfileStatus::from(c"invalid input: argument `builder` to ddog_prof_SampleBuilder_finish was null");
        };
        let Some(boxed) = h.take() else {
            return ProfileStatus::from(c"internal error: argument `builder` to ddog_prof_SampleBuilder_finish was used with an interior null pointer");
        };
        boxed
    };
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let sample = builder.builder.build()?;
        // todo: safety
        let prof = unsafe { builder.profile.as_inner_mut()? };
        prof.add_sample(sample)
    }())
}

/// Free the resource associated with the sample builder handle.
///
/// # Safety
///
/// - If non-null, `builder` must point to a valid `ProfileHandle<SampleBuilder>`.
/// - The underlying resource must be dropped at most once across all copies of
///   the handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_drop(
    builder: *mut ProfileHandle<SampleBuilder>,
) {
    if let Some(h) = builder.as_mut() {
        drop(h.take());
    }
}
