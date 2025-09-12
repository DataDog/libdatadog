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
    Link, Profile, SampleBuilder, ScratchPad, StackId,
};
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::{CharSlice, Timespec};
use std::time::SystemTime;

/// Creates a `SampleBuilder` backed by the provided `ScratchPad`.
///
/// Use [`ddog_prof_SampleBuilder_drop`] to free it, see it for more details.
///
/// # Safety
///
/// - `out` must be non-null and valid for writes of `SampleBuilderHandle`.
/// - `scratchpad` must be a live handle; its resource must outlive all uses of
///   the returned builder handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_new(
    out: *mut ProfileHandle<SampleBuilder>,
    scratchpad: ArcHandle<ScratchPad>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let sp = scratchpad.as_inner()?;
        let attributes = sp.attributes().try_clone()?;
        let links = sp.links().try_clone()?;
        let builder = SampleBuilder::new(attributes, links);
        let h = ProfileHandle::try_new(builder)?;
        unsafe { out.write(h) };
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
        let b = unsafe { handle.as_inner_mut()? };
        b.set_stack_id(stack_id);
        Ok(())
    }())
}

/// Appends a value to the builder.
///
/// # Safety
///
/// - `handle` must refer to a live builder and is treated as a unique mutable
///   reference for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_value(
    mut handle: ProfileHandle<SampleBuilder>,
    value: i64,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let b = unsafe { handle.as_inner_mut()? };
        b.push_value(value)?;
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
        let b = unsafe { handle.as_inner_mut()? };
        b.push_attribute_str(key_id, val.as_ref())?;
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
        let b = unsafe { handle.as_inner_mut()? };
        b.push_attribute_int(key_id, val)?;
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
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_link(
    mut handle: ProfileHandle<SampleBuilder>,
    link: *const Link,
) -> ProfileStatus {
    ensure_non_null_insert!(link);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let b = unsafe { handle.as_inner_mut()? };
        let link = unsafe { *link };
        b.set_link(link)?;
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
        let b = unsafe { handle.as_inner_mut()? };
        b.set_timestamp(timestamp);
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
/// - `profile` must refer to a live `Profile` and is treated as a unique
///   mutable reference for the duration of the call.
/// - After a successful build, the builder’s internal state is consumed and
///   must not be used unless rebuilt.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleBuilder_build_into_profile(
    builder: *mut ProfileHandle<SampleBuilder>,
    mut profile: ProfileHandle<Profile>,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let prof = unsafe { profile.as_inner_mut()? };
        let builder_handle =
            builder.as_mut().ok_or(ProfileError::InvalidInput)?;
        // Borrow the inner builder and build a sample, then add to profile.
        let b = unsafe { builder_handle.as_inner_mut()? };
        let sample = b.build()?;
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
