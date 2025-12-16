// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use allocator_api2::alloc::{AllocError, Allocator, Global, Layout};
use libdd_profiling::profiles::FallibleStringWriter;
use std::borrow::Cow;
use std::ffi::{c_char, CStr, CString};
use std::fmt::Display;
use std::hint::unreachable_unchecked;
use std::mem::ManuallyDrop;
use std::ptr::{null, NonNull};

const FLAG_OK: usize = 0b00;
const FLAG_STATIC: usize = 0b01;
const FLAG_ALLOCATED: usize = 0b11;

const MASK_IS_ERROR: usize = 0b01;
const MASK_IS_ALLOCATED: usize = 0b10;
const MASK_UNUSED: usize = !(MASK_IS_ERROR | MASK_IS_ALLOCATED);

/// Represents the result of an operation that either succeeds with no value, or fails with an
/// error message. This is like `Result<(), Cow<'static, CStr>` except its representation is
/// smaller, and is FFI-stable.
///
/// The OK status is guaranteed to have a representation of `{ 0, null }`.
///
/// # Ownership
///
/// A `ProfileStatus` owns its error message data. When a `ProfileStatus` with an error is
/// created, it takes ownership of the error string (either as a static reference or heap
/// allocation). The caller is responsible for eventually calling [`ddog_prof_Status_drop`] to
/// free any heap-allocated memory. This is safe to call on OK as well; it does nothing.
///
/// # FFI Safety
///
/// This type is `#[repr(C)]` and safe to pass across FFI boundaries. The C side must treat
/// this as an opaque struct and use the provided FFI functions to inspect and drop it.
#[repr(C)]
#[derive(Debug)]
pub struct ProfileStatus {
    /// Bitflags indicating the status and storage type.
    /// - `FLAG_OK` (0): Success, no error. `err` must be null. From C, this is the only thing you
    ///   should check; the other flags are internal details.
    /// - `FLAG_STATIC`: Error message points to static data. `err` is non-null and points to a
    ///   `&'static CStr`. Must not be freed.
    /// - `FLAG_ALLOCATED`: Error message is heap-allocated. `err` is non-null and points to a
    ///   heap-allocated, null-terminated string that this `ProfileStatus` owns. Must be freed via
    ///   [`ddog_prof_Status_drop`].
    pub flags: libc::size_t,

    /// Pointer to a null-terminated UTF-8 error message string.
    /// - If `flags == FLAG_OK`, this **must** be null.
    /// - If `flags & FLAG_STATIC`, this points to static data with lifetime `'static`.
    /// - If `flags & FLAG_ALLOCATED`, this points to heap-allocated data owned by this
    ///   `ProfileStatus`. The allocation was created by the global allocator and must be freed by
    ///   [`ddog_prof_Status_drop`].
    ///
    /// # Safety Invariant
    ///
    /// When non-null, `err` must point to a valid, null-terminated C
    /// string in UTF-8 encoding. The pointer remains valid for the
    /// lifetime of this `ProfileStatus` or until [`ddog_prof_Status_drop`]
    /// is called.
    pub err: *const c_char,
}

impl Default for ProfileStatus {
    fn default() -> Self {
        Self {
            flags: 0,
            err: null(),
        }
    }
}

// SAFETY: ProfileStatus is Send because:
// 1. The `flags` field is a usize, which is Send.
// 2. The `err` pointer is either:
//    - Null (FLAG_OK), which is trivially Send
//    - Points to static data (FLAG_STATIC), which is 'static and therefore Send
//    - Points to heap-allocated data (FLAG_ALLOCATED) that this ProfileStatus owns exclusively.
//      When sent to another thread, the ownership of the allocation transfers with it, and the drop
//      implementation ensures proper cleanup on the receiving thread.
// This is semantically equivalent to `Result<(), Cow<'static, CStr>>`, which is Send.
unsafe impl Send for ProfileStatus {}

// SAFETY: ProfileStatus is Sync because:
// 1. All fields are immutable from a shared reference (&ProfileStatus).
// 2. The `err` pointer points to immutable data:
//    - Static CStr (FLAG_STATIC): &'static CStr is Sync
//    - Heap-allocated CStr (FLAG_ALLOCATED): The CStr is never mutated after creation, so multiple
//      threads can safely read it concurrently.
// 3. There are no interior mutability patterns (no Cell, RefCell, etc.).
// Multiple threads holding &ProfileStatus can safely read the same error message.
unsafe impl Sync for ProfileStatus {}

impl<E> From<Result<(), E>> for ProfileStatus
where
    ProfileStatus: From<E>,
{
    fn from(result: Result<(), E>) -> Self {
        match result {
            Ok(_) => ProfileStatus::OK,
            Err(err) => ProfileStatus::from(err),
        }
    }
}

impl From<anyhow::Error> for ProfileStatus {
    fn from(err: anyhow::Error) -> ProfileStatus {
        ProfileStatus::from_error(err)
    }
}

impl From<&'static CStr> for ProfileStatus {
    fn from(value: &'static CStr) -> Self {
        Self {
            flags: FLAG_STATIC,
            err: value.as_ptr(),
        }
    }
}

impl From<CString> for ProfileStatus {
    fn from(cstring: CString) -> Self {
        Self {
            flags: FLAG_ALLOCATED,
            err: cstring.into_raw(),
        }
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

impl TryFrom<&ProfileStatus> for &CStr {
    type Error = usize;

    fn try_from(status: &ProfileStatus) -> Result<Self, Self::Error> {
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

impl From<()> for ProfileStatus {
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

pub(crate) fn string_try_shrink_to_fit(string: &mut String) -> Result<(), AllocError> {
    // Take ownership to get access to the backing Vec<u8>.
    let mut bytes = core::mem::take(string).into_bytes();
    let res = vec_try_shrink_to_fit(&mut bytes);
    // SAFETY: bytes came from a valid UTF-8 String and were not mutated.
    *string = unsafe { String::from_utf8_unchecked(bytes) };
    res
}

impl ProfileStatus {
    pub const OK: ProfileStatus = ProfileStatus {
        flags: FLAG_OK,
        err: null(),
    };

    const OUT_OF_MEMORY: ProfileStatus = ProfileStatus {
        flags: FLAG_STATIC,
        err: c"out of memory while trying to display error".as_ptr(),
    };
    const NULL_BYTE_IN_ERROR_MESSAGE: ProfileStatus = ProfileStatus {
        flags: FLAG_STATIC,
        err: c"another error occured, but cannot be displayed because it has interior null bytes"
            .as_ptr(),
    };

    pub fn from_ffi_safe_error_message<E: libdd_common::error::FfiSafeErrorMessage>(
        err: E,
    ) -> Self {
        ProfileStatus::from(err.as_ffi_str())
    }

    pub fn from_error<E: Display>(err: E) -> Self {
        use core::fmt::Write;
        let mut writer = FallibleStringWriter::new();
        if write!(writer, "{err}").is_err() {
            return ProfileStatus::OUT_OF_MEMORY;
        }

        let mut str = String::from(writer);

        // std doesn't expose memchr even though it has it, but fortunately
        // libc has it, and we use the libc crate already in FFI.
        let pos = unsafe { libc::memchr(str.as_ptr().cast(), 0, str.len()) };
        if !pos.is_null() {
            return ProfileStatus::NULL_BYTE_IN_ERROR_MESSAGE;
        }

        // Reserve memory exactly. We have to shrink later in order to turn
        // it into a box, so we don't want any excess capacity.
        if str.try_reserve_exact(1).is_err() {
            return ProfileStatus::OUT_OF_MEMORY;
        }
        str.push('\0');

        if string_try_shrink_to_fit(&mut str).is_err() {
            return ProfileStatus::OUT_OF_MEMORY;
        }

        // Pop the null off because CString::from_vec_unchecked adds one.
        _ = str.pop();

        // And finally, this is why we went through the pain of
        // string_try_shrink_to_fit: this method will call shrink_to_fit, so
        // to avoid an allocation failure here, we had to make a String with
        // no excess capacity.
        let cstring = unsafe { CString::from_vec_unchecked(str.into_bytes()) };
        ProfileStatus::from(cstring)
    }
}

/// Frees any error associated with the status, and replaces it with an OK.
///
/// # Safety
///
/// The pointer should point at a valid Status object, if it's not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Status_drop(status: *mut ProfileStatus) {
    if status.is_null() {
        return;
    }
    // SAFETY: safe when the user respects ddog_prof_Status_drop's conditions.
    let status = unsafe { core::ptr::replace(status, ProfileStatus::OK) };
    drop(Result::from(status));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn test_ok_status() {
        let status = ProfileStatus::OK;
        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());

        // Default should be OK
        let default_status = ProfileStatus::default();
        assert_eq!(default_status.flags, 0);
        assert!(default_status.err.is_null());

        // From () should be OK
        let from_unit = ProfileStatus::from(());
        assert_eq!(from_unit.flags, 0);
        assert!(from_unit.err.is_null());

        // Convert OK to Result
        let result: Result<(), Cow<'static, CStr>> = status.into();
        assert!(result.is_ok());
    }

    #[test]
    fn test_static_error() {
        let msg = c"test error message";
        let status = ProfileStatus::from(msg);

        assert_eq!(status.flags, FLAG_STATIC);
        assert_eq!(status.err, msg.as_ptr());

        // Convert to CStr
        let cstr: &CStr = (&status).try_into().unwrap();
        assert_eq!(cstr, msg);

        // Convert to Result
        let result: Result<(), Cow<'static, CStr>> = status.into();
        assert!(result.is_err());
        match result {
            Err(Cow::Borrowed(borrowed)) => assert_eq!(borrowed, msg),
            _ => panic!("Expected Cow::Borrowed"),
        }
    }

    #[test]
    fn test_allocated_error() {
        let msg = CString::new("allocated error").unwrap();
        let msg_clone = msg.clone();
        let status = ProfileStatus::from(msg);

        assert_eq!(status.flags, FLAG_ALLOCATED);
        assert!(!status.err.is_null());

        // Convert to CStr
        let cstr: &CStr = (&status).try_into().unwrap();
        assert_eq!(cstr, msg_clone.as_c_str());

        // Convert to CString
        let recovered = CString::try_from(status).unwrap();
        assert_eq!(recovered, msg_clone);
    }

    #[test]
    fn test_from_anyhow_error() {
        let err = anyhow::anyhow!("something went wrong");
        let status = ProfileStatus::from(err);

        assert!(status.flags != 0);
        assert!(!status.err.is_null());

        let cstr: &CStr = (&status).try_into().unwrap();
        assert_eq!(cstr.to_str().unwrap(), "something went wrong");

        // Clean up
        let _result: Result<(), Cow<'static, CStr>> = status.into();
    }

    #[test]
    fn test_from_result_ok() {
        let result: Result<(), anyhow::Error> = Ok(());
        let status = ProfileStatus::from(result);

        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());
    }

    #[test]
    fn test_from_result_err() {
        let result: Result<(), anyhow::Error> = Err(anyhow::anyhow!("error from result"));
        let status = ProfileStatus::from(result);

        assert!(status.flags != 0);
        assert!(!status.err.is_null());

        let cstr: &CStr = (&status).try_into().unwrap();
        assert_eq!(cstr.to_str().unwrap(), "error from result");

        // Clean up
        let _result: Result<(), Cow<'static, CStr>> = status.into();
    }

    #[test]
    fn test_from_error_with_display() {
        #[derive(Debug)]
        struct CustomError(&'static str);

        impl std::fmt::Display for CustomError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "custom: {}", self.0)
            }
        }

        let status = ProfileStatus::from_error(CustomError("test"));

        assert_eq!(status.flags, FLAG_ALLOCATED);
        assert!(!status.err.is_null());

        let cstr: &CStr = (&status).try_into().unwrap();
        assert_eq!(cstr.to_str().unwrap(), "custom: test");

        // Clean up
        let _result: Result<(), Cow<'static, CStr>> = status.into();
    }

    #[test]
    fn test_ffi_drop_null() {
        // Should not crash
        unsafe { ddog_prof_Status_drop(std::ptr::null_mut()) };
    }

    #[test]
    fn test_ffi_drop_ok() {
        let mut status = ProfileStatus::OK;
        unsafe { ddog_prof_Status_drop(&mut status) };
        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());
    }

    #[test]
    fn test_ffi_drop_static() {
        let mut status = ProfileStatus::from(c"static message");
        let original_ptr = status.err;

        unsafe { ddog_prof_Status_drop(&mut status) };

        // Should be OK now
        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());

        // Original pointer should still be valid (static)
        let recovered = unsafe { CStr::from_ptr(original_ptr) };
        assert_eq!(recovered, c"static message");
    }

    #[test]
    fn test_ffi_drop_allocated() {
        let msg = CString::new("allocated message").unwrap();
        let mut status = ProfileStatus::from(msg);

        assert_eq!(status.flags, FLAG_ALLOCATED);
        let err_ptr = status.err;
        assert!(!err_ptr.is_null());

        unsafe { ddog_prof_Status_drop(&mut status) };

        // Should be OK now
        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());
        // The allocated memory should have been freed (can't really test this without valgrind)
    }

    #[test]
    fn test_try_from_cstr_on_ok_fails() {
        let status = ProfileStatus::OK;
        let result: Result<&CStr, usize> = (&status).try_into();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), FLAG_OK);
    }

    #[test]
    fn test_try_from_cstring_on_static_fails() {
        let status = ProfileStatus::from(c"static");
        let result = CString::try_from(status);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), FLAG_STATIC);
    }

    #[test]
    fn test_send_sync() {
        // Just check that ProfileStatus implements Send and Sync
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<ProfileStatus>();
        assert_sync::<ProfileStatus>();
    }
}
