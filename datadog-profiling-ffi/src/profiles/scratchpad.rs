// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_handle::ProfileHandle;
use crate::profiles::{
    ensure_non_null_insert, ensure_non_null_out_parameter, insert_str,
    Utf8Option,
};
use crate::{EmptyHandleError, ProfileStatus};
use datadog_profiling::profiles::collections::{SetId, StringId};
use datadog_profiling::profiles::datatypes::{
    AttributeId, KeyValue, Link, LinkId, Location, LocationId, ScratchPad,
    StackId,
};
use datadog_profiling::profiles::string_writer::FallibleStringWriter;
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::CharSlice;
use std::borrow::Cow;
use std::ffi::c_void;

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_new(
    handle: *mut ProfileHandle<ScratchPad>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(ScratchPad::try_new().and_then(
        |pad| -> Result<(), ProfileError> {
            let h = ProfileHandle::try_new(pad)?;
            unsafe { handle.write(h) };
            Ok(())
        },
    ))
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_drop(
    handle: *mut ProfileHandle<ScratchPad>,
) {
    if let Some(pad) = handle.as_mut() {
        drop(pad.take());
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_location(
    out_location_id: *mut LocationId,
    handle: ProfileHandle<ScratchPad>,
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

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_stack(
    out_stack_id: *mut StackId,
    handle: ProfileHandle<ScratchPad>,
    locations: ddcommon_ffi::Slice<'_, LocationId>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_stack_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let slice = locations.as_slice();
        // Convert LocationId (NonNull<c_void>) to SetId<Location>
        let mut ids: Vec<SetId<Location>> = Vec::new();
        ids.try_reserve(slice.len()).map_err(|_| ProfileError::OutOfMemory)?;
        for id in slice {
            let sid =
                unsafe { SetId::<c_void>::from_raw(*id).cast::<Location>() };
            ids.push(sid);
        }
        let stack_id = pad.stacks().try_insert(&ids)?;
        unsafe { out_stack_id.write(stack_id) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_link(
    out_link_id: *mut LinkId,
    handle: ProfileHandle<ScratchPad>,
    link: *const Link,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_link_id);
    ensure_non_null_insert!(link);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        let id = pad.links().try_insert(unsafe { *link })?;
        unsafe { out_link_id.write(id.into_raw()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_attribute_str(
    out_attr_id: *mut AttributeId,
    handle: ProfileHandle<ScratchPad>,
    key: CharSlice<'_>,
    value: CharSlice<'_>,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out_attr_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        // Build KeyValue using fallible string writer, respecting utf8_option
        let key_str = utf8_option.try_as_bytes_convert(key)?;
        if key_str.is_empty() {
            return Err(ProfileError::InvalidInput);
        }

        let value_str = utf8_option.try_as_bytes_convert(value)?;

        let mut key_writer = FallibleStringWriter::new();
        key_writer.try_reserve(key_str.len())?;
        key_writer.try_push_str(key_str.as_ref())?;
        let key_cow: Cow<'static, str> = Cow::Owned(String::from(key_writer));

        let mut val_writer = FallibleStringWriter::new();
        val_writer.try_push_str(value_str.as_ref())?;
        let val_owned = String::from(val_writer);

        let kv = KeyValue {
            key: key_cow,
            value: datadog_profiling::profiles::datatypes::AnyValue::String(
                val_owned,
            ),
        };
        let id = pad.attributes().try_insert(kv)?;
        unsafe { out_attr_id.write(id.into_raw()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_insert_attribute_int(
    out_attr_id: *mut AttributeId,
    handle: ProfileHandle<ScratchPad>,
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
        let mut key_writer = FallibleStringWriter::new();
        key_writer.try_push_str(key_str.as_ref())?;
        let key_cow: Cow<'static, str> = Cow::Owned(String::from(key_writer));
        let kv = KeyValue {
            key: key_cow,
            value: datadog_profiling::profiles::datatypes::AnyValue::Integer(
                value,
            ),
        };
        let id = pad.attributes().try_insert(kv)?;
        unsafe { out_attr_id.write(id.into_raw()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_add_trace_endpoint(
    out_string_id: *mut StringId,
    handle: ProfileHandle<ScratchPad>,
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

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_add_endpoint_count(
    handle: ProfileHandle<ScratchPad>,
    endpoint_id: StringId,
    count: usize,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let pad = handle.as_inner()?;
        pad.endpoint_tracker().add_endpoint_count(endpoint_id, count)
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_add_trace_endpoint_with_count(
    out_string_id: *mut StringId,
    handle: ProfileHandle<ScratchPad>,
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

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ScratchPad_get_trace_endpoint_str(
    result: *mut CharSlice<'static>,
    handle: ProfileHandle<ScratchPad>,
    local_root_span_id: i64,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(result);
    let Ok(pad) = handle.as_inner() else {
        return ProfileStatus::from(EmptyHandleError::message());
    };
    if let Some(s) =
        pad.endpoint_tracker().get_trace_endpoint_str(local_root_span_id)
    {
        unsafe { result.write(std::mem::transmute(CharSlice::from(s))) };
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
            let mut handle = ProfileHandle::default();
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
            let mut handle = ProfileHandle::default();
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
