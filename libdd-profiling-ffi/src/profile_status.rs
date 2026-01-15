// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileError;
use allocator_api2::alloc::{AllocError, Allocator, Global, Layout};
use libdd_profiling::profiles::FallibleStringWriter;
use std::borrow::Cow;
use std::ffi::{c_char, CStr, CString};
use std::fmt::Display;
use std::mem::ManuallyDrop;
use std::ptr::{null, NonNull};

/// ProfileStatus uses `err` being null to encode OK, so we only need
/// one bit in flags to distinguish between STATIC and ALLOCATED errors.
const IS_ALLOCATED_MASK: usize = 1;

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
/// free any heap-allocated memory. This is safe to call on OK as well.
///
/// # FFI Safety
///
/// This type is `#[repr(C)]` and safe to pass across FFI boundaries. The C side must treat the
/// `.flags` as opaque and use API functions; the `.err` field is guaranteed to be null when the
/// `ProfileStatus` is OK, and on Err it will be non-null pointer to a UTF8 encoded string which
/// has a null  terminator.
#[repr(C)]
#[derive(Debug)]
pub struct ProfileStatus {
    /// Bitflags indicating the storage type of the error message.
    /// This is only meaningful when `err` is non-null. When `err` is
    /// null (indicating OK), this field SHOULD be zero. Currently, only one
    /// bit is used `IS_ALLOCATED_MASK`, which determines whether the error
    /// message is owned or statically borrowed.
    /// In the future, we may store error codes in here as well.
    pub flags: libc::size_t,

    /// Pointer to a null-terminated UTF-8 error message string.
    /// - If null this indicates OK (success). This is an FFI guarantee.
    /// - If non-null and allocated bit is clear: points to static data with `'static` lifetime.
    /// - If non-null and allocated bit is set: points to owned heap-allocated data.
    ///
    /// # Safety Invariant
    ///
    /// When non-null, `err` must point to a valid, null-terminated C string in UTF-8 encoding.
    /// The pointer remains valid for the lifetime of this `ProfileStatus` or until
    /// [`ddog_prof_Status_drop`] is called.
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
//    - Null (OK status), which is trivially Send.
//    - Points to static data (allocated bit clear), which is 'static and therefore Send.
//    - Points to heap-allocated data (allocated bit set) that this ProfileStatus owns exclusively.
//      When sent to another thread, the ownership of the allocation transfers with it, and the drop
//      implementation ensures proper cleanup on the receiving thread.
// This is semantically equivalent to `Result<(), Cow<'static, CStr>>`, which is Send.
unsafe impl Send for ProfileStatus {}

// SAFETY: ProfileStatus is Sync because:
// 1. All fields are immutable from a shared reference (&ProfileStatus).
// 2. The `err` pointer points to immutable data:
//    - Null (OK status): trivially Sync.
//    - Static CStr (allocated bit clear): &'static CStr is Sync.
//    - Heap-allocated CStr (allocated bit set): The CStr is never mutated after creation, so
//      multiple threads can safely read it concurrently.
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
            flags: 0,
            err: value.as_ptr(),
        }
    }
}

impl From<CString> for ProfileStatus {
    fn from(cstring: CString) -> Self {
        Self {
            flags: IS_ALLOCATED_MASK,
            err: cstring.into_raw(),
        }
    }
}

impl From<ProfileStatus> for Result<(), Cow<'static, CStr>> {
    fn from(status: ProfileStatus) -> Self {
        let flags = status.flags;
        if status.err.is_null() {
            status.verify_flags()
        } else if flags == IS_ALLOCATED_MASK {
            Err(Cow::Owned(unsafe {
                CString::from_raw(status.err.cast_mut())
            }))
        } else {
            // Static error (allocated bit clear)
            Err(Cow::Borrowed(unsafe { CStr::from_ptr(status.err) }))
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
        flags: 0,
        err: null(),
    };

    /// Verifies that the flags make sense for the state of the object. With
    /// debug assertions on, this will panic, and in production, it will return
    /// an error instead.
    fn verify_flags(&self) -> Result<(), Cow<'static, CStr>> {
        let flags = self.flags;
        if self.err.is_null() {
            if flags == 0 {
                Ok(())
            } else {
                // This is a programming error, so in debug builds we panic,
                // but in non-debug builds, we return an error message.
                debug_assert_eq!(
                    flags, 0,
                    "expected empty flag bits for ProfileStatus with no error, saw {flags:#x}"
                );
                use core::fmt::Write;
                let mut writer = FallibleStringWriter::new();
                // Include the null terminator here for the sake of allocating
                // the correct amount of memory, but it will be removed below.
                Err(
                    if write!(
                    writer,
                    "expected empty flag bits for ProfileStatus with no error, saw {flags:#x}\0"
                )
                    .is_err()
                    {
                        // We couldn't allocate or format for some reason, so just
                        // use a static string with less information.
                        Cow::Borrowed(c"expected empty flag bits for ProfileStatus with no error")
                    } else {
                        let string = String::from(writer);
                        let mut bytes = string.into_bytes();
                        // Remove the null terminator because from_vec_unchecked
                        // expects no nulls at all.
                        bytes.pop();
                        // SAFETY: the error message is ASCII, the only dynamic
                        // bit (ha) is the hexadecimal repr of the flags.
                        let err = unsafe { CString::from_vec_unchecked(bytes) };
                        Cow::Owned(err)
                    },
                )
            }
        } else {
            Ok(())
        }
    }

    pub fn from_ffi_safe_error_message<E: libdd_common::error::FfiSafeErrorMessage>(
        err: E,
    ) -> Self {
        ProfileStatus::from(err.as_ffi_str())
    }

    pub fn from_error<E: Display>(err: E) -> Self {
        ProfileStatus::from(ProfileError::from_display(err))
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
    fn test_from_result_ok() {
        let result: Result<(), anyhow::Error> = Ok(());
        let status = ProfileStatus::from(result);

        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());
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

        assert_eq!(status.flags, IS_ALLOCATED_MASK);
        let err_ptr = status.err;
        assert!(!err_ptr.is_null());

        unsafe { ddog_prof_Status_drop(&mut status) };

        // Should be OK now
        assert_eq!(status.flags, 0);
        assert!(status.err.is_null());
        // The allocated memory should have been freed (can't really test this without valgrind)
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
