// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), no_std)]

use core::cmp;
use core::ffi::{c_int, c_void};
#[cfg(not(test))]
use panic_never as _;

use core::ptr::{self, NonNull};

use libdd_collections::alloc::{AllocError, Allocator, Layout};
use libdd_collections::vec::Vec;
use libdd_collections::{TryReserveError, TryReserveErrorKind};

const OK: c_int = 0;
const NULL_ARGUMENT: c_int = 1;
const CAPACITY_OVERFLOW: c_int = 2;
const ALLOC_ERROR: c_int = 3;
const INTERNAL_ERROR: c_int = 4;
const OUT_OF_BOUNDS: c_int = 5;

const MALLOC_ALIGN: usize = core::mem::align_of::<usize>();

#[derive(Clone, Copy, Debug, Default)]
struct MallocAllocator;

#[cfg_attr(target_vendor = "apple", link(name = "System"))]
#[cfg_attr(not(target_vendor = "apple"), link(name = "c"))]
unsafe extern "C" {
    fn free(ptr: *mut c_void);
    fn malloc(size: usize) -> *mut c_void;
    fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int;
    fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
}

unsafe impl Allocator for MallocAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let ptr = if layout.size() == 0 {
            layout.align() as *mut u8
        } else if layout.align() <= MALLOC_ALIGN {
            // SAFETY: `malloc` is called with the requested non-zero size.
            unsafe { malloc(layout.size()).cast() }
        } else {
            aligned_malloc(layout)?
        };

        let ptr = NonNull::new(ptr).ok_or(AllocError)?;

        Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            // SAFETY: `ptr` came from this allocator.
            unsafe { free(ptr.as_ptr().cast()) };
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let new_ptr = if old_layout.size() == 0 {
            return self.allocate(new_layout);
        } else if old_layout.align() == new_layout.align() && new_layout.align() <= MALLOC_ALIGN {
            // SAFETY: `ptr` came from this allocator and `new_size` is the
            // requested replacement size.
            unsafe { realloc(ptr.as_ptr().cast(), new_layout.size()).cast() }
        } else {
            let new_ptr = self.allocate(new_layout)?.cast::<u8>();

            // SAFETY: `new_layout.size() >= old_layout.size()`, both
            // allocations are valid for `old_layout.size()` bytes, and the new
            // allocation cannot overlap the old one.
            unsafe {
                ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), old_layout.size());
                self.deallocate(ptr, old_layout);
            }

            return Ok(NonNull::slice_from_raw_parts(new_ptr, new_layout.size()));
        };

        let new_ptr = NonNull::new(new_ptr).ok_or(AllocError)?;

        Ok(NonNull::slice_from_raw_parts(new_ptr, new_layout.size()))
    }
}

fn aligned_malloc(layout: Layout) -> Result<*mut u8, AllocError> {
    let mut out = ptr::null_mut();
    let align = cmp::max(layout.align(), core::mem::size_of::<*mut c_void>());

    // SAFETY: `out` is valid for writes, `align` is a power-of-two multiple of
    // pointer size, and `size` is the requested non-zero size.
    let status = unsafe { posix_memalign(&mut out, align, layout.size()) };
    if status == 0 {
        Ok(out.cast())
    } else {
        Err(AllocError)
    }
}

impl MallocAllocator {
    unsafe fn deallocate_handle<T>(ptr: *mut T) {
        let layout = Layout::new::<T>();

        // SAFETY: `ptr` came from this API's matching constructor.
        unsafe {
            ptr::drop_in_place(ptr);
            MallocAllocator.deallocate(NonNull::new_unchecked(ptr).cast(), layout);
        }
    }
}

fn reserve_error_code(error: TryReserveError) -> c_int {
    match error.kind() {
        TryReserveErrorKind::CapacityOverflow => CAPACITY_OVERFLOW,
        TryReserveErrorKind::AllocError { .. } => ALLOC_ERROR,
    }
}

unsafe fn allocate_i64_handle(
    inner: Vec<i64, MallocAllocator>,
    out: *mut *mut DdogCollectionsVecI64,
) -> c_int {
    let layout = Layout::new::<DdogCollectionsVecI64>();
    let allocation = match MallocAllocator.allocate(layout) {
        Ok(allocation) => allocation,
        Err(_) => return ALLOC_ERROR,
    };

    let vec = allocation.cast::<DdogCollectionsVecI64>().as_ptr();

    // SAFETY: `vec` points to a valid allocation large enough for `DdogCollectionsVecI64`,
    // and `out` was checked non-null by the caller.
    unsafe {
        vec.write(DdogCollectionsVecI64 { inner });
        out.write(vec);
    }

    OK
}

struct RawI64Iter {
    ptr: *const i64,
    remaining: usize,
}

impl RawI64Iter {
    fn new(ptr: *const i64, len: usize) -> Result<Self, c_int> {
        if ptr.is_null() && len != 0 {
            Err(NULL_ARGUMENT)
        } else {
            Ok(Self {
                ptr,
                remaining: len,
            })
        }
    }
}

impl Iterator for RawI64Iter {
    type Item = i64;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            None
        } else {
            // SAFETY: callers construct this iterator only from a pointer valid
            // for `remaining` contiguous `i64` values.
            let value = unsafe { *self.ptr };
            // SAFETY: same allocation as above; advancing by one keeps the
            // cursor in bounds or one-past-the-end.
            self.ptr = unsafe { self.ptr.add(1) };
            self.remaining -= 1;
            Some(value)
        }
    }
}

unsafe fn i64_slice_from_raw<'a>(ptr: *const i64, len: usize) -> Result<&'a [i64], c_int> {
    if ptr.is_null() && len != 0 {
        return Err(NULL_ARGUMENT);
    }

    let ptr = if ptr.is_null() {
        NonNull::<i64>::dangling().as_ptr()
    } else {
        ptr
    };

    // SAFETY: the caller guarantees `ptr` is valid for `len` contiguous `i64`
    // values when `len` is non-zero; a dangling non-null pointer is used for
    // the empty case.
    Ok(unsafe { core::slice::from_raw_parts(ptr, len) })
}

#[repr(C)]
pub struct DdogCollectionsVecI64 {
    inner: Vec<i64, MallocAllocator>,
}

#[repr(C)]
pub struct DdogCollectionsVecI32 {
    inner: Vec<i32, MallocAllocator>,
}

/// Creates an empty `i64` vector handle.
///
/// # Safety
///
/// `out` must be null or valid to write one `*mut DdogCollectionsVecI64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_new(
    out: *mut *mut DdogCollectionsVecI64,
) -> c_int {
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: `out` was checked non-null above.
    unsafe { allocate_i64_handle(Vec::new_in(MallocAllocator), out) }
}

/// Creates an empty `i32` vector handle.
///
/// # Safety
///
/// `out` must be null or valid to write one `*mut DdogCollectionsVecI32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i32_new(
    out: *mut *mut DdogCollectionsVecI32,
) -> c_int {
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    let layout = Layout::new::<DdogCollectionsVecI32>();
    let allocation = match MallocAllocator.allocate(layout) {
        Ok(allocation) => allocation,
        Err(_) => return ALLOC_ERROR,
    };

    let vec = allocation.cast::<DdogCollectionsVecI32>().as_ptr();

    // SAFETY: `vec` points to a valid allocation large enough for `DdogCollectionsVecI32`,
    // and `out` was checked non-null above.
    unsafe {
        vec.write(DdogCollectionsVecI32 {
            inner: Vec::new_in(MallocAllocator),
        });
        out.write(vec);
    }

    OK
}

/// Frees a vector handle.
///
/// # Safety
///
/// `vec` must be null or a handle returned by `ddog_collections_vec_i64_new` that has not
/// already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_free(vec: *mut DdogCollectionsVecI64) {
    if vec.is_null() {
        return;
    }

    // SAFETY: `vec` came from `ddog_collections_vec_i64_new`.
    unsafe { MallocAllocator::deallocate_handle(vec) };
}

/// Frees an `i32` vector handle.
///
/// # Safety
///
/// `vec` must be null or a handle returned by `ddog_collections_vec_i32_new` that has not
/// already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i32_free(vec: *mut DdogCollectionsVecI32) {
    if vec.is_null() {
        return;
    }

    // SAFETY: `vec` came from `ddog_collections_vec_i32_new`.
    unsafe { MallocAllocator::deallocate_handle(vec) };
}

/// Reserves capacity for at least `additional` more values.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_reserve(
    vec: *mut DdogCollectionsVecI64,
    additional: usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_reserve(additional) {
        Ok(()) => OK,
        Err(error) => reserve_error_code(error),
    }
}

/// Reserves capacity for at least `additional` more `i32` values.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i32_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i32_reserve(
    vec: *mut DdogCollectionsVecI32,
    additional: usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_reserve(additional) {
        Ok(()) => OK,
        Err(error) => reserve_error_code(error),
    }
}

/// Reserves capacity for exactly `additional` more values.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_reserve_exact(
    vec: *mut DdogCollectionsVecI64,
    additional: usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_reserve_exact(additional) {
        Ok(()) => OK,
        Err(error) => reserve_error_code(error),
    }
}

/// Appends one value.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_push(
    vec: *mut DdogCollectionsVecI64,
    value: i64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    let vec = unsafe { vec.as_mut() };
    match vec.inner.try_reserve(1) {
        Ok(()) => {}
        Err(error) => return reserve_error_code(error),
    }

    match vec.inner.push_within_capacity(value) {
        Ok(_) => OK,
        Err(_) => INTERNAL_ERROR,
    }
}

/// Appends one value using `Vec::try_push`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_try_push(
    vec: *mut DdogCollectionsVecI64,
    value: i64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_push(value) {
        Ok(_) => OK,
        Err(_) => ALLOC_ERROR,
    }
}

/// Creates an `i64` vector by cloning values from a raw slice.
///
/// # Safety
///
/// `values` must be null only when `len == 0`; otherwise, it must be valid for
/// reads of `len` contiguous `i64` values. `out` must be null or valid to
/// write one `*mut DdogCollectionsVecI64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_try_from_slice(
    values: *const i64,
    len: usize,
    out: *mut *mut DdogCollectionsVecI64,
) -> c_int {
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: this function's caller upholds the documented pointer contract.
    let values = match unsafe { i64_slice_from_raw(values, len) } {
        Ok(values) => values,
        Err(error) => return error,
    };

    let inner = match Vec::try_from_slice_in(values, MallocAllocator) {
        Ok(inner) => inner,
        Err(error) => return reserve_error_code(error),
    };

    // SAFETY: `out` was checked non-null above.
    unsafe { allocate_i64_handle(inner, out) }
}

/// Appends as many values from `values` as will fit without allocating.
///
/// Writes the number of uninserted values to `out_uninserted`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
/// `values` must be null only when `len == 0`; otherwise, it must be valid for
/// reads of `len` contiguous `i64` values. `out_uninserted` must be null or
/// valid to write one `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_extend_from_slice_within_capacity(
    vec: *mut DdogCollectionsVecI64,
    values: *const i64,
    len: usize,
    out_uninserted: *mut usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };
    if out_uninserted.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: this function's caller upholds the documented pointer contract.
    let values = match unsafe { i64_slice_from_raw(values, len) } {
        Ok(values) => values,
        Err(error) => return error,
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    let rest = unsafe { vec.as_mut() }
        .inner
        .extend_from_slice_within_capacity(values);

    // SAFETY: `out_uninserted` was checked non-null above.
    unsafe { out_uninserted.write(rest.len()) };
    OK
}

/// Appends as many values from an iterator over `values` as will fit without
/// allocating.
///
/// Writes the number of uninserted values to `out_uninserted`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
/// `values` must be null only when `len == 0`; otherwise, it must be valid for
/// reads of `len` contiguous `i64` values. `out_uninserted` must be null or
/// valid to write one `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_extend_within_capacity(
    vec: *mut DdogCollectionsVecI64,
    values: *const i64,
    len: usize,
    out_uninserted: *mut usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };
    if out_uninserted.is_null() {
        return NULL_ARGUMENT;
    }

    let values = match RawI64Iter::new(values, len) {
        Ok(values) => values,
        Err(error) => return error,
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    let rest = unsafe { vec.as_mut() }.inner.extend_within_capacity(values);

    // SAFETY: `out_uninserted` was checked non-null above.
    unsafe { out_uninserted.write(rest.remaining) };
    OK
}

/// Shrinks the vector as much as possible.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_shrink_to_fit(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_shrink_to_fit() {
        Ok(()) => OK,
        Err(error) => reserve_error_code(error),
    }
}

/// Shrinks the vector to at least `min_capacity`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_shrink_to(
    vec: *mut DdogCollectionsVecI64,
    min_capacity: usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_shrink_to(min_capacity) {
        Ok(()) => OK,
        Err(error) => reserve_error_code(error),
    }
}

/// Resizes the vector with a cloned `value`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_try_resize(
    vec: *mut DdogCollectionsVecI64,
    new_len: usize,
    value: i64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.try_resize(new_len, value) {
        Ok(()) => OK,
        Err(error) => reserve_error_code(error),
    }
}

/// Truncates the vector to `len`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_truncate(
    vec: *mut DdogCollectionsVecI64,
    len: usize,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }.inner.truncate(len);
    OK
}

/// Clears the vector.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_clear(vec: *mut DdogCollectionsVecI64) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }.inner.clear();
    OK
}

/// Pops the last value into `out`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`; `out`
/// must be null or valid to write one `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_pop(
    vec: *mut DdogCollectionsVecI64,
    out: *mut i64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: `vec` is non-null and expected to come from this API.
    let Some(value) = (unsafe { vec.as_mut() }).inner.pop() else {
        return OUT_OF_BOUNDS;
    };

    // SAFETY: `out` was checked non-null above.
    unsafe { out.write(value) };
    OK
}

/// Retains only odd values.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_retain_odd(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }
        .inner
        .retain(|value| value.rem_euclid(2) == 1);
    OK
}

/// Adds one to each value, retaining only even results.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_retain_mut_increment_even(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }.inner.retain_mut(|value| {
        *value = value.wrapping_add(1);
        value.rem_euclid(2) == 0
    });
    OK
}

/// Removes consecutive equal values.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_dedup(vec: *mut DdogCollectionsVecI64) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }.inner.dedup();
    OK
}

/// Removes consecutive values with the same remainder modulo ten.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_dedup_by_mod_10(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }
        .inner
        .dedup_by(|a, b| a.rem_euclid(10) == b.rem_euclid(10));
    OK
}

/// Removes consecutive values with the same parity.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_dedup_by_key_parity(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    unsafe { vec.as_mut() }
        .inner
        .dedup_by_key(|value| value.rem_euclid(2));
    OK
}

/// Clears and recycles the vector allocation into a new vector of the same
/// element type.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_recycle_same(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API. The inner
    // vector is moved out, recycled, and written back before returning.
    unsafe {
        let vec = vec.as_mut();
        let inner = ptr::read(&vec.inner);
        ptr::write(&mut vec.inner, inner.recycle::<i64>());
    }

    OK
}

/// Exercises non-panicking read-only APIs and writes an aggregate to `out`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`; `out`
/// must be null or valid to write one `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_read_api_smoke(
    vec: *const DdogCollectionsVecI64,
    out: *mut i64,
) -> c_int {
    let Some(vec) = NonNull::new(vec.cast_mut()) else {
        return NULL_ARGUMENT;
    };
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: `vec` is non-null and expected to come from this API.
    let vec = unsafe { &vec.as_ref().inner };
    let mut acc = 0_i64;

    if let Some(value) = vec.first() {
        acc = acc.wrapping_add(*value);
    }
    if let Some(value) = vec.last() {
        acc = acc.wrapping_add(*value);
    }
    if let Some((first, rest)) = vec.split_first() {
        acc = acc.wrapping_add(*first);
        acc = acc.wrapping_add(rest.len() as i64);
    }
    if let Some((last, rest)) = vec.split_last() {
        acc = acc.wrapping_add(*last);
        acc = acc.wrapping_add(rest.len() as i64);
    }
    if let Some((left, right)) = vec.split_at_checked(vec.len() / 2) {
        acc = acc.wrapping_add(left.len() as i64);
        acc = acc.wrapping_add(right.len() as i64);
    }
    for value in vec {
        acc = acc.wrapping_add(*value);
    }

    let slice = <Vec<i64, MallocAllocator> as AsRef<[i64]>>::as_ref(vec);
    acc = acc.wrapping_add(slice.len() as i64);

    // SAFETY: `out` was checked non-null above.
    unsafe { out.write(acc) };
    OK
}

/// Exercises non-panicking mutable APIs.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_mut_api_smoke(
    vec: *mut DdogCollectionsVecI64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    let vec = unsafe { &mut vec.as_mut().inner };

    if let Some(value) = vec.first_mut() {
        *value = value.wrapping_add(1);
    }
    if let Some(value) = vec.last_mut() {
        *value = value.wrapping_add(1);
    }
    if let Some((first, rest)) = vec.split_first_mut() {
        *first = first.wrapping_add(rest.len() as i64);
    }
    if let Some((last, rest)) = vec.split_last_mut() {
        *last = last.wrapping_add(rest.len() as i64);
    }
    if let Some((left, right)) = vec.split_at_mut_checked(vec.len() / 2) {
        for value in left {
            *value = value.wrapping_add(1);
        }
        for value in right {
            *value = value.wrapping_add(1);
        }
    }
    for value in vec.iter_mut() {
        *value = value.wrapping_add(1);
    }

    let slice = <Vec<i64, MallocAllocator> as AsMut<[i64]>>::as_mut(vec);
    if let Some(value) = slice.first_mut() {
        *value = value.wrapping_add(1);
    }

    OK
}

/// Consumes the vector handle, drops the remaining owned iterator contents,
/// and writes the first value to `out`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new` that has
/// not already been freed; `out` must be null or valid to write one `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_into_iter_next_then_drop(
    vec: *mut DdogCollectionsVecI64,
    out: *mut i64,
) -> c_int {
    let Some(vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: `vec` is non-null and expected to come from this API. The handle
    // allocation is freed manually after its inner vector is moved out.
    let handle = unsafe { ptr::read(vec.as_ptr()) };
    let mut iter = handle.inner.into_iter();
    let Some(value) = iter.next() else {
        // SAFETY: the handle allocation came from `ddog_collections_vec_i64_new`.
        unsafe {
            MallocAllocator.deallocate(vec.cast(), Layout::new::<DdogCollectionsVecI64>());
        }
        return OUT_OF_BOUNDS;
    };

    // SAFETY: `out` was checked non-null above.
    unsafe { out.write(value) };
    drop(iter);

    // SAFETY: the handle allocation came from `ddog_collections_vec_i64_new`, and the
    // `DdogCollectionsVecI64` value has already been moved out.
    unsafe {
        MallocAllocator.deallocate(vec.cast(), Layout::new::<DdogCollectionsVecI64>());
    }

    OK
}

/// Exercises zero-sized vector operations without allocating elements.
///
/// # Safety
///
/// `out_len` must be null or valid to write one `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_zst_smoke(
    count: usize,
    out_len: *mut usize,
) -> c_int {
    if out_len.is_null() {
        return NULL_ARGUMENT;
    }

    let mut vec = Vec::<(), MallocAllocator>::new_in(MallocAllocator);
    let mut inserted = 0;
    while inserted < count {
        if vec.try_push(()).is_err() {
            return ALLOC_ERROR;
        }
        inserted += 1;
    }
    vec.truncate(count / 2);
    vec.clear();
    vec.try_resize(count, ()).ok();

    // SAFETY: `out_len` was checked non-null above.
    unsafe { out_len.write(vec.len()) };
    OK
}

/// Writes the current vector length to `out`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`; `out`
/// must be null or valid to write one `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_len(
    vec: *const DdogCollectionsVecI64,
    out: *mut usize,
) -> c_int {
    let Some(vec) = NonNull::new(vec.cast_mut()) else {
        return NULL_ARGUMENT;
    };
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: pointers are non-null and expected to come from this API.
    unsafe { out.write(vec.as_ref().inner.len()) };
    OK
}

/// Writes the value at `index` to `out`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`; `out`
/// must be null or valid to write one `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_get(
    vec: *const DdogCollectionsVecI64,
    index: usize,
    out: *mut i64,
) -> c_int {
    let Some(vec) = NonNull::new(vec.cast_mut()) else {
        return NULL_ARGUMENT;
    };
    if out.is_null() {
        return NULL_ARGUMENT;
    }

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_ref() }.inner.get(index) {
        Some(value) => {
            // SAFETY: `out` was checked non-null above.
            unsafe { out.write(*value) };
            OK
        }
        None => OUT_OF_BOUNDS,
    }
}

/// Adds `delta` to the value at `index`.
///
/// # Safety
///
/// `vec` must be null or a valid handle returned by `ddog_collections_vec_i64_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_collections_vec_i64_get_mut_add(
    vec: *mut DdogCollectionsVecI64,
    index: usize,
    delta: i64,
) -> c_int {
    let Some(mut vec) = NonNull::new(vec) else {
        return NULL_ARGUMENT;
    };

    // SAFETY: `vec` is non-null and expected to come from this API.
    match unsafe { vec.as_mut() }.inner.get_mut(index) {
        Some(value) => {
            *value = value.wrapping_add(delta);
            OK
        }
        None => OUT_OF_BOUNDS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_i64_vec() -> *mut DdogCollectionsVecI64 {
        let mut vec = core::ptr::null_mut();

        // SAFETY: `&mut vec` is valid to receive the new handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_new(&mut vec) }, OK);
        assert!(!vec.is_null());
        vec
    }

    unsafe fn len(vec: *const DdogCollectionsVecI64) -> usize {
        let mut len = usize::MAX;

        // SAFETY: `vec` is a valid test handle and `&mut len` is writable.
        assert_eq!(unsafe { ddog_collections_vec_i64_len(vec, &mut len) }, OK);
        len
    }

    unsafe fn get(vec: *const DdogCollectionsVecI64, index: usize) -> i64 {
        let mut value = i64::MIN;

        // SAFETY: `vec` is a valid test handle and `&mut value` is writable.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_get(vec, index, &mut value) },
            OK
        );
        value
    }

    unsafe fn from_i64_slice(values: &[i64]) -> *mut DdogCollectionsVecI64 {
        let mut vec = core::ptr::null_mut();

        // SAFETY: `values` is a valid slice and `&mut vec` can receive the
        // handle.
        assert_eq!(
            unsafe {
                ddog_collections_vec_i64_try_from_slice(values.as_ptr(), values.len(), &mut vec)
            },
            OK
        );
        assert!(!vec.is_null());
        vec
    }

    unsafe fn assert_values(vec: *const DdogCollectionsVecI64, expected: &[i64]) {
        // SAFETY: `vec` is a valid test handle.
        assert_eq!(unsafe { len(vec) }, expected.len());

        for (index, expected) in expected.iter().copied().enumerate() {
            // SAFETY: the loop bounds only check valid indices.
            assert_eq!(unsafe { get(vec, index) }, expected);
        }
    }

    #[test]
    fn ffi_push_and_try_push_are_both_smoked() {
        let vec = new_i64_vec();

        // SAFETY: `vec` is a valid handle returned above.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_reserve_exact(vec, 1) },
            OK
        );
        // SAFETY: `vec` is a valid handle returned above. This path reserves
        // first and then calls `push_within_capacity`.
        assert_eq!(unsafe { ddog_collections_vec_i64_push(vec, 10) }, OK);
        // SAFETY: `vec` is a valid handle returned above. This path directly
        // calls `Vec::try_push`.
        assert_eq!(unsafe { ddog_collections_vec_i64_try_push(vec, 20) }, OK);

        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[10, 20]) };

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_try_from_slice_read_mut_and_get_apis_are_smoked() {
        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 2, 3]) };
        let mut acc = i64::MIN;

        // SAFETY: `vec` is a valid handle and `&mut acc` is writable.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_read_api_smoke(vec, &mut acc) },
            OK
        );
        assert_eq!(acc, 24);

        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_mut_api_smoke(vec) }, OK);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[7, 4, 8]) };

        // SAFETY: `vec` is a valid handle and index 1 is in bounds.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_get_mut_add(vec, 1, 10) },
            OK
        );
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[7, 14, 8]) };

        let mut value = i64::MIN;
        // SAFETY: `vec` is valid, but index 99 is out of bounds.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_get(vec, 99, &mut value) },
            OUT_OF_BOUNDS
        );
        // SAFETY: `vec` is valid, but index 99 is out of bounds.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_get_mut_add(vec, 99, 1) },
            OUT_OF_BOUNDS
        );

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_extend_from_slice_within_capacity_returns_uninserted_count() {
        let vec = new_i64_vec();
        let values = [30, 40, 50];
        let mut uninserted = usize::MAX;

        // SAFETY: `vec` is a valid handle returned above.
        assert_eq!(unsafe { ddog_collections_vec_i64_push(vec, 10) }, OK);
        // SAFETY: `vec` is a valid handle returned above.
        assert_eq!(unsafe { ddog_collections_vec_i64_push(vec, 20) }, OK);

        // SAFETY: all pointers are valid and `values` lives for this call.
        assert_eq!(
            unsafe {
                ddog_collections_vec_i64_extend_from_slice_within_capacity(
                    vec,
                    values.as_ptr(),
                    values.len(),
                    &mut uninserted,
                )
            },
            OK
        );

        assert_eq!(uninserted, 1);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { len(vec) }, 4);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 0) }, 10);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 1) }, 20);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 2) }, 30);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 3) }, 40);

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_extend_within_capacity_returns_uninserted_count() {
        let vec = new_i64_vec();
        let values = [30, 40, 50];
        let mut uninserted = usize::MAX;

        // SAFETY: `vec` is a valid handle returned above.
        assert_eq!(unsafe { ddog_collections_vec_i64_push(vec, 10) }, OK);
        // SAFETY: `vec` is a valid handle returned above.
        assert_eq!(unsafe { ddog_collections_vec_i64_push(vec, 20) }, OK);

        // SAFETY: all pointers are valid and `values` lives for this call.
        assert_eq!(
            unsafe {
                ddog_collections_vec_i64_extend_within_capacity(
                    vec,
                    values.as_ptr(),
                    values.len(),
                    &mut uninserted,
                )
            },
            OK
        );

        assert_eq!(uninserted, 1);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { len(vec) }, 4);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 0) }, 10);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 1) }, 20);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 2) }, 30);
        // SAFETY: `vec` is still live and contains the checked indices.
        assert_eq!(unsafe { get(vec, 3) }, 40);

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_resize_truncate_pop_and_clear_are_smoked() {
        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 2]) };

        // SAFETY: `vec` is a valid handle.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_try_resize(vec, 4, 7) },
            OK
        );
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 2, 7, 7]) };

        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_truncate(vec, 3) }, OK);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 2, 7]) };

        let mut popped = i64::MIN;
        // SAFETY: `vec` is non-empty and `&mut popped` is writable.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_pop(vec, &mut popped) },
            OK
        );
        assert_eq!(popped, 7);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 2]) };

        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_clear(vec) }, OK);
        // SAFETY: `vec` remains valid.
        assert_eq!(unsafe { len(vec) }, 0);
        // SAFETY: `vec` is empty and `&mut popped` is writable.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_pop(vec, &mut popped) },
            OUT_OF_BOUNDS
        );

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_retain_and_dedup_apis_are_smoked() {
        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 2, 3, 4, 5]) };

        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_retain_odd(vec) }, OK);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 3, 5]) };
        // SAFETY: `vec` is a valid handle.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_retain_mut_increment_even(vec) },
            OK
        );
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[2, 4, 6]) };

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };

        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 1, 2, 2, 3]) };
        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_dedup(vec) }, OK);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 2, 3]) };
        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };

        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 11, 2, 12, 3]) };
        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_dedup_by_mod_10(vec) }, OK);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 2, 3]) };
        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };

        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 3, 2, 4, 5, 7, 8]) };
        // SAFETY: `vec` is a valid handle.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_dedup_by_key_parity(vec) },
            OK
        );
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[1, 2, 5, 8]) };
        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_shrink_and_recycle_apis_are_smoked() {
        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[1, 2, 3]) };

        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_reserve(vec, 32) }, OK);
        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_shrink_to(vec, 2) }, OK);
        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_shrink_to_fit(vec) }, OK);
        // SAFETY: shrinking preserves contents.
        unsafe { assert_values(vec, &[1, 2, 3]) };

        // SAFETY: `vec` is a valid handle.
        assert_eq!(unsafe { ddog_collections_vec_i64_recycle_same(vec) }, OK);
        // SAFETY: recycle returns an empty vector.
        assert_eq!(unsafe { len(vec) }, 0);
        // SAFETY: the recycled handle remains usable.
        assert_eq!(unsafe { ddog_collections_vec_i64_try_push(vec, 9) }, OK);
        // SAFETY: `vec` remains valid.
        unsafe { assert_values(vec, &[9]) };

        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i64_free(vec) };
    }

    #[test]
    fn ffi_owned_into_iter_consumes_handle_and_drops_rest() {
        // SAFETY: the input slice is valid for the call.
        let vec = unsafe { from_i64_slice(&[9, 10, 11]) };
        let mut first = i64::MIN;

        // SAFETY: `vec` is a valid handle and `&mut first` is writable. This
        // consumes the handle, so it must not be freed again.
        assert_eq!(
            unsafe { ddog_collections_vec_i64_into_iter_next_then_drop(vec, &mut first) },
            OK
        );
        assert_eq!(first, 9);
    }

    #[test]
    fn ffi_zst_smoke_uses_try_push_and_try_resize() {
        let mut len = usize::MAX;

        // SAFETY: `&mut len` is valid to receive the final length.
        assert_eq!(unsafe { ddog_collections_vec_zst_smoke(5, &mut len) }, OK);
        assert_eq!(len, 5);
    }

    #[test]
    fn ffi_i32_reserve_smoke() {
        let mut vec = core::ptr::null_mut();

        // SAFETY: `&mut vec` is valid to receive the new handle.
        assert_eq!(unsafe { ddog_collections_vec_i32_new(&mut vec) }, OK);
        assert!(!vec.is_null());
        // SAFETY: `vec` is a valid handle returned above.
        assert_eq!(unsafe { ddog_collections_vec_i32_reserve(vec, 8) }, OK);
        // SAFETY: `vec` is live and has not been freed yet.
        unsafe { ddog_collections_vec_i32_free(vec) };
    }
}
