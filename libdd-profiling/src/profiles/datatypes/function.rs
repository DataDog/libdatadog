// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringRef;
use crate::profiles::datatypes::StringId2;

/// A representation of a function that is an intersection of the Otel and
/// Pprof representations. Omits the start line to save space because Datadog
/// doesn't use this in any way.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[repr(C)]
pub struct Function {
    pub name: StringRef,
    pub system_name: StringRef,
    pub file_name: StringRef,
}

/// An FFI-safe version of the Function.
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
