// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arc_handle::ArcHandle2;
use crate::profiles::utf8::Utf8Option;
use crate::profiles::{ensure_non_null_insert, ensure_non_null_out_parameter};
use crate::ProfileStatus2;
use datadog_profiling2::profiles::collections::StringId2;
use datadog_profiling2::profiles::datatypes::{
    Function2, FunctionId2, Mapping2, MappingId2, ProfilesDictionary2,
};
use datadog_profiling2::profiles::ProfileError;
use ddcommon_ffi::CharSlice;

/// A StringId that represents the empty string.
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID_EMPTY: StringId2 = StringId2::EMPTY;

/// A StringId that represents the string "end_timestamp_ns".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID_END_TIMESTAMP_NS: StringId2 = StringId2::END_TIMESTAMP_NS;

/// A StringId that represents the string "local root span id".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID_LOCAL_ROOT_SPAN_ID: StringId2 = StringId2::LOCAL_ROOT_SPAN_ID;

/// A StringId that represents the string "trace endpoint".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID_TRACE_ENDPOINT: StringId2 = StringId2::TRACE_ENDPOINT;

/// A StringId that represents the string "span id".
/// This is always available in every string set and can be used without
/// needing to insert it into a string set.
#[no_mangle]
pub static DDOG_PROF_STRINGID_SPAN_ID: StringId2 = StringId2::SPAN_ID;

/// Allocates a new `ProfilesDictionary` and writes a handle to it in `handle`.
///
/// # Safety
///
/// - `handle` must be non-null and valid for writes of `ProfilesDictionaryHandle`.
/// - The returned handle must eventually drop the resource; see
///   [`ddog_prof2_ProfilesDictionary_drop`] for more details.
/// - If you need a copy, use [`ddog_prof2_ProfilesDictionary_try_clone`]; don't just memcpy a new
///   handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_new(
    handle: *mut ArcHandle2<ProfilesDictionary2>,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let dict = ProfilesDictionary2::try_new()?;
        let h = ArcHandle2::new(dict)?;
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
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_try_clone(
    out: *mut ArcHandle2<ProfilesDictionary2>,
    handle: ArcHandle2<ProfilesDictionary2>,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(out);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
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
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_insert_function(
    function_id: *mut FunctionId2,
    handle: ArcHandle2<ProfilesDictionary2>,
    function: *const Function2,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(function_id);
    ensure_non_null_insert!(function);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        let id = dict.functions().try_insert(*function)?;
        unsafe { function_id.write(id.into_raw()) };
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
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_insert_mapping(
    mapping_id: *mut MappingId2,
    handle: ArcHandle2<ProfilesDictionary2>,
    mapping: *const Mapping2,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(mapping_id);
    ensure_non_null_insert!(mapping);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        let id = dict.mappings().try_insert(*mapping)?;
        unsafe { mapping_id.write(id.into_raw()) };
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
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_insert_str(
    string_id: *mut StringId2,
    handle: ArcHandle2<ProfilesDictionary2>,
    byte_slice: CharSlice,
    utf8_option: Utf8Option,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(string_id);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        crate::profiles::utf8::insert_str(dict.strings(), byte_slice, utf8_option)
            .map(|id| unsafe { string_id.write(id) })
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
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_get_str(
    result: *mut CharSlice<'static>,
    handle: ArcHandle2<ProfilesDictionary2>,
    string_id: StringId2,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(result);
    ProfileStatus2::from(handle.as_inner().map(|dict| {
        // SAFETY: It's not actually safe--as indicated in the docs
        // for this function, the caller needs to be sure the string
        // set in the dictionary outlives the slice.
        result.write(unsafe {
            std::mem::transmute::<CharSlice<'_>, CharSlice<'static>>(CharSlice::from(
                dict.strings().get(string_id),
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
pub unsafe extern "C" fn ddog_prof2_ProfilesDictionary_drop(
    handle: *mut ArcHandle2<ProfilesDictionary2>,
) {
    if let Some(h) = handle.as_mut() {
        h.drop_resource();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::utf8::Utf8Option;
    use std::ptr::NonNull;

    #[test]
    fn test_basics_including_drop() {
        let mut handle = ArcHandle2::default();
        unsafe {
            Result::from(ddog_prof2_ProfilesDictionary_new(&mut handle)).unwrap();

            let mut string_id = StringId2::default();
            Result::from(ddog_prof2_ProfilesDictionary_insert_str(
                &mut string_id,
                handle,
                CharSlice::from("void main(int, char *[])"),
                Utf8Option::Assume,
            ))
            .unwrap();

            let mut function_id = NonNull::dangling();
            let function = Function2 {
                name: string_id,
                system_name: Default::default(),
                file_name: Default::default(),
            };
            Result::from(ddog_prof2_ProfilesDictionary_insert_function(
                &mut function_id,
                handle,
                &function,
            ))
            .unwrap();

            let mut found = CharSlice::empty();
            let status = ddog_prof2_ProfilesDictionary_get_str(&mut found, handle, string_id);
            Result::from(status).unwrap();

            ddog_prof2_ProfilesDictionary_drop(&mut handle);
        }
    }
}
