// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{SetId, StringRef};
use crate::profiles::datatypes::StringId2;

/// A representation of a function that is an intersection of the Otel and
/// Pprof representations. Omits the start line to save space because Datadog
/// doesn't use this in any way.
///
/// This representation is used internally by the `ProfilesDictionary`, and
/// utilizes the fact that `StringRef`s don't have null values. It is also
/// repr(C) to be layout-compatible with [`Function2`]. Every pointer to a
/// Function is a valid Function2 (but the reverse is not true for the null
/// case of null StringId2).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[repr(C)]
pub struct Function {
    pub name: StringRef,
    pub system_name: StringRef,
    pub file_name: StringRef,
}

/// An FFI-safe version of the Function which allows null. Be sure to maintain
/// layout-compatibility with it, except that StringId2 may be null.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Function2 {
    pub name: StringId2,
    pub system_name: StringId2,
    pub file_name: StringId2,
}

impl From<Function> for Function2 {
    fn from(f: Function) -> Function2 {
        Function2 {
            name: f.name.into(),
            system_name: f.system_name.into(),
            file_name: f.file_name.into(),
        }
    }
}

impl From<Function2> for Function {
    fn from(f2: Function2) -> Function {
        Function {
            name: f2.name.into(),
            system_name: f2.system_name.into(),
            file_name: f2.file_name.into(),
        }
    }
}

/// An FFI-safe representation of a "handle" to a function which has been
/// stored in the `ProfilesDictionary`. The representation is ensured to be a
/// pointer for ABI stability, but callers should not generally dereference
/// this pointer. When using the id, the caller needs to be sure that the
/// `ProfilesDictionary` it refers to is the same one that the operations are
/// performed on; it is not generally guaranteed that ids from one dictionary
/// can be used in another dictionary, even if it happens to work by
/// implementation detail.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct FunctionId2(pub(crate) *mut Function2);

// todo: when MSRV is 1.88.0+, derive Default
impl Default for FunctionId2 {
    fn default() -> Self {
        Self(core::ptr::null_mut())
    }
}

impl FunctionId2 {
    pub fn is_empty(self) -> bool {
        self.0.is_null()
    }

    /// Converts the `FunctionId2` into an `Option<Function2>` where an empty
    /// `FunctionId2` converts to a `None`.
    ///
    /// # Safety
    /// The pointer object must still be alive. In general this means the
    /// profiles dictionary it came from must be alive.
    pub unsafe fn read(self) -> Option<Function2> {
        if self.is_empty() {
            None
        } else {
            Some(self.0.read())
        }
    }
}

impl From<SetId<Function>> for FunctionId2 {
    fn from(id: SetId<Function>) -> FunctionId2 {
        // SAFETY: the function that SetId points to is layout compatible with
        // the one that FunctionId2 points to. The reverse is not true for the
        // null StringId cases.
        unsafe { core::mem::transmute::<SetId<Function>, FunctionId2>(id) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn v1_and_v2_have_compatible_representations() {
        // Begin with basic size and alignment.
        assert_eq!(size_of::<Function>(), size_of::<Function2>());
        assert_eq!(align_of::<Function>(), align_of::<Function2>());

        // Then check members.
        assert_eq!(offset_of!(Function, name), offset_of!(Function2, name));
        assert_eq!(
            offset_of!(Function, system_name),
            offset_of!(Function2, system_name)
        );
        assert_eq!(
            offset_of!(Function, file_name),
            offset_of!(Function2, file_name)
        );
    }
}
