// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{SetId, StringRef};
use crate::profiles::datatypes::StringId2;

/// A representation of a mapping that is an intersection of the Otel and Pprof
/// representations. Omits boolean attributes because Datadog doesn't use them
/// in any way.
///
/// This representation is used internally by the `ProfilesDictionary`, and
/// utilizes the fact that `StringRef`s don't have null values. It is also
/// repr(C) to be layout-compatible with [`Mapping2`]. Every pointer to a
/// Mapping is a valid Mapping2 (but the reverse is not true for the null case
/// of null StringId2).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mapping {
    pub memory_start: u64,
    pub memory_limit: u64,
    pub file_offset: u64,
    pub filename: StringRef,
    pub build_id: StringRef, // missing in Otel, is it made into an attribute?
}

/// An FFI-safe version of the Mapping which allows null. Be sure to maintain
/// layout-compatibility with it, except that StringId2 may be null.
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

impl From<SetId<Mapping>> for MappingId2 {
    fn from(id: SetId<Mapping>) -> MappingId2 {
        // SAFETY: the mapping that SetId points to is layout compatible with
        // the one that MappingId2 points to. The reverse is not true for the
        // null StringId cases.
        unsafe { core::mem::transmute::<SetId<Mapping>, MappingId2>(id) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn v1_and_v2_have_compatible_representations() {
        // Begin with basic size and alignment.
        assert_eq!(size_of::<Mapping>(), size_of::<Mapping2>());
        assert_eq!(align_of::<Mapping>(), align_of::<Mapping2>());

        // Then check members.
        assert_eq!(
            offset_of!(Mapping, memory_start),
            offset_of!(Mapping2, memory_start)
        );
        assert_eq!(
            offset_of!(Mapping, memory_limit),
            offset_of!(Mapping2, memory_limit)
        );
        assert_eq!(
            offset_of!(Mapping, file_offset),
            offset_of!(Mapping2, file_offset)
        );
        assert_eq!(
            offset_of!(Mapping, filename),
            offset_of!(Mapping2, filename)
        );
        assert_eq!(
            offset_of!(Mapping, build_id),
            offset_of!(Mapping2, build_id)
        );
    }
}
