// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arc_handle::ArcHandle;
use crate::profiles::utf8::{insert_str, Utf8Option};
use crate::profiles::{ensure_non_null_insert, ensure_non_null_out_parameter};
use crate::{EmptyHandleError, ProfileStatus};
use datadog_profiling::profiles::collections::StringId;
use datadog_profiling::profiles::datatypes::ProfilesDictionary;
use datadog_profiling::profiles::datatypes::{
    AttributeId, KeyValue, Link, LinkId, Location, LocationId, ScratchPad,
    StackId,
};
use datadog_profiling::profiles::string_writer::FallibleStringWriter;
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::CharSlice;

/// Allocates a new `ScratchPad` and returns a handle to it via the out
/// parameter `handle`.
///
/// Use [`ddog_prof_ScratchPad_drop`] to free; see its docs for more details.
///
/// # Safety
///
///  - `handle` must be non-null and valid for writes of `ScratchPadHandle`.
///  - Don't make C copies to handles, use [`ddog_prof_ScratchPad_try_clone`]
///    to get another refcounted copy (e.g. for another thread).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_new(
    handle: *mut ArcHandle<ScratchPad>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = ScratchPad::try_new()?;
        let h = ArcHandle::new(pad)?;
        unsafe { handle.write(h) };
        Ok(())
    }())
}

/// Creates a new handle to the same `ScratchPad` by incrementing the internal
/// reference count.
///
/// # Safety
///
/// - `out` must be non-null and valid for writes of `ScratchPadHandle`.
/// - `handle` must refer to a live `ScratchPad`.
/// - Do not duplicate handles via memcpy; always use this API to create new
///   handles so the reference count is maintained correctly.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_try_clone(
    out: *mut ArcHandle<ScratchPad>,
    handle: ArcHandle<ScratchPad>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let cloned = handle.try_clone()?;
        unsafe { out.write(cloned) };
        Ok(())
    }())
}

/// Decrements the refcount on the underlying `ScratchPad` resource held by
/// `handle` and leaves an empty handle. If the refcount hits zero, it will
/// be destroyed.
///
/// # Safety
///
/// - If non-null, `handle` must point to a valid `ScratchPadHandle`.
/// - Only drop properly created/cloned handles.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_drop(
    handle: *mut ArcHandle<ScratchPad>,
) {
    if let Some(h) = handle.as_mut() {
        h.drop_resource();
    }
}

/// Inserts a `Location` and returns its id.
///
/// # Safety
///
/// - `out_location_id` must be non-null and valid for writes of `LocationId`.
/// - `handle` must refer to a live `ScratchPad`.
/// - `location` must be non-null and valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_location(
    out_location_id: *mut LocationId,
    handle: ArcHandle<ScratchPad>,
    location: *const Location,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_location_id);
    ensure_non_null_insert!(location);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let id = pad.locations().try_insert(unsafe { *location })?;
        unsafe { out_location_id.write(id.into_raw()) };
        Ok(())
    }())
}

/// Interns a stack of `LocationId` and returns its `StackId`.
///
/// # Safety
///
/// - `out_stack_id` must be non-null and valid for writes of `StackId`.
/// - `handle` must refer to a live `ScratchPad`.
/// - `locations` must point to valid `LocationId`s obtained from the same
///   `ScratchPad` and be valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_stack(
    out_stack_id: *mut StackId,
    handle: ArcHandle<ScratchPad>,
    locations: ddcommon_ffi::Slice<'_, LocationId>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_stack_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let slice =
            locations.try_as_slice().ok_or(ProfileError::InvalidInput)?;
        // SAFETY: re-interpreting LocationId as SetId<Location> is safe as
        // long as they were made from SetId::into_raw.
        let ids = unsafe {
            core::slice::from_raw_parts(slice.as_ptr().cast(), slice.len())
        };
        let stack_id = pad.stacks().try_insert(ids)?;
        unsafe { out_stack_id.write(stack_id) };
        Ok(())
    }())
}

/// Inserts a `Link` and returns its id.
///
/// # Safety
///
/// - `out_link_id` must be non-null and valid for writes of `LinkId`.
/// - `handle` must refer to a live `ScratchPad`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_link(
    out_link_id: *mut LinkId,
    handle: ArcHandle<ScratchPad>,
    link: Link,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_link_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let id = pad.links().try_insert(link)?;
        unsafe { out_link_id.write(id.into_raw()) };
        Ok(())
    }())
}

/// Inserts a string attribute key/value pair and returns its id.
///
/// # Safety
///
/// - `out_attr_id` must be non-null and valid for writes of `AttributeId`.
/// - `handle` must refer to a live `ScratchPad`.
/// - `key`/`value` must adhere to the UTF-8 policy expressed by `utf8_option`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_attribute_str(
    out_attr_id: *mut AttributeId,
    handle: ArcHandle<ScratchPad>,
    dictionary: ArcHandle<ProfilesDictionary>,
    key: CharSlice<'_>,
    value: CharSlice<'_>,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_attr_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let key_str = utf8_option.try_as_bytes_convert(key)?;
        if key_str.is_empty() {
            return Err(ProfileError::InvalidInput);
        }

        let value_str = utf8_option.try_as_bytes_convert(value)?;

        // Intern key string into the dictionary string table
        let dict = dictionary.as_inner()?;
        let key_id = dict.strings().try_insert(key_str.as_ref())?;

        let mut val_writer = FallibleStringWriter::new();
        val_writer.try_push_str(value_str.as_ref())?;
        let val_owned = String::from(val_writer);

        let kv = KeyValue {
            key: key_id,
            value: datadog_profiling::profiles::datatypes::AnyValue::String(
                val_owned,
            ),
        };
        let id = pad.attributes().try_insert(kv)?;
        unsafe { out_attr_id.write(id.into_raw()) };
        Ok(())
    }())
}

/// Inserts an integer attribute and returns its id.
///
/// # Safety
///
/// - `out_attr_id` must be non-null and valid for writes of `AttributeId`.
/// - `handle` must refer to a live `ScratchPad`.
/// - `key` must adhere to the UTF-8 policy expressed by `utf8_option`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_attribute_int(
    out_attr_id: *mut AttributeId,
    handle: ArcHandle<ScratchPad>,
    dictionary: ArcHandle<ProfilesDictionary>,
    key: CharSlice<'_>,
    value: i64,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_attr_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let key_str = utf8_option.try_as_bytes_convert(key)?;
        if key_str.is_empty() {
            return Err(ProfileError::InvalidInput);
        }
        // Intern key string into the dictionary string table
        let dict = dictionary.as_inner()?;
        let key_id = dict.strings().try_insert(key_str.as_ref())?;
        let kv = KeyValue {
            key: key_id,
            value: datadog_profiling::profiles::datatypes::AnyValue::Integer(
                value,
            ),
        };
        let id = pad.attributes().try_insert(kv)?;
        unsafe { out_attr_id.write(id.into_raw()) };
        Ok(())
    }())
}

/// Registers a trace endpoint for a local root span id. Returns its `StringId`.
///
/// # Safety
///
/// - `out_string_id` must be non-null and valid for writes of `StringId`.
/// - `handle` must refer to a live `ScratchPad`.
/// - `endpoint` must adhere to the UTF-8 policy expressed by `utf8_option`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_add_trace_endpoint(
    out_string_id: *mut StringId,
    handle: ArcHandle<ScratchPad>,
    local_root_span_id: i64,
    endpoint: CharSlice<'_>,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_string_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        // Use the same UTF-8 handling helpers as string insertion
        let id = insert_str(
            pad.endpoint_tracker().strings(),
            endpoint,
            utf8_option,
        )?;
        // Now register the mapping and counts
        let _ = pad
            .endpoint_tracker()
            .add_trace_endpoint(local_root_span_id, unsafe {
                pad.endpoint_tracker().strings().get(id)
            })?;
        unsafe { out_string_id.write(id) };
        Ok(())
    }())
}

/// Adds a count to an existing endpoint id.
///
/// # Safety
///
/// - `handle` must refer to a live `ScratchPad`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_add_endpoint_count(
    handle: ArcHandle<ScratchPad>,
    endpoint_id: StringId,
    count: usize,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        pad.endpoint_tracker().add_endpoint_count(endpoint_id, count)
    }())
}

/// Registers a trace endpoint and adds an initial count; returns its id.
///
/// # Safety
///
/// - `out_string_id` must be non-null and valid for writes of `StringId`.
/// - `handle` must refer to a live `ScratchPad`.
/// - `endpoint` must adhere to the UTF-8 policy expressed by `utf8_option`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_add_trace_endpoint_with_count(
    out_string_id: *mut StringId,
    handle: ArcHandle<ScratchPad>,
    local_root_span_id: i64,
    endpoint: CharSlice<'_>,
    utf8_option: Utf8Option,
    count: usize,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_string_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let endpoint_str = utf8_option.try_as_bytes_convert(endpoint)?;
        let id = pad.endpoint_tracker().add_trace_endpoint_with_count(
            local_root_span_id,
            endpoint_str.as_ref(),
            count,
        )?;
        unsafe { out_string_id.write(id) };
        Ok(())
    }())
}

#[derive(thiserror::Error, Debug)]
#[error("trace endpoint not found for local root span id 0x{0:X}")]
struct EndpointNotFound(u64);

/// Returns the endpoint string for `local_root_span_id` if present.
///
/// # Safety
///
/// - `result` must be non-null and valid for writes of `CharSlice<'static>`.
/// - The returned slice borrows from the scratchpad’s internal string table;
///   the caller must ensure the scratchpad outlives any use of `*result`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_get_trace_endpoint_str(
    result: *mut CharSlice<'static>,
    handle: ArcHandle<ScratchPad>,
    local_root_span_id: i64,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(result);
    let Ok(pad) = handle.as_inner() else {
        return ProfileStatus::from(EmptyHandleError::message());
    };
    if let Some(s) =
        pad.endpoint_tracker().get_trace_endpoint_str(local_root_span_id)
    {
        // SAFETY: the lifetime is _not_ safe, it's not static! It's tied to
        // the underlying string set (owned by the ScratchPad). It's up to the
        // FFI to use responsibly.
        let slice = unsafe {
            std::mem::transmute::<CharSlice<'_>, CharSlice<'static>>(
                CharSlice::from(s),
            )
        };
        unsafe { result.write(slice) };
        ProfileStatus::OK
    } else {
        ProfileStatus::from_error(EndpointNotFound(local_root_span_id as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddog_prof_Status_drop;
    use ddcommon_ffi::slice::AsBytes;
    use std::ffi::CStr;

    #[test]
    fn get_endpoint_str_not_found_has_message() {
        unsafe {
            let mut handle = ArcHandle::<ScratchPad>::default();
            Result::from(ddog_prof_ScratchPad_new(&mut handle)).unwrap();

            let mut out = CharSlice::empty();
            let mut status = ddog_prof_ScratchPad_get_trace_endpoint_str(
                &mut out,
                handle,
                u64::MAX as i64,
            );

            let cstr: &CStr =
                (&status).try_into().expect("expected error status");
            let msg = cstr.to_string_lossy();
            assert_eq!(
                msg.as_ref(),
                "trace endpoint not found for local root span id 0xFFFFFFFFFFFFFFFF"
            );

            ddog_prof_ScratchPad_drop(&mut handle);
            ddog_prof_Status_drop(&mut status);
        }
    }

    #[test]
    fn add_and_get_endpoint_str_ok() {
        unsafe {
            let mut handle = ArcHandle::<ScratchPad>::default();
            Result::from(ddog_prof_ScratchPad_new(&mut handle)).unwrap();

            let mut str_id = StringId::default();
            let ep = CharSlice::from("/users/{id}");
            let status = ddog_prof_ScratchPad_add_trace_endpoint(
                &mut str_id,
                handle,
                0x1234,
                ep,
                Utf8Option::Validate,
            );
            Result::from(status).unwrap();

            let mut out = CharSlice::empty();
            let status = ddog_prof_ScratchPad_get_trace_endpoint_str(
                &mut out, handle, 0x1234,
            );
            Result::from(status).unwrap();
            assert_eq!(out.try_to_utf8().unwrap(), "/users/{id}");

            ddog_prof_ScratchPad_drop(&mut handle);
        }
    }
}
