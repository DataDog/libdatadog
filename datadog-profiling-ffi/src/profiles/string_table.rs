// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling::collections::string_table::StringTable;
use datadog_profiling::{ProfileError, ProfileVoidResult};
use datadog_profiling_protobuf::StringOffset;
use ddcommon_ffi::{CharSlice, MutSlice, Slice};
use std::{borrow, ptr, slice, str};

// Well-known string offset constants
/// String offset for "end_timestamp_ns" in string tables
#[no_mangle]
pub static DDOG_PROF_STRING_TABLE_END_TIMESTAMP_NS_OFFSET: StringOffset =
    StringTable::END_TIMESTAMP_NS_OFFSET;

/// String offset for "local root span id" in string tables  
#[no_mangle]
pub static DDOG_PROF_STRING_TABLE_LOCAL_ROOT_SPAN_ID_OFFSET: StringOffset =
    StringTable::LOCAL_ROOT_SPAN_ID_OFFSET;

/// String offset for "trace endpoint" in string tables
#[no_mangle]
pub static DDOG_PROF_STRING_TABLE_TRACE_ENDPOINT_OFFSET: StringOffset =
    StringTable::TRACE_ENDPOINT_OFFSET;

/// String offset for "span id" in string tables
#[no_mangle]
pub static DDOG_PROF_STRING_TABLE_SPAN_ID_OFFSET: StringOffset =
    StringTable::SPAN_ID_OFFSET;

#[repr(C)]
pub enum StringTableNewResult {
    Ok(*mut StringTable),
    Err(ProfileError),
}

impl From<StringTableNewResult> for Result<*mut StringTable, ProfileError> {
    fn from(result: StringTableNewResult) -> Self {
        match result {
            StringTableNewResult::Ok(string_table) => Ok(string_table),
            StringTableNewResult::Err(err) => Err(err),
        }
    }
}

/// Tries to create a new string table.
///
/// # Errors
///
/// Fails if the string table or its data structures fail to allocate memory.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_StringTable_new() -> StringTableNewResult {
    match StringTable::try_new() {
        Ok(string_table) => match datadog_alloc::Box::try_new(string_table) {
            Ok(boxed) => {
                StringTableNewResult::Ok(datadog_alloc::Box::into_raw(boxed))
            }
            Err(_) => StringTableNewResult::Err(ProfileError::OutOfMemory),
        },
        Err(err) => StringTableNewResult::Err(err.into()),
    }
}

#[repr(C)]
pub enum StringTableInternResult {
    Ok(StringOffset),
    Err(ProfileError),
}

impl From<StringTableInternResult> for Result<StringOffset, ProfileError> {
    fn from(result: StringTableInternResult) -> Self {
        match result {
            StringTableInternResult::Ok(ok) => Ok(ok),
            StringTableInternResult::Err(err) => Err(err),
        }
    }
}

/// Interns the `str` without checking if the string is utf8.
///
/// # Errors
///
/// See [`ddog_prof_StringTable_intern`] for errors.
///
/// # Safety
/// The `str` must be valid UTF-8; see [`ddog_prof_StringTable_intern`] for
/// more safety conditions.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_StringTable_intern_utf8(
    string_table: *mut StringTable,
    str: CharSlice,
) -> StringTableInternResult {
    ddog_prof_StringTable_intern(string_table, str, true)
}

/// Interns the `str`, converting it to utf8. It will do a lossy conversion if
/// needed. If it's already known to be utf8, and you are sure enough to skip
/// the check, then use [ddog_prof_StringTable_intern_utf8] instead for better
/// performance.
///
/// # Errors
///
/// See [`ddog_prof_StringTable_intern`] for errors.
///
/// # Safety
///
/// See [`ddog_prof_StringTable_intern`] for safety conditions.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_StringTable_intern_bytes(
    string_table: *mut StringTable,
    str: CharSlice,
) -> StringTableInternResult {
    ddog_prof_StringTable_intern(string_table, str, false)
}

/// Interns the `ffi_slice`. If `assume_utf8` is true, then `ffi_slice` is
/// assumed to be valid UTF-8; otherwise it is lossily converted.
///
/// # Errors
///
///  1. Fails if `string_table` is a null pointer.
///  2. Fails if `ffi_slice` is an invalid slice. Note that there are safety
///     requirements that need to be kept that can't be checked at runtime.
///  3. Fails if the string table fails to allocate memory.
///
/// # Safety
///
///  1. The `string_table` ptr must point to a valid string table object if
///     it's not null.
///  2. The `ffi_slice` needs to uphold all the slice invariants.
///  3. If `assume_utf8` is true, then `ffi_slice` must be valid UTF-8.
#[inline(never)]
#[no_mangle]
#[must_use]
unsafe extern "C" fn ddog_prof_StringTable_intern(
    string_table: *mut StringTable,
    ffi_slice: CharSlice,
    assume_utf8: bool,
) -> StringTableInternResult {
    let Some(slice) = ffi_slice.try_as_slice() else {
        return StringTableInternResult::Err(ProfileError::InvalidInput);
    };
    let bytes = slice::from_raw_parts(slice.as_ptr().cast(), slice.len());
    let utf8 = if assume_utf8 {
        borrow::Cow::Borrowed(str::from_utf8_unchecked(bytes))
    } else {
        String::from_utf8_lossy(bytes)
    };

    if let Some(string_table) = string_table.as_mut() {
        match string_table.try_intern(utf8.as_ref()) {
            Ok(offset) => StringTableInternResult::Ok(offset),
            Err(err) => StringTableInternResult::Err(err.into()),
        }
    } else {
        StringTableInternResult::Err(ProfileError::InvalidInput)
    }
}

unsafe fn ffi_drop(string_table: *mut *mut StringTable) {
    if string_table.is_null() || (*string_table).is_null() {
        return;
    }

    drop(Box::from_raw(*string_table));
    *string_table = ptr::null_mut();
}

/// Drops a string table.
///
/// After this call, the StringTable object remains valid but useless--all
/// operations except another drop will fail after this.
///
/// # Safety
///
///  1. The `string_table` ptr must point to a valid string table object if it's not null, and the
///     string table's internal pointer should only be manipulated through the StringTable FFI
///     functions.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StringTable_drop(
    string_table: *mut *mut StringTable,
) {
    ffi_drop(string_table);
}

/// Clears the string table back to the containing only the empty string.
///
/// This shrinks the capacity of the string table, releases the arena, and
/// makes a new one.
///
/// # Safety
///
///  1. The `string_table` ptr must point to a valid string table object if
///     it's not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StringTable_clear(
    string_table: *mut StringTable,
) {
    if let Some(st) = string_table.as_mut() {
        st.clear();
    }
}

#[repr(C)]
#[derive(Debug)]
pub enum StringTableLookupResult<'a> {
    Ok(CharSlice<'a>),
    Err(ProfileError),
}

/// Tries to return the string associated with the string offset.
///
/// # Errors
///
/// Returns NotFound if the string cannot be found.
///
/// # Safety
///  1. `string_table` _must_ be a valid reference!
///  2. The lifetime of the returned `CharSlice` is bound to the underlying
///     string storage. The string table must not be dropped, cleared, nor
///     new strings interned while a `CharSlice` returned by lookup lives.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_StringTable_lookup(
    string_table: &mut StringTable,
    offset: StringOffset,
) -> StringTableLookupResult {
    match string_table.lookup(offset) {
        Ok(str) => StringTableLookupResult::Ok(str.into()),
        Err(err) => StringTableLookupResult::Err(err.into()),
    }
}

/// Tries to copy the provided strings from one string table into another.
///
/// If this succeeds, then `to` will be fully initialized.
///
/// # Errors
///  1. Returns OutOfMemory if `dst` needs to allocate and cannot.
///  2. Returns NotFound if any of the string offsets in `from` cannot be found
///     in `src`.
///  3. Returns StorageFull if a new string offset wouldn't fit in 32 bits.
///  4. Returns InvalidInput for bad inputs such as null string tables,
///     unaligned pointers for slices, bad slice sizes, and if the lengths of
///     `from` and `to` are not equal.
///
/// # Safety
///
///  1. `dst` and `src` need to point to valid StringTable objects if they are
///     not null.
///  2. If `to.len` is greater than 0, `to.ptr` must be valid for `to.len`
///     consecutive writes.
///  3. If `from.len` is greater than 0, `from.ptr` must be valid for
///     `from.len` consecutive reads.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_StringTable_insert_from(
    dst: *mut StringTable,
    src: *const StringTable,
    to: MutSlice<StringOffset>,
    from: Slice<StringOffset>,
) -> ProfileVoidResult {
    let Some(dst) = dst.as_mut() else {
        return ProfileError::InvalidInput.into();
    };
    let Some(src) = src.as_ref() else {
        return ProfileError::InvalidInput.into();
    };

    let Some(from) = from.try_as_slice() else {
        return ProfileError::InvalidInput.into();
    };
    let Some(to) = to.try_as_uninit() else {
        return ProfileError::InvalidInput.into();
    };

    if to.len() != from.len() {
        return ProfileError::InvalidInput.into();
    }

    match dst.insert_from(src, to, from) {
        Ok(_) => ProfileVoidResult::Ok,
        Err(err) => ProfileVoidResult::Err(err.into()),
    }
}
