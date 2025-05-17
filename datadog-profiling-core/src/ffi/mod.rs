// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::protobuf::{Buffer, StringTable};
use allocator_api2::alloc::{Allocator, Layout};
use allocator_api2::collections::TryReserveErrorKind::{AllocError, CapacityOverflow};
use core::alloc::LayoutError;
use core::{cmp, mem, ptr, slice};
use datadog_alloc::VirtualAllocator;

#[repr(C)]
#[derive(Debug)]
pub struct FfiBuffer {
    ptr: ptr::NonNull<u8>,
    capacity: usize,
    len: usize,
}

unsafe impl Send for FfiBuffer {}

#[repr(C)]
enum TryReserveError {
    CapacityOverflow,
    AllocError,
}

impl FfiBuffer {
    const fn new() -> Self {
        Self {
            ptr: ptr::NonNull::dangling(),
            capacity: 0,
            len: 0,
        }
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let len = self.len;
        if self.needs_to_grow(len, additional) {
            self.try_grow(len, additional)?
        }
        Ok(())
    }

    /// # Safety
    /// The caller needs to ensure there is enough space before calling this
    /// function (such as after a successful [Self::try_reserve]).
    unsafe fn extend_within_capacity(&mut self, data: &[u8]) {
        let len = self.len;
        let additional = 1;

        // SAFETY: valid pointer due to reserved capacity.
        let begin = unsafe { self.ptr.add(len) };

        // SAFETY: caller is required to ensure enough capacity, and since
        // we're adding to unused capacity, it's not possible for the input to
        // alias this space.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), begin.as_ptr(), additional) };

        self.len = len + additional;
    }
}

impl FfiBuffer {
    #[inline(always)]
    fn needs_to_grow(&self, len: usize, additional: usize) -> bool {
        additional > self.capacity.wrapping_sub(len)
    }

    #[inline(always)]
    fn current_memory(&self) -> Option<(ptr::NonNull<u8>, Layout)> {
        // We have an allocated chunk of memory, so we can bypass runtime
        // checks to get our current layout.
        unsafe {
            let layout = Layout::array::<u8>(self.capacity).unwrap_unchecked();
            Some((self.ptr.cast(), layout))
        }
    }

    #[inline(always)]
    fn set_ptr_and_cap(&mut self, ptr: ptr::NonNull<[u8]>) {
        self.ptr = unsafe { ptr::NonNull::new_unchecked(ptr.cast().as_ptr()) };
        self.capacity = ptr.len() / core::mem::size_of::<u8>();
    }

    #[inline(never)]
    #[cold]
    fn try_grow(&mut self, len: usize, additional: usize) -> Result<(), TryReserveError> {
        debug_assert!(additional > 0);
        let required_cap = len
            .checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)?;
        // This guarantees exponential growth. The doubling cannot overflow
        // because `cap <= isize::MAX` and the type of `cap` is `usize`.
        let cap = cmp::max(self.capacity * 2, required_cap);
        // In the current implementation, the exact number doesn't matter, as
        // long as it's > 1. The virtual allocator is going to give us a much
        // larger capacity.
        let cap = cmp::max(8, cap);
        let new_layout = Layout::array::<u8>(cap);

        // `finish_grow` is non-generic over `T`.
        let ptr = finish_grow(new_layout, self.current_memory())?;
        self.set_ptr_and_cap(ptr);
        Ok(())
    }
}

#[inline(always)]
fn alloc_guard(alloc_size: usize) -> Result<(), TryReserveError> {
    if usize::BITS < 64 && alloc_size > isize::MAX as usize {
        Err(TryReserveError::CapacityOverflow)
    } else {
        Ok(())
    }
}

unsafe fn assume(v: bool) {
    if !v {
        core::unreachable!()
    }
}

#[inline(always)]
fn finish_grow(
    new_layout: Result<Layout, LayoutError>,
    current_memory: Option<(ptr::NonNull<u8>, Layout)>,
) -> Result<ptr::NonNull<[u8]>, TryReserveError> {
    // Check for the error here to minimize the size of `RawVec::grow_*`.
    let new_layout = new_layout.map_err(|_| TryReserveError::CapacityOverflow)?;

    alloc_guard(new_layout.size())?;

    let memory = if let Some((ptr, old_layout)) = current_memory {
        debug_assert_eq!(old_layout.align(), new_layout.align());
        unsafe {
            // The allocator checks for alignment equality
            assume(old_layout.align() == new_layout.align());
            (VirtualAllocator {}).grow(ptr, old_layout, new_layout)
        }
    } else {
        (VirtualAllocator {}).allocate(new_layout)
    };

    memory.map_err(|_| TryReserveError::AllocError)
}

impl Default for FfiBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FfiBuffer {
    fn drop(&mut self) {
        let alloc = VirtualAllocator {};
        // Avoid dropping the dangling pointer
        if self.capacity > 0 {
            // SAFETY: the object has already been allocated, so
            let layout = unsafe { Layout::from_size_align_unchecked(self.capacity, 1) };
            // SAFETY: FfiBuffer always uses VirtualAllocator.
            unsafe { alloc.deallocate(self.ptr, layout) };
        }
    }
}

#[allow(unused)]
#[repr(C)]
pub enum BufferError {
    CapacityOverflow,
    AllocError,
}

#[repr(C)]
pub enum FfiResult<T, E> {
    Ok(T),
    Err(E),
}

#[allow(unused)]
#[repr(C)]
pub enum BufferTryWithCapacityResult {
    Ok(FfiBuffer),
    Err(BufferError),
}

#[no_mangle]
pub extern "C" fn ffi_buffer_new() -> FfiBuffer {
    FfiBuffer::new()
}

/// # Safety
/// Caller needs to not have any references to the buffer's data.
#[no_mangle]
pub unsafe extern "C" fn ffi_buffer_drop(buffer: *mut FfiBuffer) {
    unsafe { ptr::drop_in_place(buffer) };
    unsafe { ptr::write(buffer, FfiBuffer::new()) };
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
pub unsafe extern "C" fn ffi_buffer_try_reserve(
    buffer: *mut FfiBuffer,
    additional: usize,
) -> FfiResult<(), FfiBufferTryReserveError> {
    if buffer.is_null() {
        return FfiResult::Err(FfiBufferTryReserveError::NullBuffer);
    }

    // SAFETY: Caller is required to provide a valid buffer.
    if let Err(err) = unsafe { &mut *buffer }
        .try_reserve(additional)
        .map_err(FfiBufferTryReserveError::from)
    {
        FfiResult::Err(err)
    } else {
        FfiResult::Ok(())
    }
}

#[repr(C)]
pub enum FfiBufferExtendWithinCapacityError {
    NullBuffer,
    NullPointer,
}

/// # Safety
/// Caller needs to not have any references to the buffer's data.
/// Buffer needs to be a legitimate buffer.
/// todo: make this more precise.
#[no_mangle]
pub unsafe extern "C" fn ffi_buffer_extend_within_capacity(
    buffer: *mut FfiBuffer,
    ptr: *const u8,
    len: usize,
) -> FfiResult<(), FfiBufferExtendWithinCapacityError> {
    if buffer.is_null() {
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

    unsafe { (&mut *buffer).extend_within_capacity(slice) };
    FfiResult::Ok(())
}

// #[no_mangle]
// pub extern "C" fn virtual_alloc(len: usize) -> *mut u8 {
//     use core::alloc::Layout;
//     use allocator_api2::alloc::{AllocError, Allocator};
//     let alloc = VirtualAllocator {};
//     let Ok(layout) = Layout::from_size_align(len, core::mem::align_of::<*mut ()>()) else {
//         return ptr::null_mut();
//     };
//
//     match alloc.allocate(layout) {
//         Ok(ok) => ok.as_ptr().cast(),
//         Err(_) => ptr::null_mut(),
//     }
// }

// #[export_name = "ddog_prof_Buffer_try_with_capacity"]
// #[no_panic::no_panic]
// pub extern "C" fn buffer_try_with_capacity(capacity: usize) -> BufferTryWithCapacityResult {
//     match Buffer::try_with_capacity_in(capacity, VirtualAllocator {}) {
//         Ok(buffer) => {
//             let mut buf = mem::ManuallyDrop::new(buffer);
//             BufferTryWithCapacityResult::Ok(FfiBuffer {
//                 ptr: unsafe { ptr::NonNull::new_unchecked(buf.vec.as_mut_ptr()) },
//                 capacity: buf.vec.capacity(),
//                 len: buf.vec.len(),
//             })
//         },
//         Err(err) =>
//             BufferTryWithCapacityResult::Err(match err {
//                 TryReserveError::CapacityOverflow => BufferError::CapacityOverflow,
//                 TryReserveError::AllocError { .. } => BufferError::AllocError,
//             }),
//     }
// }

// #[repr(C)]
// pub enum StringTableNewError {
//
// }

// #[no_mangle]
// pub extern "C" fn string_table_new(buffer: *mut FfiBuffer, capacity: usize) {
//     let buf
//     let mut ht = StringTable::new_in(VirtualAllocator {});
//     ht.in
// }
