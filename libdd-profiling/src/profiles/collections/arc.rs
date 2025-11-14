// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// This is heavily inspired by the standard library's `Arc` implementation,
// which is dual-licensed as Apache-2.0 or MIT.

use allocator_api2::alloc::{AllocError, Allocator, Global};
use allocator_api2::boxed::Box;
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use core::{alloc::Layout, fmt, mem::ManuallyDrop, ptr};
use core::{marker::PhantomData, ops::Deref, ptr::NonNull};
use crossbeam_utils::CachePadded;

/// A thread-safe reference-counting pointer with only strong references.
///
/// This type is similar to `std::sync::Arc` but intentionally omits APIs that
/// can panic or abort the process. In particular:
/// - There are no weak references.
/// - Cloning uses [`Arc::try_clone`], which returns an error on reference-count overflow instead of
///   aborting the process.
/// - Construction uses fallible allocation via [`Arc::try_new`].
///
/// Deref gives shared access to the inner value; mutation should use interior
/// mutability primitives as with `std::sync::Arc`.
#[repr(C)]
#[derive(Debug)]
pub struct Arc<T, A: Allocator = Global> {
    ptr: NonNull<ArcInner<T>>,
    alloc: A,
    phantom: PhantomData<ArcInner<T>>,
}

// repr(C) prevents field reordering that could affect raw-pointer helpers.
#[repr(C)]
struct ArcInner<T> {
    refcount: CachePadded<AtomicUsize>,
    data: CachePadded<T>,
}

impl<T> ArcInner<T> {
    fn from_ptr<'a>(ptr: *const T) -> &'a Self {
        let data = ptr.cast::<u8>();
        let data_offset = Arc::<T>::data_offset();
        let bytes_ptr = unsafe { data.sub(data_offset) };
        let arc_ptr = bytes_ptr as *mut ArcInner<T>;
        unsafe { &*arc_ptr }
    }

    fn try_clone(&self) -> Result<(), ArcOverflow> {
        if self.refcount.fetch_add(1, Ordering::Relaxed) > MAX_REFCOUNT {
            self.refcount.fetch_sub(1, Ordering::Relaxed);
            return Err(ArcOverflow);
        }
        Ok(())
    }
}

impl<T> Arc<T> {
    pub fn try_new(data: T) -> Result<Arc<T, Global>, AllocError> {
        Self::try_new_in(data, Global)
    }

    /// Tries to increment the reference count using only a pointer to the
    /// inner `T`. This does not create an `Arc<T>` instance.
    ///
    /// # Safety
    /// - `ptr` must be a valid pointer to the `T` inside an `Arc<T>` allocation produced by this
    ///   module. Passing any other pointer is undefined behavior.
    /// - There must be at least one existing reference alive when called.
    pub unsafe fn try_increment_count(ptr: *const T) -> Result<(), ArcOverflow> {
        let inner = ArcInner::from_ptr(ptr);
        inner.try_clone()
    }
}

impl<T, A: Allocator> Arc<T, A> {
    /// Constructs a new `Arc<T, A>` in the provided allocator, returning an
    /// error if allocation fails.
    pub fn try_new_in(data: T, alloc: A) -> Result<Arc<T, A>, AllocError> {
        let inner = ArcInner {
            refcount: CachePadded::new(AtomicUsize::new(1)),
            data: CachePadded::new(data),
        };
        let boxed = Box::try_new_in(inner, alloc)?;
        let (ptr, alloc) = Box::into_non_null(boxed);
        Ok(Arc {
            ptr,
            alloc,
            phantom: PhantomData,
        })
    }

    /// Returns the inner value, if the `Arc` has exactly one reference.
    ///
    /// Otherwise, an [`Err`] is returned with the same `Arc` that was passed
    /// in.
    ///
    /// It is strongly recommended to use [`Arc::into_inner`] instead if you
    /// don't keep the `Arc` in the [`Err`] case.
    pub fn try_unwrap(this: Self) -> Result<T, Self> {
        // Attempt to take unique ownership by transitioning strong: 1 -> 0
        let inner_ref = unsafe { this.ptr.as_ref() };
        if inner_ref
            .refcount
            .compare_exchange(1, 0, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // We have unique ownership; move out T and deallocate without dropping T.
            let this = ManuallyDrop::new(this);
            let ptr = this.ptr.as_ptr();
            let alloc: A = unsafe { ptr::read(&this.alloc) };
            // Reconstruct a Box to ArcInner and convert into inner to avoid double-drop of T
            let boxed: Box<ArcInner<T>, A> = unsafe { Box::from_raw_in(ptr, alloc) };
            let ArcInner { refcount: _, data } = Box::into_inner(boxed);
            // We moved out `data` above, so do not use `data` here; free already done via
            // into_inner
            Ok(CachePadded::into_inner(data))
        } else {
            Err(this)
        }
    }

    pub fn into_inner(this: Self) -> Option<T> {
        // Prevent running Drop; we will manage the refcount and allocation manually.
        let this = ManuallyDrop::new(this);
        let inner = unsafe { this.ptr.as_ref() };
        if inner.refcount.fetch_sub(1, Ordering::Release) != 1 {
            return None;
        }
        fence(Ordering::Acquire);

        // We are the last strong reference. Move out T and free the allocation
        // without dropping T.
        let ptr = this.ptr.as_ptr();
        let alloc: A = unsafe { ptr::read(&this.alloc) };
        let boxed: Box<ArcInner<T>, A> = unsafe { Box::from_raw_in(ptr, alloc) };
        let ArcInner { refcount: _, data } = Box::into_inner(boxed);
        Some(CachePadded::into_inner(data))
    }

    /// Returns a raw non-null pointer to the inner value. The pointer is valid
    /// as long as there is at least one strong reference alive.
    #[inline]
    pub fn as_ptr(&self) -> NonNull<T> {
        let ptr = NonNull::as_ptr(self.ptr);
        // SAFETY: `ptr` points to a valid `ArcInner<T>` allocation. Taking the
        // address of the `data` field preserves provenance unlike going
        // through Deref.
        let data = unsafe { ptr::addr_of_mut!((*ptr).data) };
        // SAFETY: data field address is derived from a valid NonNull.
        unsafe { NonNull::new_unchecked(data as *mut T) }
    }

    /// Converts the Arc into a non-null pointer to the inner value, without
    /// decreasing the reference count.
    ///
    /// The caller must later call `Arc::from_raw` with the same pointer exactly
    /// once to avoid leaking the allocation.
    #[inline]
    #[must_use = "losing the pointer will leak memory"]
    pub fn into_raw(this: Self) -> NonNull<T> {
        let this = ManuallyDrop::new(this);
        // Reuse as_ptr logic without dropping `this`.
        Arc::as_ptr(&this)
    }
}

// SAFETY: `Arc<T, A>` is Send and Sync iff `T` is Send and Sync.
unsafe impl<T: Send + Sync, A: Allocator> Send for Arc<T, A> {}
unsafe impl<T: Send + Sync, A: Allocator> Sync for Arc<T, A> {}

impl<T, A: Allocator> Arc<T, A> {
    #[inline]
    fn inner(&self) -> &ArcInner<T> {
        // SAFETY: `ptr` is a valid, live allocation managed by this Arc
        unsafe { self.ptr.as_ref() }
    }
}

/// Error returned when the reference count would overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArcOverflow;

impl fmt::Display for ArcOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("arc: reference count overflow")
    }
}

impl core::error::Error for ArcOverflow {}

/// A limit on the amount of references that may be made to an `Arc`.
const MAX_REFCOUNT: usize = isize::MAX as usize;

impl<T, A: Allocator + Clone> Arc<T, A> {
    /// Fallible clone that increments the strong reference count.
    ///
    /// Returns an error if the reference count would exceed `isize::MAX`.
    pub fn try_clone(&self) -> Result<Self, ArcOverflow> {
        let inner = self.inner();
        inner.try_clone()?;
        Ok(Arc {
            ptr: self.ptr,
            alloc: self.alloc.clone(),
            phantom: PhantomData,
        })
    }
}

impl<T, A: Allocator> Drop for Arc<T, A> {
    fn drop(&mut self) {
        let inner = self.inner();
        if inner.refcount.fetch_sub(1, Ordering::Release) == 1 {
            // Synchronize with other threads that might have modified the data
            // before dropping the last strong reference.
            // Raymond Chen wrote a little blog article about it:
            // https://devblogs.microsoft.com/oldnewthing/20251015-00/?p=111686
            fence(Ordering::Acquire);
            // SAFETY: this was the last strong reference; reclaim allocation
            let ptr = self.ptr.as_ptr();
            // Move out allocator for deallocation, but prevent double-drop of `alloc`
            let alloc: A = unsafe { ptr::read(&self.alloc) };
            unsafe { drop(Box::<ArcInner<T>, A>::from_raw_in(ptr, alloc)) };
        }
    }
}

impl<T, A: Allocator> Deref for Arc<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The allocation outlives `self` while any strong refs exist.
        unsafe { &self.ptr.as_ref().data }
    }
}

impl<T, A: Allocator> Arc<T, A> {
    #[inline]
    fn data_offset() -> usize {
        // Layout of ArcInner<T> is repr(C): [CachePadded<AtomicUsize>, CachePadded<T>]
        let header = Layout::new::<CachePadded<AtomicUsize>>();
        match header.extend(Layout::new::<CachePadded<T>>()) {
            Ok((_layout, offset)) => offset,
            Err(_) => {
                // Fallback: compute padding manually to avoid unwrap. This should
                // not fail in practice for valid types.
                let align = Layout::new::<CachePadded<T>>().align();
                let size = header.size();
                let padding = (align - (size % align)) % align;
                size + padding
            }
        }
    }

    /// Recreates an `Arc<T, A>` from a raw pointer produced by `Arc::into_raw`.
    ///
    /// # Safety
    /// - `ptr` must have been returned by a previous call to `Arc::<T, A>::into_raw`.
    /// - if `ptr` has been cast, it needs to be to a compatible repr.
    /// - It must not be used to create multiple owning `Arc`s without corresponding `into_raw`
    ///   calls; otherwise the refcount will be decremented multiple times.
    #[inline]
    pub unsafe fn from_raw_in(ptr: NonNull<T>, alloc: A) -> Self {
        let data = ptr.as_ptr() as *const u8;
        let arc_ptr_u8 = data.sub(Self::data_offset());
        let arc_ptr = arc_ptr_u8 as *mut ArcInner<T>;
        Arc {
            ptr: NonNull::new_unchecked(arc_ptr),
            alloc,
            phantom: PhantomData,
        }
    }
}

impl<T> Arc<T> {
    /// Recreates an `Arc<T>` in the `Global` allocator from a raw pointer
    /// produced by `Arc::into_raw`.
    ///
    /// # Safety
    /// See [`Arc::from_raw_in`] for requirements.
    #[inline]
    pub unsafe fn from_raw(ptr: NonNull<T>) -> Self {
        Arc::from_raw_in(ptr, Global)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_new_and_unwrap_unique() {
        let arc = Arc::try_new(123u32).unwrap();
        let v = Arc::try_unwrap(arc).ok().unwrap();
        assert_eq!(v, 123);
    }

    #[test]
    fn try_unwrap_fails_when_shared() {
        let arc = Arc::try_new(5usize).unwrap();
        let arc2 = arc.try_clone().unwrap();
        assert!(Arc::try_unwrap(arc).is_err());
        assert_eq!(*arc2, 5);
    }

    #[test]
    fn deref_access() {
        let arc = Arc::try_new("abc").unwrap();
        assert_eq!(arc.len(), 3);
        assert_eq!(*arc, "abc");
    }
}
