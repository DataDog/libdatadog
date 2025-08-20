// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_alloc::{AllocError, Allocator, Global};
use datadog_profiling::profiles::{FallibleStringWriter, ProfileError};
use std::borrow::Cow;
use std::ffi::{c_char, CStr, CString};
use std::hint::unreachable_unchecked;

const FLAG_OK: usize = 0b00;
const FLAG_STATIC: usize = 0b01;
const FLAG_ALLOCATED: usize = 0b11;

const MASK_IS_ERROR: usize = 0b01;
const MASK_IS_ALLOCATED: usize = 0b10;
const MASK_UNUSED: usize = !(MASK_IS_ERROR | MASK_IS_ALLOCATED);

pub const STATUS_OK: usize = 0;

// Extracting the "common" fields allow for common union handling that is
// agnostic to the type of T.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ProfileStatus {
    // 0 means okay, everything else is opaque in C.
    // In Rust, it will need to reserve a bit somewhere.
    pub flags: usize,
    pub err: *const c_char, // null when okay
}

impl<E: core::error::Error> From<Result<(), E>> for ProfileStatus {
    fn from(result: Result<(), E>) -> Self {
        match result {
            Ok(_) => ProfileStatus::OK,
            Err(err) => ProfileStatus::from_error(err),
        }
    }
}

impl From<CString> for ProfileStatus {
    fn from(cstring: CString) -> Self {
        Self { flags: FLAG_ALLOCATED, err: cstring.into_raw() }
    }
}

impl TryFrom<ProfileStatus> for CString {
    type Error = usize;

    fn try_from(status: ProfileStatus) -> Result<Self, Self::Error> {
        if status.flags == FLAG_ALLOCATED {
            Ok(unsafe { CString::from_raw(status.err.cast_mut()) })
        } else {
            Err(status.flags)
        }
    }
}

impl TryFrom<ProfileStatus> for &'static CStr {
    type Error = usize;

    fn try_from(status: ProfileStatus) -> Result<Self, Self::Error> {
        if status.flags != FLAG_OK {
            Ok(unsafe { CStr::from_ptr(status.err.cast_mut()) })
        } else {
            Err(status.flags)
        }
    }
}

impl From<ProfileStatus> for Result<(), Cow<'static, CStr>> {
    fn from(status: ProfileStatus) -> Self {
        let flags = status.flags;
        let is_error = (flags & MASK_IS_ERROR) != 0;
        let is_allocated = (flags & MASK_IS_ALLOCATED) != 0;
        if cfg!(debug_assertions) {
            if MASK_UNUSED != 0 {
                panic!("invalid bit pattern: {flags:b}");
            }
        }
        match (is_allocated, is_error) {
            (false, false) => Ok(()),
            (false, true) => {
                Err(Cow::Borrowed(unsafe { CStr::from_ptr(status.err) }))
            }
            (true, true) => Err(Cow::Owned(unsafe {
                CString::from_raw(status.err.cast_mut())
            })),
            (true, false) => {
                if cfg!(debug_assertions) {
                    panic!("invalid bit pattern: {flags:b}");
                }
                unsafe { unreachable_unchecked() }
            }
        }
    }
}

impl From<()> for ProfileStatus {
    fn from(_: ()) -> Self {
        Self::OK
    }
}

fn try_shrink_to_fit_vec<T: Copy>(vec: Vec<T>) -> Result<Vec<T>, AllocError> {
    if vec.capacity() > vec.len() {
        // Unfortunately, there aren't APIs for try_into_boxed_slice, so we
        // can't shrink in place without violating abstractions.
        // So we force a copy.
        let mut new = Vec::new();
        new.try_reserve(vec.len()).map_err(|_| AllocError)?;
        new.copy_from_slice(vec.as_slice());
        Ok(new)
    } else {
        Ok(vec)
    }
}

fn try_shrink_to_fit(string: String) -> Result<String, AllocError> {
    let bytes = try_shrink_to_fit_vec(string.into_bytes())?;
    unsafe { Ok(String::from_utf8_unchecked(bytes)) }
}

impl ProfileStatus {
    pub const OK: ProfileStatus =
        ProfileStatus { flags: FLAG_OK, err: std::ptr::null() };

    pub const OUT_OF_MEMORY: ProfileStatus = ProfileStatus {
        flags: FLAG_STATIC,
        err: c"out of memory while trying to display error".as_ptr(),
    };
    pub const NULL_BYTE_IN_ERROR_MESSAGE: ProfileStatus = ProfileStatus {
        flags: FLAG_STATIC,
        err: c"another error occured, but cannot be displayed because it has interior null bytes".as_ptr(),
    };

    pub fn from_error<E: core::error::Error>(err: E) -> Self {
        use core::fmt::Write;
        let mut writer = FallibleStringWriter::new();
        if write!(writer, "{}", err).is_err() {
            return ProfileStatus::OUT_OF_MEMORY;
        }

        // Terminate with null. We use exact because it's the last append and
        // in some cases we may get the exact size and avoid a shrink.
        if writer.try_reserve_exact(1).is_err() {
            return ProfileStatus::OUT_OF_MEMORY;
        }
        // Cannot fail, we just reserved the memory.
        _ = writer.write_str("\0");

        // For FFI, it has to be an exact fit. It may not fit exactly already
        // such as if there was capacity for 8, len=4, and we append the null
        // the length is still only 5 of 8.
        let Ok(str) = try_shrink_to_fit(writer.into()) else {
            return ProfileStatus::OUT_OF_MEMORY;
        };
        if CStr::from_bytes_with_nul(str.as_bytes()).is_err() {
            return ProfileStatus::NULL_BYTE_IN_ERROR_MESSAGE;
        }
        ProfileStatus::from(unsafe {
            CString::from_vec_unchecked(str.into_bytes())
        })
    }
}

// handles okay, heap-allocated, and static so caller doesn't need to be
// aware of which bit is for indicating heap allocation.
#[no_mangle]
pub unsafe extern "C" fn ddog_Status_drop(status: ProfileStatus) {
    if let Ok(cstring) = CString::try_from(status) {
        drop(cstring);
    }
}
