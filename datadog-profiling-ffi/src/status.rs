// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use allocator_api2::alloc::{AllocError, Allocator, Global, Layout};
use datadog_profiling2::profiles::FallibleStringWriter;
use std::borrow::Cow;
use std::ffi::{c_char, CStr, CString};
use std::hint::unreachable_unchecked;
use std::mem::ManuallyDrop;
use std::ptr::{null, NonNull};

const FLAG_OK: usize = 0b00;
const FLAG_STATIC: usize = 0b01;
const FLAG_ALLOCATED: usize = 0b11;

const MASK_IS_ERROR: usize = 0b01;
const MASK_IS_ALLOCATED: usize = 0b10;
const MASK_UNUSED: usize = !(MASK_IS_ERROR | MASK_IS_ALLOCATED);

/// Represents the result of an operation that either succeeds with no value,
/// or fails with an error message. This is like `Result<(), Cow<CStr>` except
/// its representation is smaller, and is FFI-stable.
///
/// The OK status is guaranteed to have a representation of `{ 0, null }`.
#[repr(C)]
#[derive(Debug)]
pub struct ProfileStatus2 {
    /// 0 means okay, everything else is opaque in C.
    /// In Rust, the bits help us know whether it is heap allocated or not.
    pub flags: libc::size_t,
    /// If not null, this is a pointer to a valid null-terminated string in
    /// UTF-8 encoding.
    /// This is null if `flags` == 0.
    pub err: *const c_char,
}

impl Default for ProfileStatus2 {
    fn default() -> Self {
        Self {
            flags: 0,
            err: null(),
        }
    }
}

unsafe impl Send for ProfileStatus2 {}
unsafe impl Sync for ProfileStatus2 {}

impl<E: core::error::Error> From<Result<(), E>> for ProfileStatus2 {
    fn from(result: Result<(), E>) -> Self {
        match result {
            Ok(_) => ProfileStatus2::OK,
            Err(err) => ProfileStatus2::from_error(err),
        }
    }
}

impl From<&'static CStr> for ProfileStatus2 {
    fn from(value: &'static CStr) -> Self {
        Self {
            flags: FLAG_STATIC,
            err: value.as_ptr(),
        }
    }
}

impl From<CString> for ProfileStatus2 {
    fn from(cstring: CString) -> Self {
        Self {
            flags: FLAG_ALLOCATED,
            err: cstring.into_raw(),
        }
    }
}

impl TryFrom<ProfileStatus2> for CString {
    type Error = usize;

    fn try_from(status: ProfileStatus2) -> Result<Self, Self::Error> {
        if status.flags == FLAG_ALLOCATED {
            Ok(unsafe { CString::from_raw(status.err.cast_mut()) })
        } else {
            Err(status.flags)
        }
    }
}

impl TryFrom<&ProfileStatus2> for &CStr {
    type Error = usize;

    fn try_from(status: &ProfileStatus2) -> Result<Self, Self::Error> {
        if status.flags != FLAG_OK {
            Ok(unsafe { CStr::from_ptr(status.err.cast_mut()) })
        } else {
            Err(status.flags)
        }
    }
}

impl From<ProfileStatus2> for Result<(), Cow<'static, CStr>> {
    fn from(status: ProfileStatus2) -> Self {
        let flags = status.flags;
        let is_error = (flags & MASK_IS_ERROR) != 0;
        let is_allocated = (flags & MASK_IS_ALLOCATED) != 0;
        #[allow(clippy::panic)]
        if cfg!(debug_assertions) && (status.flags & MASK_UNUSED) != 0 {
            panic!("invalid bit pattern: {flags:b}");
        }
        match (is_allocated, is_error) {
            (false, false) => Ok(()),
            (false, true) => Err(Cow::Borrowed(unsafe { CStr::from_ptr(status.err) })),
            (true, true) => Err(Cow::Owned(unsafe {
                CString::from_raw(status.err.cast_mut())
            })),
            (true, false) => {
                #[allow(clippy::panic)]
                if cfg!(debug_assertions) {
                    panic!("invalid bit pattern: {flags:b}");
                }
                unsafe { unreachable_unchecked() }
            }
        }
    }
}

impl From<()> for ProfileStatus2 {
    fn from(_: ()) -> Self {
        Self::OK
    }
}

/// Tries to shrink a vec to exactly fit its length.
/// On success, the vector's capacity equals its length.
/// Returns an allocation error if the allocator cannot shrink.
fn vec_try_shrink_to_fit<T>(vec: &mut Vec<T>) -> Result<(), AllocError> {
    let len = vec.len();
    if vec.capacity() == len || core::mem::size_of::<T>() == 0 {
        return Ok(());
    }

    // Take ownership temporarily to manipulate raw parts; put an empty vec
    // in its place.
    let mut md = ManuallyDrop::new(core::mem::take(vec));

    // Avoid len=0 case for allocators by dropping the allocation and replacing
    // it with a new empty vec.
    if len == 0 {
        // SAFETY: we have exclusive access, and we're not exposing the zombie
        // bits to safe code since we're just returning (original vec was
        // replaced by an empty vec).
        unsafe { ManuallyDrop::drop(&mut md) };
        return Ok(());
    }

    let ptr = md.as_mut_ptr();
    let cap = md.capacity();

    // SAFETY: Vec invariants ensure `cap >= len`, and capacity/len fit isize.
    let old_layout = unsafe { Layout::array::<T>(cap).unwrap_unchecked() };
    let new_layout = unsafe { Layout::array::<T>(len).unwrap_unchecked() };

    // SAFETY: `ptr` is non-null and properly aligned for T (Vec invariant).
    let old_ptr_u8 = unsafe { NonNull::new_unchecked(ptr.cast::<u8>()) };

    match unsafe { Global.shrink(old_ptr_u8, old_layout, new_layout) } {
        Ok(new_ptr_u8) => {
            let new_ptr = new_ptr_u8.as_ptr().cast::<T>();
            // SAFETY: new allocation valid for len Ts; capacity == len.
            let new_vec = unsafe { Vec::from_raw_parts(new_ptr, len, len) };
            *vec = new_vec;
            Ok(())
        }
        Err(_) => {
            // Reconstruct original and put it back; report OOM.
            let orig = unsafe { Vec::from_raw_parts(ptr, len, cap) };
            *vec = orig;
            Err(AllocError)
        }
    }
}

fn string_try_shrink_to_fit(string: &mut String) -> Result<(), AllocError> {
    // Take ownership to get access to the backing Vec<u8>.
    let mut bytes = core::mem::take(string).into_bytes();
    let res = vec_try_shrink_to_fit(&mut bytes);
    // SAFETY: bytes came from a valid UTF-8 String and were not mutated.
    *string = unsafe { String::from_utf8_unchecked(bytes) };
    res
}

impl ProfileStatus2 {
    pub const OK: ProfileStatus2 = ProfileStatus2 {
        flags: FLAG_OK,
        err: null(),
    };

    const OUT_OF_MEMORY: ProfileStatus2 = ProfileStatus2 {
        flags: FLAG_STATIC,
        err: c"out of memory while trying to display error".as_ptr(),
    };
    const NULL_BYTE_IN_ERROR_MESSAGE: ProfileStatus2 = ProfileStatus2 {
        flags: FLAG_STATIC,
        err: c"another error occured, but cannot be displayed because it has interior null bytes"
            .as_ptr(),
    };

    pub fn from_ffi_safe_error_message<E: ddcommon::error::FfiSafeErrorMessage>(err: E) -> Self {
        ProfileStatus2::from(err.as_ffi_str())
    }

    pub fn from_error<E: core::error::Error>(err: E) -> Self {
        use core::fmt::Write;
        let mut writer = FallibleStringWriter::new();
        if write!(writer, "{}", err).is_err() {
            return ProfileStatus2::OUT_OF_MEMORY;
        }

        let mut str = String::from(writer);

        // std doesn't expose memchr even though it has it, but fortunately
        // libc has it, and we use the libc crate already in FFI.
        let pos = unsafe { libc::memchr(str.as_ptr().cast(), 0, str.len()) };
        if !pos.is_null() {
            return ProfileStatus2::NULL_BYTE_IN_ERROR_MESSAGE;
        }

        // Reserve memory exactly. We have to shrink later in order to turn
        // it into a box, so we don't want any excess capacity.
        if str.try_reserve_exact(1).is_err() {
            return ProfileStatus2::OUT_OF_MEMORY;
        }
        str.push('\0');

        if string_try_shrink_to_fit(&mut str).is_err() {
            return ProfileStatus2::OUT_OF_MEMORY;
        }

        // Pop the null off because CString::from_vec_unchecked adds one.
        _ = str.pop();

        // And finally, this is why we went through the pain of
        // string_try_shrink_to_fit: this method will call shrink_to_fit, so
        // to avoid an allocation failure here, we had to make a String with
        // no excess capacity.
        let cstring = unsafe { CString::from_vec_unchecked(str.into_bytes()) };
        ProfileStatus2::from(cstring)
    }
}

/// Frees any error associated with the status, and replaces it with an OK.
///
/// # Safety
///
/// The pointer should point at a valid Status object, if it's not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_Status_drop(status: *mut ProfileStatus2) {
    if status.is_null() {
        return;
    }
    // SAFETY: safe when the user respects ddog_prof2_Status_drop's conditions.
    let status = unsafe { core::ptr::replace(status, ProfileStatus2::OK) };
    drop(Result::from(status));
}
