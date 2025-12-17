// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringRef;

/// An FFI-safe string ID where a null StringId2 maps to `StringRef::EMPTY`.
/// The representation is ensured to be a  pointer for ABI stability, but
/// callers should not generally dereference this pointer. When using the id,
/// the caller needs to be sure that the `ProfilesDictionary` or string set it
/// refers to is the same one that the operations are performed on; it is not
/// generally guaranteed that ids from one dictionary/set can be used in
/// another, even if it happens to work by implementation detail. There is an
/// exception is for well-known strings, which are considered present in every
/// string set.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct StringId2(*mut StringHeader);

/// Represents what StringIds point to. Its definition is intentionally
/// obscured; the actual layout is being hidden. This is here so that
/// cbindgen will generate a unique type as opposed to relying on `void *` or
/// similar. We want StringId2, FunctionId2, and MappingId2 to all point to
/// unique so that compilers will distinguish between them and provide some
/// type safety.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct StringHeader(u8);

unsafe impl Send for StringId2 {}

unsafe impl Sync for StringId2 {}

// todo: when MSRV is 1.88.0+, derive Default
impl Default for StringId2 {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl StringId2 {
    pub const EMPTY: StringId2 = StringId2(core::ptr::null_mut());

    pub fn is_empty(&self) -> bool {
        self.0.is_null()
    }
}

impl From<StringRef> for StringId2 {
    fn from(s: StringRef) -> Self {
        // SAFETY: every StringRef is a valid StringId2 (but not the other way
        // because of null).
        unsafe { core::mem::transmute::<StringRef, StringId2>(s) }
    }
}

impl From<StringId2> for StringRef {
    fn from(id: StringId2) -> Self {
        if id.0.is_null() {
            StringRef::EMPTY
        } else {
            // SAFETY: every non-null StringId2 is supposed to be a valid
            // StringRef.
            unsafe { core::mem::transmute::<StringId2, StringRef>(id) }
        }
    }
}
