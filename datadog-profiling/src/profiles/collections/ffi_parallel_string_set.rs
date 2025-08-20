// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ParallelStringSet;
use super::string_set::StringId;
use super::SetError;

use core::ffi::c_void;
use std::borrow::Cow;
use std::mem::ManuallyDrop;
use std::ptr::{null_mut, NonNull};

/// A parallel set that allows for a null pointer for FFI safety.
///
/// Operations on the set are thread-safe, but each thread needs its own copy
/// to keep the refcounts alive. Use the
/// [`ddog_prof_ParallelStringSet_try_clone`] function to create a copy.
///
/// The underlying implementation uses read-write locks, so it is possible for
/// a writer to block readers. However, the implementation is designed to
/// minimize contention by using multiple shards.
#[repr(C)]
#[derive(Debug, Default)]
pub struct FfiParallelStringSet {
    pub(crate) ptr: *mut c_void,
}

// SAFETY: The FFI set uses an Arc, it's just erased for FFI reasons. Arc
// pointers are both Send and Sync if the underlying type is, and in our case,
// the ParallelSliceStorage is Send and Sync.
unsafe impl Send for FfiParallelStringSet {}
//SAFETY: The FFI set uses an arc pointer, it's just erased for FFI reasons.
unsafe impl Sync for FfiParallelStringSet {}

#[repr(C)]
pub enum ParallelStringSetTryInsertResult {
    Ok(StringId),
    Err(SetError),
}

#[repr(C)]
pub enum Utf8Option {
    /// The string is assumed to be valid UTF-8. If it's not, the behavior
    /// is undefined.
    Assume,
    /// The string is converted to UTF-8 using lossy conversion.
    ConvertLossy,
    /// The string is validated to be UTF-8. If it's not, an error is
    /// returned.
    Validate,
}

impl From<Result<StringId, SetError>> for ParallelStringSetTryInsertResult {
    fn from(result: Result<StringId, SetError>) -> Self {
        match result {
            Ok(id) => ParallelStringSetTryInsertResult::Ok(id),
            Err(err) => ParallelStringSetTryInsertResult::Err(err),
        }
    }
}

impl From<ParallelStringSetTryInsertResult> for Result<StringId, SetError> {
    fn from(result: ParallelStringSetTryInsertResult) -> Self {
        match result {
            ParallelStringSetTryInsertResult::Ok(ok) => Ok(ok),
            ParallelStringSetTryInsertResult::Err(err) => Err(err),
        }
    }
}

impl From<ParallelStringSet> for FfiParallelStringSet {
    fn from(set: ParallelStringSet) -> Self {
        let ptr = set.into_raw().cast().as_ptr();
        FfiParallelStringSet { ptr }
    }
}

impl TryFrom<FfiParallelStringSet> for ParallelStringSet {
    type Error = SetError;

    fn try_from(ffi: FfiParallelStringSet) -> Result<Self, Self::Error> {
        match NonNull::new(ffi.ptr.cast()) {
            // SAFETY: as long as FFI upholds all the invariants, we've
            // round-tripped correctly.
            Some(raw) => unsafe { Ok(Self::from_raw(raw)) },
            None => Err(SetError::InvalidArgument),
        }
    }
}

impl Drop for FfiParallelStringSet {
    fn drop(&mut self) {
        if let Ok(set) = unsafe { Self::try_unwrap_set(Some(self)) } {
            // Since this is an FFI type, set this pointer to null to limit
            // the impact of double-drop and use-after-free.
            self.ptr = null_mut();
            drop(ManuallyDrop::into_inner(set));
        }
    }
}



impl FfiParallelStringSet {
    unsafe fn try_insert(
        set: Option<&FfiParallelStringSet>,
        str: CharSlice,
        utf8_options: Utf8Option,
    ) -> Result<StringId, SetError> {
        let set = set.ok_or(SetError::InvalidArgument)?;
        let slice = str.try_as_slice().ok_or(SetError::InvalidArgument)?;
        // Convert from &[i8] to &[u8]
        let slice = unsafe { std::slice::from_raw_parts(slice.as_ptr() as *const u8, slice.len()) };
        let string = match utf8_options {
            Utf8Option::Assume => {
                // SAFETY: the caller is asserting the data is valid UTF-8.
                Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(slice) })
            }
            Utf8Option::ConvertLossy => try_from_utf8_lossy(slice)?,
            Utf8Option::Validate => {
                Cow::Borrowed(std::str::from_utf8(slice).map_err(|_| SetError::InvalidArgument)?)
            }
        };

        let set = unsafe { Self::try_unwrap_set(Some(set))? };
        set.try_insert(string.as_ref())
    }

    unsafe fn try_clone(
        set: Option<&FfiParallelStringSet>,
    ) -> Result<FfiParallelStringSet, SetError> {
        let set_md = FfiParallelStringSet::try_unwrap_set(set)?;
        let cloned = set_md.try_clone()?;
        Ok(FfiParallelStringSet::from(cloned))
    }

    /// # Safety
    /// The caller must ensure that the pointer is valid or null.
    unsafe fn try_unwrap_set(
        set: Option<&FfiParallelStringSet>,
    ) -> Result<ManuallyDrop<ParallelStringSet>, SetError> {
        let ffi = set.ok_or(SetError::InvalidArgument)?;
        match NonNull::new(ffi.ptr) {
            Some(raw) => Ok(ManuallyDrop::new(ParallelStringSet::from_raw(raw.cast()))),
            None => Err(SetError::InvalidArgument),
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_prof_ParallelStringSet_new() -> FfiParallelStringSet {
    match ParallelStringSet::try_new() {
        Ok(set) => FfiParallelStringSet::from(set),
        Err(_) => FfiParallelStringSet::default(),
    }
}

/// # Safety
/// The caller must ensure that the pointer is valid or null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelStringSet_try_clone(
    set: Option<&FfiParallelStringSet>,
) -> FfiParallelStringSet {
    FfiParallelStringSet::try_clone(set).unwrap_or_default()
}

/// # Safety
/// The caller must ensure that the pointer is valid or null, and that the
/// slice is valid for the given length.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelStringSet_try_insert(
    set: Option<&FfiParallelStringSet>,
    str: CharSlice,
    utf8_options: Utf8Option,
) -> ParallelStringSetTryInsertResult {
    FfiParallelStringSet::try_insert(set, str, utf8_options).into()
}

/// # Safety
/// The caller must ensure that the pointer is valid or null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelStringSet_drop(ptr: *mut FfiParallelStringSet) {
    if !ptr.is_null() {
        let ffi = &mut *ptr;
        if let Ok(set) = FfiParallelStringSet::try_unwrap_set(Some(ffi)) {
            // Since this is an FFI type, set this pointer to null to limit
            // the impact of double-drop and use-after-free.
            ffi.ptr = null_mut();
            drop(std::mem::ManuallyDrop::into_inner(set));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::slice::CharSlice;

    #[test]
    fn test_ffi() {
        let mut ffi = ddog_prof_ParallelStringSet_new();
        let quotes = [
            "I know that I am intelligent, because I know that I know nothing.",
            "Success is not something to wait for, it is something to work for.",
            "Relationships are based on four principles: Respect, Understanding, Acceptance, and Appreciation.",
            "A hero is human who does what he can.",
            "One who believes in himself has no need to convince others.",
        ];

        // Test reference counting by creating multiple clones
        let clone1 = unsafe { ddog_prof_ParallelStringSet_try_clone(Some(&ffi)) };
        let clone2 = unsafe { ddog_prof_ParallelStringSet_try_clone(Some(&ffi)) };
        let clone3 = unsafe { ddog_prof_ParallelStringSet_try_clone(Some(&clone1)) };

        for quote in quotes {
            let slice = CharSlice::from(quote);
            let result = unsafe {
                ddog_prof_ParallelStringSet_try_insert(Some(&ffi), slice, Utf8Option::Assume)
            };
            let id = match result {
                ParallelStringSetTryInsertResult::Ok(ok) => ok,
                ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
            };

            // Test that the same string inserted in one set is found in all clones
            match unsafe {
                ddog_prof_ParallelStringSet_try_insert(Some(&clone1), slice, Utf8Option::Validate)
            } {
                ParallelStringSetTryInsertResult::Ok(id2) => {
                    assert_eq!(&*id.0, &*id2.0)
                }
                ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
            }

            match unsafe {
                ddog_prof_ParallelStringSet_try_insert(
                    Some(&clone2),
                    slice,
                    Utf8Option::ConvertLossy,
                )
            } {
                ParallelStringSetTryInsertResult::Ok(id2) => {
                    assert_eq!(&*id.0, &*id2.0)
                }
                ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
            }

            match unsafe {
                ddog_prof_ParallelStringSet_try_insert(Some(&clone3), slice, Utf8Option::Validate)
            } {
                ParallelStringSetTryInsertResult::Ok(id2) => {
                    assert_eq!(&*id.0, &*id2.0)
                }
                ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
            }
        }

        // Test reference counting by dropping clones in different order
        drop(clone2); // Drop middle clone first

        // Insert a new string after dropping one clone
        let refcount_test_slice = CharSlice::from("refcount_test_string");
        let refcount_id = match unsafe {
            ddog_prof_ParallelStringSet_try_insert(
                Some(&ffi),
                refcount_test_slice,
                Utf8Option::Assume,
            )
        } {
            ParallelStringSetTryInsertResult::Ok(id) => id,
            ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
        };

        // Should still be accessible from remaining clones
        match unsafe {
            ddog_prof_ParallelStringSet_try_insert(
                Some(&clone1),
                refcount_test_slice,
                Utf8Option::Validate,
            )
        } {
            ParallelStringSetTryInsertResult::Ok(id2) => {
                assert_eq!(&*refcount_id.0, &*id2.0)
            }
            ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
        }

        match unsafe {
            ddog_prof_ParallelStringSet_try_insert(
                Some(&clone3),
                refcount_test_slice,
                Utf8Option::Validate,
            )
        } {
            ParallelStringSetTryInsertResult::Ok(id2) => {
                assert_eq!(&*refcount_id.0, &*id2.0)
            }
            ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
        }

        // Drop original set
        unsafe { ddog_prof_ParallelStringSet_drop(&mut ffi) };

        // Should still work with remaining clones (testing that refcount keeps data alive)
        match unsafe {
            ddog_prof_ParallelStringSet_try_insert(
                Some(&clone1),
                refcount_test_slice,
                Utf8Option::Validate,
            )
        } {
            ParallelStringSetTryInsertResult::Ok(id2) => {
                assert_eq!(&*refcount_id.0, &*id2.0)
            }
            ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
        }

        drop(clone1);

        // Last clone should still work
        match unsafe {
            ddog_prof_ParallelStringSet_try_insert(
                Some(&clone3),
                refcount_test_slice,
                Utf8Option::Validate,
            )
        } {
            ParallelStringSetTryInsertResult::Ok(id2) => {
                assert_eq!(&*refcount_id.0, &*id2.0)
            }
            ParallelStringSetTryInsertResult::Err(err) => panic!("{err}"),
        }

        drop(clone3); // Final cleanup
    }

    #[test]
    fn test_ffi_error_handling() {
        // Test with null set
        let null_ffi = FfiParallelStringSet::default();
        let slice = CharSlice::from("test");

        let result = unsafe {
            ddog_prof_ParallelStringSet_try_insert(Some(&null_ffi), slice, Utf8Option::Validate)
        };

        match result {
            ParallelStringSetTryInsertResult::Err(SetError::InvalidArgument) => {}
            _ => panic!("Expected InvalidArgument error for null set"),
        }

        // Test with None set
        let result =
            unsafe { ddog_prof_ParallelStringSet_try_insert(None, slice, Utf8Option::Validate) };

        match result {
            ParallelStringSetTryInsertResult::Err(SetError::InvalidArgument) => {}
            _ => panic!("Expected InvalidArgument error for None set"),
        }
    }

    #[test]
    fn test_utf8_validation() {
        let ffi = ddog_prof_ParallelStringSet_new();

        // Test valid UTF-8
        let valid_utf8 = "Hello, 世界! 🦀";
        let slice = CharSlice::from(valid_utf8);

        let result = unsafe {
            ddog_prof_ParallelStringSet_try_insert(Some(&ffi), slice, Utf8Option::Validate)
        };

        match result {
            ParallelStringSetTryInsertResult::Ok(_) => {}
            ParallelStringSetTryInsertResult::Err(err) => {
                panic!("Valid UTF-8 should not fail: {}", err)
            }
        }

        // Test invalid UTF-8 bytes
        let invalid_bytes = [0xFF, 0xFE, 0xFD];
        let invalid_slice = unsafe {
            super::slice::Slice::from_raw_parts(
                invalid_bytes.as_ptr() as *const i8,
                invalid_bytes.len(),
            )
        };

        let result = unsafe {
            ddog_prof_ParallelStringSet_try_insert(Some(&ffi), invalid_slice, Utf8Option::Validate)
        };

        match result {
            ParallelStringSetTryInsertResult::Err(SetError::InvalidArgument) => {}
            _ => panic!("Invalid UTF-8 should fail with Validate option"),
        }
    }
}
