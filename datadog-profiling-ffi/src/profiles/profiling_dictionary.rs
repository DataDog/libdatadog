// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_handle::ProfileHandle;
use crate::profiles::Utf8Option;
use crate::profiles::{ensure_non_null_insert, ensure_non_null_out_parameter};
use crate::ProfileStatus;
use datadog_profiling::profiles::collections::StringId;
use datadog_profiling::profiles::datatypes::{
    Function, FunctionId, Mapping, MappingId, ProfilesDictionary,
};
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::CharSlice;

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_new(
    handle: *mut ProfileHandle<ProfilesDictionary>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(ProfilesDictionary::try_new().and_then(
        |dict| -> Result<(), ProfileError> {
            let h = ProfileHandle::try_new(dict)?;
            unsafe { handle.write(h) };
            Ok(())
        },
    ))
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_insert_function(
    function_id: *mut FunctionId,
    handle: ProfileHandle<ProfilesDictionary>,
    function: *const Function,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(function_id);
    ensure_non_null_insert!(function);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        let id = dict.functions().try_insert(*function)?;
        unsafe { function_id.write(id.into_raw()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_insert_mapping(
    mapping_id: *mut MappingId,
    handle: ProfileHandle<ProfilesDictionary>,
    mapping: *const Mapping,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(mapping_id);
    ensure_non_null_insert!(mapping);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        let id = dict.mappings().try_insert(*mapping)?;
        unsafe { mapping_id.write(id.into_raw()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_insert_str(
    string_id: *mut StringId,
    handle: ProfileHandle<ProfilesDictionary>,
    byte_slice: CharSlice,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(string_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = handle.as_inner()?;
        crate::profiles::insert_str(dict.strings(), byte_slice, utf8_option)
            .map(|id| unsafe { string_id.write(id) })
    }())
}

// todo: add ddog_prof_ProfilesDictionary_insert_str_utf16 and learn about the
//       necessary nuances such as endianness and byte-order marks. Adding
//       this to the API allows for unified handling of the errors, including
//       checking for allocator failures.

/// Tries to get the string value associated with the string id. Fails if the
/// handle has been taken from, or the result param is null.
///
/// # Safety
///
///  1. The lifetime of the return slice is tied to the underlying storage of
///     the string set, make sure the string set is still alive when using the
///     returned slice.
///  2. The string id should belong to the string set in this dictionary.
///     Well-known strings are an exception, as they exist in every set.
///  3. The handle must represent a live profiles dictionary. Remember handles
///     can be copied, and if _any_ handle drops the resource, then all handles
///     pointing the resource are now invalid, even if though they are unaware
///     of it.
///  4. The result pointer must valid for [`core::ptr::write`].
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_get_str(
    result: *mut CharSlice<'static>,
    handle: ProfileHandle<ProfilesDictionary>,
    string_id: StringId,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(result);
    ProfileStatus::from(handle.as_inner().map(|dict| {
        // SAFETY: It's not actually safe--as indicated in the docs
        // for this function, the caller needs to be sure the string
        // set in the dictionary outlives the slice.
        result.write(unsafe {
            std::mem::transmute(CharSlice::from(dict.strings().get(string_id)))
        })
    }))
}

/// Drops the ProfilesDictionary that the handle owns, leaving a valid but
/// useless handle (all operations on it will error). This takes a pointer to
/// the handle to be able to modify it to leave behind an empty handle.
///
/// # Safety
///
/// If there are other handles to the same ProfilesDictionary, they are now
/// invalid and should be discarded without dropping them.
///
/// All handles and ids to data inside this dictionary should be discarded,
/// as they are now invalid.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfilesDictionary_drop(
    handle: *mut ProfileHandle<ProfilesDictionary>,
) {
    // Semantically, the handle has not been dropped, only its contents.
    if let Some(dict) = handle.as_mut() {
        drop(dict.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::Utf8Option;
    use std::ptr::NonNull;

    #[test]
    fn test_basics_including_drop() {
        let mut handle = ProfileHandle::default();
        unsafe {
            Result::from(ddog_prof_ProfilesDictionary_new(&mut handle))
                .unwrap();

            let mut string_id = StringId::default();
            Result::from(ddog_prof_ProfilesDictionary_insert_str(
                &mut string_id,
                handle,
                CharSlice::from("void main(int, char *[])"),
                Utf8Option::Assume,
            ))
            .unwrap();

            let mut function_id = NonNull::dangling();
            let function = Function {
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
            let status = ddog_prof_ProfilesDictionary_get_str(
                &mut found, handle, string_id,
            );
            Result::from(status).unwrap();

            ddog_prof_ProfilesDictionary_drop(&mut handle);
        }
    }
}
