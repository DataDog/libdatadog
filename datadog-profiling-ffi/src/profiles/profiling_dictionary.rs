// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arc_handle::ArcHandle;
use crate::profiles::utf8::Utf8Option;
use crate::profiles::{ensure_non_null_insert, ensure_non_null_out_parameter};
use crate::ProfileStatus;
use datadog_profiling::api2::{Function2, FunctionId2, Mapping2, MappingId2, StringId2};
use datadog_profiling::profiles::collections::{SetId, StringRef};
use datadog_profiling::profiles::datatypes::{self as dt, ProfilesDictionary};
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::CharSlice;

/// A StringId that represents the empty string.
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID2_EMPTY: StringId2 = StringId2::EMPTY;

/// A StringId that represents the string "end_timestamp_ns".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID2_END_TIMESTAMP_NS: StringId2 =
    unsafe { core::mem::transmute(StringRef::END_TIMESTAMP_NS) };

/// A StringId that represents the string "local root span id".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID2_LOCAL_ROOT_SPAN_ID: StringId2 =
    unsafe { core::mem::transmute(StringRef::LOCAL_ROOT_SPAN_ID) };

/// A StringId that represents the string "trace endpoint".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID2_TRACE_ENDPOINT: StringId2 =
    unsafe { core::mem::transmute(StringRef::TRACE_ENDPOINT) };

/// A StringId that represents the string "span id".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID2_SPAN_ID: StringId2 =
    unsafe { core::mem::transmute(StringRef::SPAN_ID) };

/// Allocates a new `ProfilesDictionary` and writes a handle to it in `handle`.
///
/// # Safety
///
/// - `handle` must be non-null and valid for writes of `ProfilesDictionaryHandle`.
/// - The returned handle must eventually drop the resource; see
///   [`ddog_prof_ProfilesDictionary_drop`] for more details.
/// - If you need a copy, use [`ddog_prof_ProfilesDictionary_try_clone`]; don't just memcpy a new
///   handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_new(
    handle: *mut ArcHandle<ProfilesDictionary>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = ProfilesDictionary::try_new()?;
        let h = ArcHandle::new(dict)?;
        unsafe { handle.write(h) };
        Ok(())
    }())
}

/// Creates a new handle to the same `ProfilesDictionary` by incrementing the
/// internal reference count.
///
/// # Safety
///
/// - `out` must be non-null and valid for writes of `ProfilesDictionaryHandle`.
/// - `handle` must point to a live dictionary resource.
/// - Do not duplicate handles via memcpy; always use this API to create new handles so the
///   reference count is maintained correctly.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_try_clone(
    out: *mut ArcHandle<ProfilesDictionary>,
    handle: ArcHandle<ProfilesDictionary>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let cloned = handle.try_clone()?;
        unsafe { out.write(cloned) };
        Ok(())
    }())
}

/// Inserts a `Function` into the dictionary and returns its id.
///
/// # Safety
///
/// - `function_id` must be non-null and valid for writes of `FunctionId`.
/// - `handle` must refer to a live dictionary.
/// - `function` must be non-null and point to a valid `Function` for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_insert_function(
    function_id: *mut FunctionId2,
    handle: ArcHandle<ProfilesDictionary>,
    function: *const Function2,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(function_id);
    ensure_non_null_insert!(function);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        let f2: Function2 = unsafe { *function };
        let id = dict.try_insert_function2(f2)?;
        unsafe { function_id.write(id) };
        Ok(())
    }())
}

/// Inserts a `Mapping` into the dictionary and returns its id.
///
/// # Safety
///
/// - `mapping_id` must be non-null and valid for writes of `MappingId`.
/// - `handle` must refer to a live dictionary.
/// - `mapping` must be non-null and point to a valid `Mapping` for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_insert_mapping(
    mapping_id: *mut MappingId2,
    handle: ArcHandle<ProfilesDictionary>,
    mapping: *const Mapping2,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(mapping_id);
    ensure_non_null_insert!(mapping);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        let m2 = unsafe { *mapping };
        let id = dict.try_insert_mapping2(m2)?;
        unsafe { mapping_id.write(id) };
        Ok(())
    }())
}

/// Inserts a UTF-8 string into the dictionary string table.
///
/// # Safety
///
/// - `string_id` must be non-null and valid for writes of `StringId`.
/// - `handle` must refer to a live dictionary.
/// - The UTF-8 policy indicated by `utf8_option` must be respected by caller for the provided
///   `byte_slice`.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_insert_str(
    string_id: *mut StringId2,
    handle: ArcHandle<ProfilesDictionary>,
    byte_slice: CharSlice,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(string_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        crate::profiles::utf8::insert_str(dict.strings(), byte_slice, utf8_option)
            .map(|id| unsafe { string_id.write(id.into()) })
    }())
}

/// Tries to get the string value associated with the string id. Fails if the
/// handle has been taken from, or the result param is null.
///
/// # Safety
///
///  1. The lifetime of the return slice is tied to the underlying storage of the string set, make
///     sure the string set is still alive when using the returned slice.
///  2. The string id should belong to the string set in this dictionary. Well-known strings are an
///     exception, as they exist in every set.
///  3. The handle must represent a live profiles dictionary. Remember handles can be copied, and if
///     _any_ handle drops the resource, then all handles pointing the resource are now invalid,
///     even if though they are unaware of it.
///  4. The result pointer must valid for [`core::ptr::write`].
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_get_str(
    result: *mut CharSlice<'static>,
    handle: ArcHandle<ProfilesDictionary>,
    string_id: StringId2,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(result);
    ProfileStatus::from(handle.as_inner().map(|dict| {
        let string_ref = StringRef::from(string_id);
        // SAFETY: It's not actually safe--as indicated in the docs
        // for this function, the caller needs to be sure the string
        // set in the dictionary outlives the slice.
        result.write(unsafe {
            std::mem::transmute::<CharSlice<'_>, CharSlice<'static>>(CharSlice::from(
                dict.strings().get(string_ref),
            ))
        })
    }))
}

/// Drops the `ProfilesDictionary` that the handle owns, leaving a valid but
/// useless handle (all operations on it will error). This takes a pointer to
/// the handle to be able to modify it to leave behind an empty handle.
///
/// # Safety
///
/// - If non-null, `handle` must point to a valid `ProfilesDictionaryHandle`.
/// - The underlying resource must be dropped exactly once across all copies of the handle. After
///   dropping, all other copies become invalid and must not be used; they should be discarded
///   without dropping.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_drop(
    handle: *mut ArcHandle<ProfilesDictionary>,
) {
    if let Some(h) = handle.as_mut() {
        h.drop_resource();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::utf8::Utf8Option;

    #[test]
    fn test_basics_including_drop() {
        let mut handle = ArcHandle::default();
        unsafe {
            Result::from(ddog_prof_ProfilesDictionary_new(&mut handle)).unwrap();

            let mut string_id = StringId2::default();
            Result::from(ddog_prof_ProfilesDictionary_insert_str(
                &mut string_id,
                handle,
                CharSlice::from("void main(int, char *[])"),
                Utf8Option::Assume,
            ))
            .unwrap();

            let mut function_id = FunctionId2::default();
            let function = Function2 {
                name: string_id,
                system_name: Default::default(),
                file_name: Default::default(),
            };
            Result::from(ddog_prof_ProfilesDictionary_insert_function(
                &mut function_id,
                handle,
                &function,
            ))
            .unwrap();

            let mut found = CharSlice::empty();
            let status = ddog_prof_ProfilesDictionary_get_str(&mut found, handle, string_id);
            Result::from(status).unwrap();

            ddog_prof_ProfilesDictionary_drop(&mut handle);
        }
    }
}
