// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileStatus;
use datadog_profiling::profiles::collections::{
    ParallelStringSet, StringId as RustStringId, StringId,
};
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use std::borrow::Cow;
use std::collections::TryReserveError;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

/// Opaque handle type for a string set. Do not reach into this, it's only
/// there for size and alignment and the detail may change.
pub type StringSet = *mut c_void;

/// Tries to create a new string set.
/// If the status is OK, then the `set` has been written with an actual set
/// handle which will later need to be dropped; otherwise it remains
/// unchanged.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StringSet_new(
    set: NonNull<StringSet>,
) -> ProfileStatus {
    match ParallelStringSet::try_new() {
        Ok(string_set) => unsafe {
            set.write(string_set.into_raw().as_ptr());
            ProfileStatus::OK
        },
        Err(err) => ProfileStatus::from_error(err),
    }
}

/// Options for converting a ByteSlice to a UTF-8 string.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Utf8Option {
    /// The string is assumed to be valid UTF-8. If it's not, the behavior
    /// is undefined.
    Assume,
    /// The string is converted to UTF-8 using lossy conversion.
    ConvertLossy,
    /// The string is validated to be UTF-8. If it's not, an error is
    /// returned.
    Validate,
}

impl Utf8Option {
    /// Converts a byte slice to a UTF-8 string according to the option.
    /// - Assume: Borrow without validation (caller guarantees UTF-8)
    /// - ConvertLossy: Lossy conversion with fallible allocation
    /// - Validate: Validate and borrow on success
    pub unsafe fn convert(
        self,
        bytes: &[u8],
    ) -> Result<Cow<'_, str>, ProfileError> {
        Ok(match self {
            Utf8Option::Assume => {
                // SAFETY: caller asserts validity under Assume
                Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(bytes) })
            }
            Utf8Option::ConvertLossy => try_from_utf8_lossy(bytes)
                .map_err(|_| ProfileError::OutOfMemory)?,
            Utf8Option::Validate => Cow::Borrowed(
                std::str::from_utf8(bytes)
                    .map_err(|_| ProfileError::InvalidInput)?,
            ),
        })
    }

    pub unsafe fn try_as_bytes_convert<'a, T: AsBytes<'a>>(
        self,
        t: T,
    ) -> Result<Cow<'a, str>, ProfileError> {
        let bytes = t.try_as_bytes().ok_or(ProfileError::InvalidInput)?;
        self.convert(bytes)
    }
}

/// Tries to convert a slice of bytes to a string. The input may have invalid
/// characters.
///
/// This is the same implementation as [`String::from_utf8_lossy`] except that
/// this uses fallible allocations.
pub fn try_from_utf8_lossy(v: &[u8]) -> Result<Cow<'_, str>, TryReserveError> {
    let mut iter = v.utf8_chunks();

    let first_valid = if let Some(chunk) = iter.next() {
        let valid = chunk.valid();
        if chunk.invalid().is_empty() {
            debug_assert_eq!(valid.len(), v.len());
            return Ok(Cow::Borrowed(valid));
        }
        valid
    } else {
        return Ok(Cow::Borrowed(""));
    };

    const REPLACEMENT: &str = "\u{FFFD}";
    const REPLACEMENT_LEN: usize = REPLACEMENT.len();

    let mut res = String::new();
    res.try_reserve(v.len())?;
    res.push_str(first_valid);
    res.try_reserve(REPLACEMENT_LEN)?;
    res.push_str(REPLACEMENT);

    for chunk in iter {
        let valid = chunk.valid();
        res.try_reserve(valid.len())?;
        res.push_str(valid);
        if !chunk.invalid().is_empty() {
            res.try_reserve(REPLACEMENT_LEN)?;
            res.push_str(REPLACEMENT);
        }
    }

    Ok(Cow::Owned(res))
}

pub fn insert_str(
    set: &ParallelStringSet,
    str: CharSlice<'_>,
    utf8_options: Utf8Option,
) -> Result<StringId, ProfileError> {
    let bytes = str.try_as_bytes().ok_or(ProfileError::InvalidInput)?;
    let string = match utf8_options {
        Utf8Option::Assume => {
            // SAFETY: the caller is asserting the data is valid UTF-8.
            Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(bytes) })
        }
        Utf8Option::ConvertLossy => try_from_utf8_lossy(bytes)?,
        Utf8Option::Validate => Cow::Borrowed(
            std::str::from_utf8(bytes)
                .map_err(|_| ProfileError::InvalidInput)?,
        ),
    };
    Ok(set.try_insert(string.as_ref())?)
}

fn stringset_insert(
    this: StringSet,
    str: CharSlice,
    utf8_options: Utf8Option,
) -> Result<RustStringId, ProfileError> {
    let set = match NonNull::new(this) {
        None => return Err(ProfileError::InvalidInput),
        // todo: run with miri to see if this is okay to call multiple times.
        Some(raw) => {
            ManuallyDrop::new(unsafe { ParallelStringSet::from_raw(raw) })
        }
    };
    insert_str(&set, str, utf8_options)
}

/// Tries to insert a string into the set.
/// If the status is OK, then the `id` has been written with an actual string
/// id; otherwise it remains unchanged.

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StringSet_insert(
    id: NonNull<StringId>,
    set: StringSet,
    str: CharSlice,
    utf8_options: Utf8Option,
) -> ProfileStatus {
    match stringset_insert(set, str, utf8_options) {
        Ok(string_id) => {
            unsafe { id.write(string_id) };
            ProfileStatus::OK
        }
        Err(err) => ProfileStatus::from_error(err),
    }
}

/// Drops the string set. The caller must not use the string set anymore.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StringSet_drop(set: StringSet) {
    if let Some(raw) = NonNull::new(set) {
        drop(unsafe { ParallelStringSet::from_raw(raw) });
    }
}
