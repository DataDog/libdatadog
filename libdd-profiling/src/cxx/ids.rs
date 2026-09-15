// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ffi;
use crate::api2;
use crate::profiles;

impl Default for ffi::DictionaryStringId {
    fn default() -> Self {
        Self {
            handle: std::ptr::null_mut(),
        }
    }
}

impl Default for ffi::DictionaryFunctionId {
    fn default() -> Self {
        Self {
            handle: std::ptr::null_mut(),
        }
    }
}

impl Default for ffi::DictionaryMappingId {
    fn default() -> Self {
        Self {
            handle: std::ptr::null_mut(),
        }
    }
}

impl ffi::DictionaryStringId {
    pub fn is_null(&self) -> bool {
        self.handle.is_null()
    }
}

impl ffi::DictionaryFunctionId {
    pub fn is_null(&self) -> bool {
        self.handle.is_null()
    }
}

impl ffi::DictionaryMappingId {
    pub fn is_null(&self) -> bool {
        self.handle.is_null()
    }
}

impl From<profiles::datatypes::StringId2> for ffi::DictionaryStringId {
    fn from(id: profiles::datatypes::StringId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid DictionaryStringId handle produced by
/// libdatadog. Null/default handles represent the empty string. For non-null
/// handles, the producing ProfileDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
pub(crate) unsafe fn dictionary_string_id_from_cxx(
    id: &ffi::DictionaryStringId,
) -> profiles::datatypes::StringId2 {
    unsafe { profiles::datatypes::StringId2::from_raw_ptr(id.handle.cast()) }
}

impl From<profiles::datatypes::FunctionId2> for ffi::DictionaryFunctionId {
    fn from(id: profiles::datatypes::FunctionId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid DictionaryFunctionId handle produced by
/// libdatadog. Null/default handles represent the default/unknown function. For
/// non-null handles, the producing ProfileDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
pub(crate) unsafe fn dictionary_function_id_from_cxx(
    id: &ffi::DictionaryFunctionId,
) -> profiles::datatypes::FunctionId2 {
    unsafe { profiles::datatypes::FunctionId2::from_raw_ptr(id.handle.cast()) }
}

impl From<profiles::datatypes::MappingId2> for ffi::DictionaryMappingId {
    fn from(id: profiles::datatypes::MappingId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid DictionaryMappingId handle produced by
/// libdatadog. Null/default handles represent an unknown/no mapping. For
/// non-null handles, the producing ProfileDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
pub(crate) unsafe fn dictionary_mapping_id_from_cxx(
    id: &ffi::DictionaryMappingId,
) -> profiles::datatypes::MappingId2 {
    unsafe { profiles::datatypes::MappingId2::from_raw_ptr(id.handle.cast()) }
}

/// # Safety
///
/// All ids in function must be null/default or valid handles produced by the
/// same ProfileDictionary receiving the function interning operation. Null/default ids
/// represent empty strings.
pub(crate) unsafe fn dictionary_function_from_cxx(
    function: &ffi::DictionaryFunction,
) -> profiles::datatypes::Function2 {
    profiles::datatypes::Function2 {
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // same ProfileDictionary receiving the interning operation. Null/default ids
        // represent empty strings.
        name: unsafe { dictionary_string_id_from_cxx(&function.name) },
        system_name: unsafe { dictionary_string_id_from_cxx(&function.system_name) },
        file_name: unsafe { dictionary_string_id_from_cxx(&function.filename) },
    }
}

/// # Safety
///
/// All ids in mapping must be null/default or valid handles produced by the
/// same ProfileDictionary receiving the mapping interning operation. Null/default ids
/// represent empty strings.
pub(crate) unsafe fn dictionary_mapping_from_cxx(
    mapping: &ffi::DictionaryMapping,
) -> profiles::datatypes::Mapping2 {
    profiles::datatypes::Mapping2 {
        memory_start: mapping.memory_start,
        memory_limit: mapping.memory_limit,
        file_offset: mapping.file_offset,
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // same ProfileDictionary receiving the interning operation. Null/default ids
        // represent empty strings.
        filename: unsafe { dictionary_string_id_from_cxx(&mapping.filename) },
        build_id: unsafe { dictionary_string_id_from_cxx(&mapping.build_id) },
    }
}

/// # Safety
///
/// All ids in location must be null/default or valid handles produced by the
/// ProfileDictionary used to create the receiving Profile. Null/default ids
/// represent unknown values.
pub(crate) unsafe fn dictionary_location_from_cxx(
    location: &ffi::DictionaryLocation,
) -> api2::Location2 {
    api2::Location2 {
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // ProfileDictionary used to create the receiving Profile. Null/default
        // ids represent unknown values.
        mapping: unsafe { dictionary_mapping_id_from_cxx(&location.mapping) },
        function: unsafe { dictionary_function_id_from_cxx(&location.function) },
        address: location.address,
        line: location.line,
    }
}
