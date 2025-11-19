// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringRef;
use crate::profiles::datatypes::StringId2;

/// A representation of a mapping that is an intersection of the Otel and Pprof
/// representations. Omits boolean attributes because Datadog doesn't use them
/// in any way.
///
/// This representation is used internally by the `ProfilesDictionary`, and
/// utilizes the fact that `StringRef`s don't have null values.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mapping {
    pub memory_start: u64,
    pub memory_limit: u64,
    pub file_offset: u64,
    pub filename: StringRef,
    pub build_id: StringRef, // missing in Otel, is it made into an attribute?
}

/// An FFI-safe version of the Mapping which allows null.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Mapping2 {
    pub memory_start: u64,
    pub memory_limit: u64,
    pub file_offset: u64,
    pub filename: StringId2,
    pub build_id: StringId2, // missing in Otel, is it made into an attribute?
}

impl From<Mapping2> for Mapping {
    fn from(m2: Mapping2) -> Self {
        Self {
            memory_start: m2.memory_start,
            memory_limit: m2.memory_limit,
            file_offset: m2.file_offset,
            filename: m2.filename.into(),
            build_id: m2.build_id.into(),
        }
    }
}

impl From<Mapping> for Mapping2 {
    fn from(m: Mapping) -> Self {
        Self {
            memory_start: m.memory_start,
            memory_limit: m.memory_limit,
            file_offset: m.file_offset,
            filename: m.filename.into(),
            build_id: m.build_id.into(),
        }
    }
}

/// An FFI-safe representation of a "handle" to a mapping which has been
/// stored in the `ProfilesDictionary`. The representation is ensured to be a
/// pointer for ABI stability, but callers should not generally dereference
/// this pointer. When using the id, the caller needs to be sure that the
/// `ProfilesDictionary` it refers to is the same one that the operations are
/// performed on; it is not generally guaranteed that ids from one dictionary
/// can be used in another dictionary, even if it happens to work by
/// implementation detail.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct MappingId2(pub(crate) *mut Mapping2);

// todo: when MSRV is 1.88.0+, derive Default
impl Default for MappingId2 {
    fn default() -> Self {
        Self(core::ptr::null_mut())
    }
}

impl MappingId2 {
    pub fn is_empty(self) -> bool {
        self.0.is_null()
    }

    /// Converts the `MappingId2` into an `Option<Mapping2>` where an empty
    /// `MappingId2` converts to a `None`.
    ///
    /// # Safety
    /// The pointer object must still be alive. In general this means the
    /// profiles dictionary it came from must be alive.
    pub unsafe fn read(self) -> Option<Mapping2> {
        if self.is_empty() {
            None
        } else {
            Some(self.0.read())
        }
    }
}
