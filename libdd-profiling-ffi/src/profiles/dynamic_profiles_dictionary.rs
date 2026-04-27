// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arc_handle::ArcHandle;
use crate::profile_status::ProfileStatus;
use crate::profiles::dynamic::{DynamicFunction, DynamicFunctionIndex, DynamicStringIndex};
use crate::profiles::utf8::{Utf8ConversionError, Utf8Option};
use crate::profiles::{ensure_non_null_insert, ensure_non_null_out_parameter};
use crate::ProfileError;
use libdd_common::error::FfiSafeErrorMessage;
use libdd_common_ffi::slice::{CharSlice, Slice};
use libdd_common_ffi::MutSlice;
use libdd_profiling::dynamic::DynamicProfilesDictionary;
use std::borrow::Cow;
use std::ffi::CStr;

const NULL_DYNAMIC_PROFILES_DICTIONARY: &CStr =
    c"passed a null pointer for a DynamicProfilesDictionary";

fn convert_utf8(
    byte_slice: CharSlice<'_>,
    utf8_option: Utf8Option,
) -> Result<Cow<'_, str>, ProfileError> {
    unsafe { utf8_option.try_as_bytes_convert(byte_slice) }.map_err(|err| match err {
        Utf8ConversionError::OutOfMemory(err) => ProfileError::from(err),
        Utf8ConversionError::SliceConversionError(err) => ProfileError::from(err.as_ffi_str()),
        Utf8ConversionError::Utf8Error(_) => ProfileError::from(
            c"tried to insert a non-UTF8 string into a DynamicProfilesDictionary",
        ),
    })
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_new(
    handle: *mut ArcHandle<DynamicProfilesDictionary>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = DynamicProfilesDictionary::try_new().map_err(ProfileError::from_display)?;
        let h = ArcHandle::new(dict)?;
        unsafe { handle.write(h) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_try_clone(
    out: *mut ArcHandle<DynamicProfilesDictionary>,
    handle: ArcHandle<DynamicProfilesDictionary>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(out);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let cloned = handle.try_clone()?;
        unsafe { out.write(cloned) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_insert_function(
    function_id: *mut DynamicFunctionIndex,
    dict: Option<&DynamicProfilesDictionary>,
    function: *const DynamicFunction,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(function_id);
    ensure_non_null_insert!(function);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = dict.ok_or(NULL_DYNAMIC_PROFILES_DICTIONARY)?;
        let function = unsafe { *function };
        let id = dict
            .try_insert_function(function.into())
            .map_err(ProfileError::from_display)?;
        unsafe { function_id.write(id.into()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_insert_str(
    string_id: *mut DynamicStringIndex,
    dict: Option<&DynamicProfilesDictionary>,
    byte_slice: CharSlice,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(string_id);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = dict.ok_or(NULL_DYNAMIC_PROFILES_DICTIONARY)?;
        let string = convert_utf8(byte_slice, utf8_option)?;
        let id = dict
            .try_insert_str(string.as_ref())
            .map_err(ProfileError::from_display)?;
        unsafe { string_id.write(id.into()) };
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_insert_strs(
    mut string_ids: MutSlice<DynamicStringIndex>,
    dict: Option<&DynamicProfilesDictionary>,
    strings: Slice<CharSlice>,
    utf8_option: Utf8Option,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let dict = dict.ok_or(NULL_DYNAMIC_PROFILES_DICTIONARY)?;
        if strings.len() != string_ids.len() {
            return Err(ProfileError::from(
                c"input strings slice and output DynamicStringIndex slice have different lengths",
            ));
        }
        for (byte_slice, id_out) in strings.iter().zip(string_ids.as_mut_slice().iter_mut()) {
            let string = convert_utf8(*byte_slice, utf8_option)?;
            let id = dict
                .try_insert_str(string.as_ref())
                .map_err(ProfileError::from_display)?;
            *id_out = id.into();
        }
        Ok(())
    }())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_get_str(
    result: *mut CharSlice<'static>,
    dict: Option<&DynamicProfilesDictionary>,
    string_id: DynamicStringIndex,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(result);
    let Some(dict) = dict else {
        return ProfileStatus::from(NULL_DYNAMIC_PROFILES_DICTIONARY);
    };
    result.write(unsafe {
        std::mem::transmute::<CharSlice<'_>, CharSlice<'static>>(CharSlice::from(
            dict.get_str(string_id.into()),
        ))
    });
    ProfileStatus::OK
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_get_func(
    result: *mut DynamicFunction,
    dict: Option<&DynamicProfilesDictionary>,
    function_id: DynamicFunctionIndex,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(result);
    let Some(dict) = dict else {
        return ProfileStatus::from(NULL_DYNAMIC_PROFILES_DICTIONARY);
    };
    unsafe { result.write(dict.get_func(function_id.into()).into()) };
    ProfileStatus::OK
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_DynamicProfilesDictionary_drop(
    handle: *mut ArcHandle<DynamicProfilesDictionary>,
) {
    if let Some(handle) = unsafe { handle.as_mut() } {
        handle.drop_resource();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common_ffi::slice::AsBytes;

    #[test]
    fn ffi_string_round_trip() {
        unsafe {
            let mut handle = ArcHandle::<DynamicProfilesDictionary>::default();
            Result::<(), _>::from(ddog_prof_DynamicProfilesDictionary_new(&mut handle)).unwrap();
            let dict = handle.as_inner().ok();

            let mut id = DynamicStringIndex::default();
            Result::<(), _>::from(ddog_prof_DynamicProfilesDictionary_insert_str(
                &mut id,
                dict,
                CharSlice::from("hello"),
                Utf8Option::Validate,
            ))
            .unwrap();

            let mut found = CharSlice::empty();
            Result::<(), _>::from(ddog_prof_DynamicProfilesDictionary_get_str(
                &mut found, dict, id,
            ))
            .unwrap();
            assert_eq!(found.try_as_bytes().unwrap(), b"hello");
            ddog_prof_DynamicProfilesDictionary_drop(&mut handle);
        }
    }
}
