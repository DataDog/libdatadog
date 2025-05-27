// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This example exists to demonstrate and check that panics are not generated
//! in release builds at all for this crate. This saves on library size and
//! prevents panics at runtime. This is a very strict thing to do, and it is
//! dependent on many things, including the optimizer and inlining.

// extern crate std;

// Debug assertions cause no_panic to fail.
// Note that generic types can't be #[no_mangle]. Set T to an FFI-safe type.

#[cfg(all(not(miri), not(debug_assertions)))]
mod vec_tests {
    use core::{ptr, slice};
    use datadog_alloc::vec::*;
    use datadog_alloc::TryReserveError;
    use no_panic::no_panic;

    #[repr(C)]
    pub enum FfiResult<T, E> {
        Ok(T),
        Err(E),
    }

    #[no_mangle]
    #[no_panic]
    pub extern "C" fn ffi_vec_new() -> VirtualVec<u8> {
        VirtualVec::<u8>::new()
    }

    /// # Safety
    /// Caller needs to not have any references to the buffer's data.
    #[no_mangle]
    #[no_panic]
    pub unsafe extern "C" fn ffi_vec_drop(buffer: *mut VirtualVec<u8>) {
        unsafe { ptr::drop_in_place(buffer) };
        unsafe { ptr::write(buffer, VirtualVec::new()) };
    }

    #[repr(C)]
    pub enum FfiBufferTryReserveError {
        NullBuffer,
        CapacityOverflow,
        AllocError,
    }

    impl From<TryReserveError> for FfiBufferTryReserveError {
        fn from(value: TryReserveError) -> Self {
            match value {
                TryReserveError::CapacityOverflow => FfiBufferTryReserveError::CapacityOverflow,
                TryReserveError::AllocError => FfiBufferTryReserveError::AllocError,
            }
        }
    }

    /// # Safety
    /// Caller needs to not have any references to the buffer's data.
    /// Buffer needs to be a legitimate buffer.
    /// todo: make this more precise.
    #[no_mangle]
    #[no_panic]
    pub unsafe extern "C" fn ffi_vec_try_reserve(
        buffer: *mut VirtualVec<u8>,
        additional: usize,
    ) -> FfiResult<(), FfiBufferTryReserveError> {
        if buffer.is_null() {
            return FfiResult::Err(FfiBufferTryReserveError::NullBuffer);
        }

        // SAFETY: Caller is required to provide a valid buffer.
        if let Err(err) = unsafe { &mut *buffer }.try_reserve(additional) {
            FfiResult::Err(FfiBufferTryReserveError::from(err))
        } else {
            FfiResult::Ok(())
        }
    }

    #[repr(C)]
    pub enum FfiBufferExtendWithinCapacityError {
        NullBuffer,
        NullPointer,
    }

    #[no_mangle]
    #[no_panic]
    pub unsafe extern "C" fn ffi_vec_extend_within_capacity(
        vec: *mut VirtualVec<u8>,
        ptr: *const u8,
        len: usize,
    ) -> FfiResult<(), FfiBufferExtendWithinCapacityError> {
        if vec.is_null() {
            return FfiResult::Err(FfiBufferExtendWithinCapacityError::NullBuffer);
        }

        let slice = if len == 0 {
            if ptr.is_null() {
                return FfiResult::Err(FfiBufferExtendWithinCapacityError::NullPointer);
            }
            &[]
        } else {
            unsafe { slice::from_raw_parts(ptr, len) }
        };

        unsafe { (&mut *vec).extend_from_slice_within_capacity(slice) };
        FfiResult::Ok(())
    }

    #[no_mangle]
    #[no_panic]
    pub unsafe extern "C" fn ffi_vec_push_within_capacity(
        vec: *mut VirtualVec<u8>,
        value: u8,
    ) -> FfiResult<(), FfiBufferExtendWithinCapacityError> {
        if vec.is_null() {
            return FfiResult::Err(FfiBufferExtendWithinCapacityError::NullBuffer);
        }

        unsafe { (&mut *vec).push_within_capacity(value) };
        FfiResult::Ok(())
    }

    pub fn test() -> Result<(), &'static str> {
        let expected: &[u8] = &[0; 64];
        let mut vec: VirtualVec<u8> = ffi_vec_new();

        match unsafe { ffi_vec_try_reserve(ptr::addr_of_mut!(vec), expected.len()) } {
            FfiResult::Ok(_) => {
                let result = unsafe {
                    ffi_vec_extend_within_capacity(
                        ptr::addr_of_mut!(vec),
                        vec.as_ptr(),
                        expected.len(),
                    )
                };
                if let FfiResult::Err(_) = result {
                    return Err("failed to extend within capacity");
                }

                let mut i = vec.len() as u8;
                for _ in vec.len()..vec.capacity() {
                    let r = unsafe { ffi_vec_push_within_capacity(ptr::addr_of_mut!(vec), i) };
                    if let FfiResult::Err(_) = r {
                        return Err("failed to push within capacity");
                    }
                    i = i.wrapping_add(1);
                }
            }
            FfiResult::Err(_) => return Err("failed to reserve additional capacity"),
        }

        unsafe { ffi_vec_drop(ptr::addr_of_mut!(vec)) };
        // Shouldn't be an issue, the ffi_vec_drop should leave an empty vec in
        // place for this to not cause issues.
        drop(vec);
        Ok(())
    }
}

fn main() {
    #[cfg(all(not(miri), not(debug_assertions)))]
    {
        match vec_tests::test() {
            Ok(_) => println!("success!"),
            Err(err) => eprintln!("ERROR: {err}"),
        }
    }
    #[cfg(miri)]
    {
        eprintln!("no_panic can't be built and run with miri")
    }
    #[cfg(debug_assertions)]
    {
        eprintln!("no_panic can't be run because of debug_assertions")
    }
}
