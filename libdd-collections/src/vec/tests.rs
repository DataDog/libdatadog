// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

use crate::alloc::{AllocError, Global};

use core::cell::Cell;
extern crate std;

const BUCKET_SIZE: usize = 4096;

struct CountingAllocator {
    allocations: Cell<usize>,
    grows: Cell<usize>,
    shrinks: Cell<usize>,
    deallocations: Cell<usize>,
    last_allocate_size: Cell<usize>,
    last_grow_new_size: Cell<usize>,
    last_shrink_new_size: Cell<usize>,
}

impl CountingAllocator {
    const fn new() -> Self {
        Self {
            allocations: Cell::new(0),
            grows: Cell::new(0),
            shrinks: Cell::new(0),
            deallocations: Cell::new(0),
            last_allocate_size: Cell::new(0),
            last_grow_new_size: Cell::new(0),
            last_shrink_new_size: Cell::new(0),
        }
    }
}

// SAFETY: all memory operations are forwarded to `Global`; this wrapper only
// records call counts and does not alter allocator behavior.
unsafe impl Allocator for CountingAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocations.set(self.allocations.get() + 1);
        self.last_allocate_size.set(layout.size());
        Global.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.deallocations.set(self.deallocations.get() + 1);
        // SAFETY: the caller upholds `Allocator::deallocate`'s contract; this
        // wrapper forwards the same pointer and layout to `Global`.
        unsafe { Global.deallocate(ptr, layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.grows.set(self.grows.get() + 1);
        self.last_grow_new_size.set(new_layout.size());
        // SAFETY: the caller upholds `Allocator::grow`'s contract; this wrapper
        // forwards the same pointer and layouts to `Global`.
        unsafe { Global.grow(ptr, old_layout, new_layout) }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.shrinks.set(self.shrinks.get() + 1);
        self.last_shrink_new_size.set(new_layout.size());
        // SAFETY: the caller upholds `Allocator::shrink`'s contract; this
        // wrapper forwards the same pointer and layouts to `Global`.
        unsafe { Global.shrink(ptr, old_layout, new_layout) }
    }
}

struct BucketAllocator {
    allocations: Cell<usize>,
    grows: Cell<usize>,
    shrinks: Cell<usize>,
    deallocations: Cell<usize>,
    current: Cell<Option<(NonNull<u8>, Layout)>>,
}

impl BucketAllocator {
    const fn new() -> Self {
        Self {
            allocations: Cell::new(0),
            grows: Cell::new(0),
            shrinks: Cell::new(0),
            deallocations: Cell::new(0),
            current: Cell::new(None),
        }
    }

    fn bucket_layout(layout: Layout) -> Result<Layout, AllocError> {
        let buckets = layout.size().max(1).div_ceil(BUCKET_SIZE);
        let size = buckets.checked_mul(BUCKET_SIZE).ok_or(AllocError)?;
        Layout::from_size_align(size, layout.align()).map_err(|_| AllocError)
    }

    fn dangling(layout: Layout) -> NonNull<[u8]> {
        let ptr = NonNull::new(layout.align() as *mut u8).unwrap();
        NonNull::slice_from_raw_parts(ptr, 0)
    }
}

// SAFETY: this allocator obtains memory from `Global` using bucket-sized layouts
// and stores the actual layout so fitting smaller layouts can be accepted later.
unsafe impl Allocator for BucketAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocations.set(self.allocations.get() + 1);

        if layout.size() == 0 {
            return Ok(Self::dangling(layout));
        }

        let actual_layout = Self::bucket_layout(layout)?;
        let ptr = Global.allocate(actual_layout)?.cast::<u8>();
        self.current.set(Some((ptr, actual_layout)));
        Ok(NonNull::slice_from_raw_parts(ptr, actual_layout.size()))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.deallocations.set(self.deallocations.get() + 1);

        if layout.size() == 0 {
            return;
        }

        let Some((current_ptr, actual_layout)) = self.current.take() else {
            panic!("deallocated without an active allocation");
        };
        assert_eq!(current_ptr, ptr);

        // SAFETY: this allocator originally allocated `ptr` from `Global` with
        // `actual_layout`, and no active allocation remains after `take`.
        unsafe { Global.deallocate(ptr, actual_layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.grows.set(self.grows.get() + 1);

        if old_layout.size() == 0 {
            if new_layout.size() == 0 {
                return Ok(Self::dangling(new_layout));
            }

            let new_actual_layout = Self::bucket_layout(new_layout)?;
            let new_ptr = Global.allocate(new_actual_layout)?.cast::<u8>();
            self.current.set(Some((new_ptr, new_actual_layout)));
            return Ok(NonNull::slice_from_raw_parts(
                new_ptr,
                new_actual_layout.size(),
            ));
        }

        let Some((current_ptr, old_actual_layout)) = self.current.take() else {
            panic!("grew without an active allocation");
        };
        assert_eq!(current_ptr, ptr);

        let new_actual_layout = Self::bucket_layout(new_layout)?;
        let new_ptr = Global.allocate(new_actual_layout)?.cast::<u8>();

        // SAFETY: `ptr` and `new_ptr` are distinct live allocations, both are
        // valid for at least `old_layout.size()` bytes, and `old_actual_layout`
        // is the exact layout used to allocate `ptr` from `Global`.
        unsafe {
            core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), old_layout.size())
        };
        // SAFETY: `old_actual_layout` is the exact layout used to allocate
        // `ptr` from `Global`, and the contents have been copied above.
        unsafe { Global.deallocate(ptr, old_actual_layout) };

        self.current.set(Some((new_ptr, new_actual_layout)));
        Ok(NonNull::slice_from_raw_parts(
            new_ptr,
            new_actual_layout.size(),
        ))
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.shrinks.set(self.shrinks.get() + 1);

        if old_layout.size() == 0 {
            return Ok(Self::dangling(new_layout));
        }

        let Some((current_ptr, old_actual_layout)) = self.current.take() else {
            panic!("shrank without an active allocation");
        };
        assert_eq!(current_ptr, ptr);

        if new_layout.size() == 0 {
            // SAFETY: `old_actual_layout` is the exact layout used to allocate
            // `ptr` from `Global`, and no active allocation remains after
            // `take`.
            unsafe { Global.deallocate(ptr, old_actual_layout) };
            return Ok(Self::dangling(new_layout));
        }

        let new_actual_layout = Self::bucket_layout(new_layout)?;
        if new_actual_layout.size() == old_actual_layout.size() {
            self.current.set(Some((ptr, old_actual_layout)));
            return Ok(NonNull::slice_from_raw_parts(ptr, old_actual_layout.size()));
        }

        let new_ptr = Global.allocate(new_actual_layout)?.cast::<u8>();

        // SAFETY: `ptr` and `new_ptr` are distinct live allocations, both are
        // valid for at least `new_layout.size()` bytes, and `old_actual_layout`
        // is the exact layout used to allocate `ptr` from `Global`.
        unsafe {
            core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), new_layout.size())
        };
        // SAFETY: `old_actual_layout` is the exact layout used to allocate
        // `ptr` from `Global`, and the contents have been copied above.
        unsafe { Global.deallocate(ptr, old_actual_layout) };

        self.current.set(Some((new_ptr, new_actual_layout)));
        Ok(NonNull::slice_from_raw_parts(
            new_ptr,
            new_actual_layout.size(),
        ))
    }
}

struct ExactCountingAllocator {
    allocations: Cell<usize>,
    grows: Cell<usize>,
    shrinks: Cell<usize>,
}

impl ExactCountingAllocator {
    const fn new() -> Self {
        Self {
            allocations: Cell::new(0),
            grows: Cell::new(0),
            shrinks: Cell::new(0),
        }
    }
}

// SAFETY: this wrapper forwards allocation to `Global` and reports exactly the
// requested layout size to keep vector capacity deterministic in tests.
unsafe impl Allocator for ExactCountingAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocations.set(self.allocations.get() + 1);

        let ptr = Global.allocate(layout)?.cast::<u8>();
        Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller upholds `Allocator::deallocate`'s contract; this
        // wrapper allocated `ptr` from `Global` with the same layout.
        unsafe { Global.deallocate(ptr, layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.grows.set(self.grows.get() + 1);

        // SAFETY: the caller upholds `Allocator::grow`'s contract; this
        // wrapper forwards the same pointer and layouts to `Global`.
        let ptr = unsafe { Global.grow(ptr, old_layout, new_layout) }?.cast::<u8>();
        Ok(NonNull::slice_from_raw_parts(ptr, new_layout.size()))
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.shrinks.set(self.shrinks.get() + 1);

        // SAFETY: the caller upholds `Allocator::shrink`'s contract; this
        // wrapper forwards the same pointer and layouts to `Global`.
        let ptr = unsafe { Global.shrink(ptr, old_layout, new_layout) }?.cast::<u8>();
        Ok(NonNull::slice_from_raw_parts(ptr, new_layout.size()))
    }
}

struct CountingIterator<'a> {
    next_calls: &'a Cell<usize>,
    next: i32,
    end: i32,
}

impl Iterator for CountingIterator<'_> {
    type Item = i32;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_calls.set(self.next_calls.get() + 1);

        if self.next == self.end {
            None
        } else {
            let value = self.next;
            self.next += 1;
            Some(value)
        }
    }
}

#[test]
fn bucket_allocator_uses_dangling_for_zero_sized_allocations() {
    let allocator = BucketAllocator::new();
    let zero = Layout::from_size_align(0, 8).unwrap();

    let block = allocator.allocate(zero).unwrap();

    assert_eq!(block.len(), 0);
    assert_eq!(block.as_ptr().cast::<u8>() as usize % zero.align(), 0);
    assert_eq!(allocator.allocations.get(), 1);
    assert!(allocator.current.get().is_none());

    // SAFETY: `block` came from `allocator.allocate(zero)` above.
    unsafe { allocator.deallocate(block.cast(), zero) };

    assert_eq!(allocator.deallocations.get(), 1);
    assert!(allocator.current.get().is_none());
}

#[test]
fn bucket_allocator_shrink_to_zero_returns_dangling_and_frees_bucket() {
    let allocator = BucketAllocator::new();
    let old = Layout::from_size_align(8, 8).unwrap();
    let zero = Layout::from_size_align(0, 8).unwrap();

    let block = allocator.allocate(old).unwrap();
    assert_eq!(block.len(), BUCKET_SIZE);
    assert!(allocator.current.get().is_some());

    // SAFETY: `block` came from `allocator.allocate(old)` above, and `zero`
    // has the same alignment and a smaller size.
    let shrunk = unsafe { allocator.shrink(block.cast(), old, zero) }.unwrap();

    assert_eq!(shrunk.len(), 0);
    assert_eq!(shrunk.as_ptr().cast::<u8>() as usize % zero.align(), 0);
    assert_eq!(allocator.shrinks.get(), 1);
    assert!(allocator.current.get().is_none());

    // SAFETY: `shrunk` came from `allocator.shrink(..., zero)` above.
    unsafe { allocator.deallocate(shrunk.cast(), zero) };

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn vec_basics() {
    let mut vec = Vec::try_with_capacity_in(1, Global).unwrap();
    assert!(vec.is_empty());
    assert_eq!(vec.len(), 0);
    let capacity = vec.capacity();
    assert!(capacity >= 1);

    vec.push_within_capacity(1).unwrap();

    // Remember the allocator is free to over-size, so the expectations are
    // split based on whether it did or not.
    if capacity == 1 {
        vec.push_within_capacity(2).unwrap_err();
        assert!(!vec.is_empty());
        assert_eq!(vec.len(), 1);
        assert_eq!(vec.capacity(), capacity);
        assert_eq!(vec.as_slice(), &[1]);
    } else {
        vec.push_within_capacity(2).unwrap();
        assert!(!vec.is_empty());
        assert_eq!(vec.len(), 2);
        assert_eq!(vec.capacity(), capacity);
        assert_eq!(vec.as_slice(), &[1, 2]);
    }
    vec.try_reserve(1).unwrap();
}

#[test]
fn try_push_uses_spare_capacity_without_allocating() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    let inserted = vec.try_push(10).unwrap();
    *inserted = 11;
    vec.try_push(20).unwrap();

    assert_eq!(vec.as_slice(), &[11, 20]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_push_grows_from_zero_capacity() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u8, _>::new_in(&allocator);

    let inserted = vec.try_push(5).unwrap();
    *inserted = 7;

    assert_eq!(vec.as_slice(), &[7]);
    assert!(vec.capacity() >= 8);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_push_grows_when_full() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(1, &allocator).unwrap();
    vec.push_within_capacity(10).unwrap();

    let old_capacity = vec.capacity();
    let inserted = vec.try_push(20).unwrap();
    *inserted = 21;

    assert_eq!(vec.as_slice(), &[10, 21]);
    assert!(vec.capacity() > old_capacity);
    assert_eq!(allocator.grows.get(), 1);
}

#[test]
fn try_push_returns_value_when_growth_fails() {
    struct FailingGrowAllocator {
        grows: Cell<usize>,
    }

    // SAFETY: allocation and deallocation use `Global` with the requested
    // layouts; growth deliberately fails to exercise `Vec::try_push`.
    unsafe impl Allocator for FailingGrowAllocator {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            let ptr = Global.allocate(layout)?.cast::<u8>();
            Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
        }

        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            // SAFETY: `allocate` above allocated `ptr` from `Global` with this
            // layout, and the caller upholds the allocator contract.
            unsafe { Global.deallocate(ptr, layout) }
        }

        unsafe fn grow(
            &self,
            _ptr: NonNull<u8>,
            _old_layout: Layout,
            _new_layout: Layout,
        ) -> Result<NonNull<[u8]>, AllocError> {
            self.grows.set(self.grows.get() + 1);
            Err(AllocError)
        }

        unsafe fn shrink(
            &self,
            ptr: NonNull<u8>,
            old_layout: Layout,
            new_layout: Layout,
        ) -> Result<NonNull<[u8]>, AllocError> {
            // SAFETY: the caller upholds `Allocator::shrink`'s contract; this
            // wrapper forwards the same pointer and layouts to `Global`.
            let ptr = unsafe { Global.shrink(ptr, old_layout, new_layout) }?.cast::<u8>();
            Ok(NonNull::slice_from_raw_parts(ptr, new_layout.size()))
        }
    }

    let allocator = FailingGrowAllocator {
        grows: Cell::new(0),
    };
    let mut vec = Vec::try_with_capacity_in(1, &allocator).unwrap();
    vec.push_within_capacity(10).unwrap();

    let value = vec.try_push(20).unwrap_err();

    assert_eq!(value, 20);
    assert_eq!(vec.as_slice(), &[10]);
    assert_eq!(allocator.grows.get(), 1);
}

#[test]
fn try_push_handles_zsts_without_allocating() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<(), _>::new_in(&allocator);

    vec.try_push(()).unwrap();
    vec.try_push(()).unwrap();

    assert_eq!(vec.len(), 2);
    assert_eq!(vec.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_from_slice_in_clones_elements() {
    let allocator = ExactCountingAllocator::new();
    let vec = Vec::try_from_slice_in(&[10, 20, 30], &allocator).unwrap();

    assert_eq!(vec.as_slice(), &[10, 20, 30]);
    assert_eq!(vec.capacity(), 3);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_from_slice_in_empty_slice_does_not_allocate() {
    let allocator = CountingAllocator::new();
    let vec = Vec::<i32, _>::try_from_slice_in(&[], &allocator).unwrap();

    assert_eq!(vec.as_slice(), &[]);
    assert_eq!(vec.capacity(), 0);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_from_slice_in_returns_error_when_allocation_fails() {
    struct FailingAllocateAllocator {
        allocations: Cell<usize>,
    }

    // SAFETY: all operations deliberately fail or are unreachable in this
    // test-only allocator, which is used only to verify allocation failure.
    unsafe impl Allocator for FailingAllocateAllocator {
        fn allocate(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            self.allocations.set(self.allocations.get() + 1);
            Err(AllocError)
        }

        unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
            panic!("unexpected deallocate");
        }

        unsafe fn grow(
            &self,
            _ptr: NonNull<u8>,
            _old_layout: Layout,
            _new_layout: Layout,
        ) -> Result<NonNull<[u8]>, AllocError> {
            Err(AllocError)
        }

        unsafe fn shrink(
            &self,
            _ptr: NonNull<u8>,
            _old_layout: Layout,
            _new_layout: Layout,
        ) -> Result<NonNull<[u8]>, AllocError> {
            Err(AllocError)
        }
    }

    let allocator = FailingAllocateAllocator {
        allocations: Cell::new(0),
    };

    let result = Vec::try_from_slice_in(&[10, 20, 30], &allocator);

    assert!(result.is_err());
    assert_eq!(allocator.allocations.get(), 1);
}

#[test]
fn try_from_slice_in_handles_zsts_without_allocating() {
    let allocator = CountingAllocator::new();
    let vec = Vec::try_from_slice_in(&[(), (), ()], &allocator).unwrap();

    assert_eq!(vec.len(), 3);
    assert_eq!(vec.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_from_slice_in_is_panic_safe_when_clone_panics() {
    #[derive(Debug)]
    struct PanicOnSecondClone<'a> {
        value: i32,
        clones: &'a Cell<usize>,
        drops: &'a Cell<usize>,
    }

    impl Clone for PanicOnSecondClone<'_> {
        fn clone(&self) -> Self {
            let clones = self.clones.get() + 1;
            self.clones.set(clones);

            if clones == 2 {
                panic!("clone panic");
            }

            Self {
                value: self.value,
                clones: self.clones,
                drops: self.drops,
            }
        }
    }

    impl Drop for PanicOnSecondClone<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    let clones = Cell::new(0);
    let drops = Cell::new(0);
    let source = [
        PanicOnSecondClone {
            value: 10,
            clones: &clones,
            drops: &drops,
        },
        PanicOnSecondClone {
            value: 20,
            clones: &clones,
            drops: &drops,
        },
        PanicOnSecondClone {
            value: 30,
            clones: &clones,
            drops: &drops,
        },
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = Vec::try_from_slice_in(&source, Global);
    }));

    assert!(result.is_err());
    assert_eq!(clones.get(), 2);
    assert_eq!(drops.get(), 1);

    drop(source);
    assert_eq!(drops.get(), 4);
}

#[test]
fn iter_returns_initialized_elements() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    let mut iter = vec.iter();

    assert_eq!(iter.next(), Some(&10));
    assert_eq!(iter.next(), Some(&20));
    assert_eq!(iter.next(), Some(&30));
    assert_eq!(iter.next(), None);
}

#[test]
fn iter_mut_returns_initialized_elements_mutably() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(1).unwrap();
    vec.push_within_capacity(2).unwrap();
    vec.push_within_capacity(3).unwrap();

    for value in vec.iter_mut() {
        *value *= 2;
    }

    assert_eq!(vec.as_slice(), &[2, 4, 6]);
}

#[test]
fn as_slice_returns_initialized_elements() {
    const _: core::mem::ManuallyDrop<Vec<u8, Global>> =
        core::mem::ManuallyDrop::new(Vec::new_in(Global));
    const _: core::mem::ManuallyDrop<Vec<u8, Global>> = core::mem::ManuallyDrop::new(Vec::new());
    // These constants prove that the accessors are const. Rust 1.87 cannot
    // call through `ManuallyDrop`'s deref in const context, so read the value
    // first and then forget the `Vec` to avoid const drop.
    #[allow(dead_code)]
    const EMPTY_LEN: usize = {
        let empty = Vec::<u8, Global>::new();
        let len = empty.len();
        core::mem::forget(empty);
        len
    };
    #[allow(dead_code)]
    const EMPTY_CAPACITY: usize = {
        let empty = Vec::<u8, Global>::new();
        let capacity = empty.capacity();
        core::mem::forget(empty);
        capacity
    };
    #[allow(dead_code)]
    const EMPTY_IS_EMPTY: bool = {
        let empty = Vec::<u8, Global>::new();
        let is_empty = empty.is_empty();
        core::mem::forget(empty);
        is_empty
    };
    #[allow(clippy::assertions_on_constants)]
    const _: () = assert!(EMPTY_LEN == 0);
    #[allow(clippy::assertions_on_constants)]
    const _: () = assert!(EMPTY_CAPACITY == 0);
    #[allow(clippy::assertions_on_constants)]
    const _: () = assert!(EMPTY_IS_EMPTY);

    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    assert_eq!(vec.as_slice(), &[10, 20]);

    let empty = Vec::<u8, _>::new_in(Global);
    assert_eq!(empty.as_slice(), &[]);
}

#[test]
fn get_returns_element_or_none() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    let first = 0;
    assert_eq!(vec.get(first), Some(&10));
    assert_eq!(vec.get(2), Some(&30));
    assert_eq!(vec.get(3), None);
    assert_eq!(vec.get(usize::MAX), None);

    let empty = Vec::<u8, _>::new_in(Global);
    assert_eq!(empty.get(first), None);
}

#[test]
fn get_returns_subslice_or_none() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();
    vec.push_within_capacity(40).unwrap();

    assert_eq!(vec.get(..), Some(&[10, 20, 30, 40][..]));
    assert_eq!(vec.get(1..3), Some(&[20, 30][..]));
    assert_eq!(vec.get(2..), Some(&[30, 40][..]));
    assert_eq!(vec.get(..=1), Some(&[10, 20][..]));
    assert_eq!(vec.get(4..5), None);
    let invalid_start = 3;
    let invalid_end = 2;
    assert_eq!(vec.get(invalid_start..invalid_end), None);
}

#[test]
fn get_mut_returns_element_or_none() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    *vec.get_mut(1).unwrap() = 25;

    assert_eq!(vec.as_slice(), &[10, 25, 30]);
    assert_eq!(vec.get_mut(3), None);
    assert_eq!(vec.get_mut(usize::MAX), None);

    let mut empty = Vec::<u8, _>::new_in(Global);
    assert_eq!(empty.get_mut(0), None);
}

#[test]
fn get_mut_returns_mutable_subslice_or_none() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();
    vec.push_within_capacity(40).unwrap();

    if let Some(values) = vec.get_mut(1..3) {
        values[0] = 21;
        values[1] = 31;
    }

    assert_eq!(vec.as_slice(), &[10, 21, 31, 40]);
    assert_eq!(vec.get_mut(4..5), None);
    let invalid_start = 3;
    let invalid_end = 2;
    assert_eq!(vec.get_mut(invalid_start..invalid_end), None);
}

#[test]
fn first_and_last_return_end_elements_or_none() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();

    assert_eq!(vec.first(), None);
    assert_eq!(vec.last(), None);
    assert_eq!(vec.first_mut(), None);
    assert_eq!(vec.last_mut(), None);

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    assert_eq!(vec.first(), Some(&10));
    assert_eq!(vec.last(), Some(&30));

    *vec.first_mut().unwrap() = 11;
    *vec.last_mut().unwrap() = 31;

    assert_eq!(vec.as_slice(), &[11, 20, 31]);
}

#[test]
fn split_first_and_last_return_parts_or_none() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();

    assert_eq!(vec.split_first(), None);
    assert_eq!(vec.split_last(), None);
    assert_eq!(vec.split_first_mut(), None);
    assert_eq!(vec.split_last_mut(), None);

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    assert_eq!(vec.split_first(), Some((&10, &[20, 30][..])));
    assert_eq!(vec.split_last(), Some((&30, &[10, 20][..])));

    if let Some((first, rest)) = vec.split_first_mut() {
        *first = 11;
        rest[0] = 21;
    }

    if let Some((last, rest)) = vec.split_last_mut() {
        *last = 31;
        rest[0] = 12;
    }

    assert_eq!(vec.as_slice(), &[12, 21, 31]);
}

#[test]
fn split_at_checked_returns_subslices_or_none() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();
    vec.push_within_capacity(40).unwrap();

    assert_eq!(
        vec.split_at_checked(0),
        Some((&[][..], &[10, 20, 30, 40][..]))
    );
    assert_eq!(
        vec.split_at_checked(2),
        Some((&[10, 20][..], &[30, 40][..]))
    );
    assert_eq!(
        vec.split_at_checked(4),
        Some((&[10, 20, 30, 40][..], &[][..]))
    );
    assert_eq!(vec.split_at_checked(5), None);

    if let Some((left, right)) = vec.split_at_mut_checked(2) {
        left[0] = 11;
        right[1] = 41;
    }

    assert_eq!(vec.as_slice(), &[11, 20, 30, 41]);
    assert!(vec.split_at_mut_checked(5).is_none());
}

#[test]
fn pop_returns_last_element_or_none() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    let capacity = vec.capacity();

    assert_eq!(vec.pop(), None);

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    assert_eq!(vec.pop(), Some(30));
    assert_eq!(vec.pop(), Some(20));
    assert_eq!(vec.pop(), Some(10));
    assert_eq!(vec.pop(), None);
    assert_eq!(vec.len(), 0);
    assert_eq!(vec.capacity(), capacity);
}

#[test]
fn pop_if_removes_last_element_when_predicate_matches() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    let capacity = vec.capacity();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    assert_eq!(vec.pop_if(|last| *last == 20), None);
    assert_eq!(vec.as_slice(), &[10, 20, 30]);

    assert_eq!(vec.pop_if(|last| *last == 30), Some(30));
    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(vec.capacity(), capacity);
}

#[test]
fn pop_if_allows_mutating_unpopped_last_element() {
    let mut vec = Vec::try_with_capacity_in(2, Global).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    assert_eq!(
        vec.pop_if(|last| {
            *last += 1;
            false
        }),
        None
    );
    assert_eq!(vec.as_slice(), &[10, 21]);
}

#[test]
fn pop_if_does_not_call_predicate_when_empty() {
    let called = Cell::new(false);
    let mut vec = Vec::<i32, Global>::new();

    assert_eq!(
        vec.pop_if(|_| {
            called.set(true);
            true
        }),
        None
    );
    assert!(!called.get());
}

#[test]
fn truncate_shortens_drops_tail_and_keeps_capacity() {
    #[derive(Debug)]
    struct DropCounter<'a> {
        value: i32,
        drops: &'a Cell<usize>,
    }

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
        let capacity = vec.capacity();

        for value in [10, 20, 30, 40] {
            vec.push_within_capacity(DropCounter {
                value,
                drops: &drops,
            })
            .unwrap();
        }

        vec.truncate(2);

        assert_eq!(vec.len(), 2);
        assert_eq!(vec.capacity(), capacity);
        assert_eq!(vec.get(0).map(|value| value.value), Some(10));
        assert_eq!(vec.get(1).map(|value| value.value), Some(20));
        assert_eq!(drops.get(), 2);
    }

    assert_eq!(drops.get(), 4);
}

#[test]
fn truncate_is_noop_when_len_is_at_least_current_len() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    let capacity = vec.capacity();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    vec.truncate(2);
    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(vec.capacity(), capacity);

    vec.truncate(3);
    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(vec.capacity(), capacity);
}

#[test]
fn truncate_handles_zsts_without_allocating() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropZst;

    impl Drop for DropZst {
        fn drop(&mut self) {
            ZST_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);

    ZST_DROPS.store(0, Ordering::Relaxed);

    {
        let allocator = CountingAllocator::new();
        let mut vec = Vec::<DropZst, _>::new_in(&allocator);

        for _ in 0..4 {
            vec.push_within_capacity(DropZst).unwrap();
        }

        vec.truncate(1);

        assert_eq!(vec.len(), 1);
        assert_eq!(vec.capacity(), usize::MAX);
        assert_eq!(allocator.allocations.get(), 0);
        assert_eq!(allocator.deallocations.get(), 0);
        assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 3);
    }

    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 4);
}

#[test]
fn retain_removes_rejected_elements_and_keeps_capacity() {
    #[derive(Debug)]
    struct DropCounter<'a> {
        value: i32,
        drops: &'a Cell<usize>,
    }

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(5, Global).unwrap();
        let capacity = vec.capacity();

        for value in 1..=5 {
            vec.push_within_capacity(DropCounter {
                value,
                drops: &drops,
            })
            .unwrap();
        }

        vec.retain(|item| item.value % 2 == 1);

        assert_eq!(vec.len(), 3);
        assert_eq!(vec.capacity(), capacity);
        assert_eq!(vec.get(0).map(|item| item.value), Some(1));
        assert_eq!(vec.get(1).map(|item| item.value), Some(3));
        assert_eq!(vec.get(2).map(|item| item.value), Some(5));
        assert_eq!(drops.get(), 2);
    }

    assert_eq!(drops.get(), 5);
}

#[test]
fn retain_visits_elements_once_in_original_order() {
    let keep = [false, true, true, false, true];
    let mut keep = keep.iter();
    let mut vec = Vec::try_with_capacity_in(5, Global).unwrap();

    for value in 1..=5 {
        vec.push_within_capacity(value).unwrap();
    }

    vec.retain(|_| *keep.next().unwrap());

    assert_eq!(vec.as_slice(), &[2, 3, 5]);
    assert!(keep.next().is_none());
}

#[test]
fn retain_mut_can_mutate_kept_elements() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();

    for value in [1, 2, 3, 4] {
        vec.push_within_capacity(value).unwrap();
    }

    vec.retain_mut(|item| {
        if *item <= 3 {
            *item += 10;
            true
        } else {
            false
        }
    });

    assert_eq!(vec.as_slice(), &[11, 12, 13]);
}

#[test]
fn retain_is_noop_when_all_elements_are_kept() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    let capacity = vec.capacity();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    vec.retain(|_| true);

    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(vec.capacity(), capacity);
}

#[test]
fn retain_mut_handles_zsts_without_allocating() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropZst;

    impl Drop for DropZst {
        fn drop(&mut self) {
            ZST_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);

    ZST_DROPS.store(0, Ordering::Relaxed);

    {
        let allocator = CountingAllocator::new();
        let seen = Cell::new(0);
        let mut vec = Vec::<DropZst, _>::new_in(&allocator);

        for _ in 0..4 {
            vec.push_within_capacity(DropZst).unwrap();
        }

        vec.retain_mut(|_| {
            let next = seen.get() + 1;
            seen.set(next);
            next % 2 == 0
        });

        assert_eq!(seen.get(), 4);
        assert_eq!(vec.len(), 2);
        assert_eq!(vec.capacity(), usize::MAX);
        assert_eq!(allocator.allocations.get(), 0);
        assert_eq!(allocator.deallocations.get(), 0);
        assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 2);
    }

    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 4);
}

#[test]
fn clear_drops_elements_and_keeps_capacity() {
    #[derive(Debug)]
    struct DropCounter<'a>(&'a Cell<usize>);

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
        let capacity = vec.capacity();

        vec.push_within_capacity(DropCounter(&drops)).unwrap();
        vec.push_within_capacity(DropCounter(&drops)).unwrap();
        vec.push_within_capacity(DropCounter(&drops)).unwrap();

        vec.clear();

        assert_eq!(vec.len(), 0);
        assert_eq!(vec.capacity(), capacity);
        assert_eq!(drops.get(), 3);
    }

    assert_eq!(drops.get(), 3);
}

#[test]
fn recycle_clears_elements_and_reuses_allocation() {
    #[derive(Debug)]
    #[repr(transparent)]
    struct DropCounter<'a>(&'a Cell<usize>);

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let allocator = CountingAllocator::new();
    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();
        let capacity = vec.capacity();
        let ptr = vec.as_slice().as_ptr();

        vec.push_within_capacity(DropCounter(&drops)).unwrap();
        vec.push_within_capacity(DropCounter(&drops)).unwrap();

        let recycled: Vec<core::mem::ManuallyDrop<DropCounter<'_>>, _> = vec.recycle();

        assert_eq!(recycled.len(), 0);
        assert_eq!(recycled.capacity(), capacity);
        assert_eq!(recycled.as_slice().as_ptr().cast::<DropCounter<'_>>(), ptr);
        assert_eq!(drops.get(), 2);
        assert_eq!(allocator.deallocations.get(), 0);
    }

    assert_eq!(drops.get(), 2);
    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn recycle_into_smaller_element_type_scales_capacity() {
    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::<[u32; 2], _>::try_with_capacity_in(3, &allocator).unwrap();
        let capacity = vec.capacity();
        let ptr = vec.as_slice().as_ptr();

        vec.push_within_capacity([10, 20]).unwrap();
        vec.push_within_capacity([30, 40]).unwrap();

        let recycled: Vec<u32, _> = vec.recycle();

        assert_eq!(recycled.len(), 0);
        assert_eq!(recycled.capacity(), capacity * 2);
        assert_eq!(recycled.as_slice().as_ptr().cast::<[u32; 2]>(), ptr);
        assert_eq!(allocator.deallocations.get(), 0);
    }

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn recycle_empty_vec_keeps_zero_capacity() {
    let allocator = CountingAllocator::new();
    let vec = Vec::<u32, _>::new_in(&allocator);

    let recycled: Vec<i32, _> = vec.recycle();

    assert_eq!(recycled.len(), 0);
    assert_eq!(recycled.capacity(), 0);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.deallocations.get(), 0);
}

#[test]
fn recycle_handles_zsts_without_allocating() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropZst;

    impl Drop for DropZst {
        fn drop(&mut self) {
            ZST_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);

    ZST_DROPS.store(0, Ordering::Relaxed);

    {
        let allocator = CountingAllocator::new();
        let mut vec = Vec::<DropZst, _>::new_in(&allocator);

        for _ in 0..3 {
            vec.push_within_capacity(DropZst).unwrap();
        }

        let recycled: Vec<(), _> = vec.recycle();

        assert_eq!(recycled.len(), 0);
        assert_eq!(recycled.capacity(), usize::MAX);
        assert_eq!(allocator.allocations.get(), 0);
        assert_eq!(allocator.deallocations.get(), 0);
        assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 3);
    }

    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 3);
}

#[test]
fn into_boxed_slice_unchecked_transfers_full_allocation() {
    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::try_with_capacity_in(3, &allocator).unwrap();
        assert_eq!(vec.capacity(), 3);

        vec.push_within_capacity(10).unwrap();
        vec.push_within_capacity(20).unwrap();
        vec.push_within_capacity(30).unwrap();

        // SAFETY: the vector is full, so len equals capacity.
        let boxed = unsafe { vec.into_boxed_slice_unchecked() };

        assert_eq!(&*boxed, &[10, 20, 30]);
        assert_eq!(allocator.deallocations.get(), 0);
    }

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn into_boxed_slice_unchecked_drops_elements_once() {
    #[derive(Debug)]
    struct DropCounter<'a>(&'a Cell<usize>);

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(2, Global).unwrap();

        vec.push_within_capacity(DropCounter(&drops)).unwrap();
        vec.push_within_capacity(DropCounter(&drops)).unwrap();

        // SAFETY: the vector is full, so len equals capacity.
        let boxed = unsafe { vec.into_boxed_slice_unchecked() };
        assert_eq!(drops.get(), 0);

        drop(boxed);
        assert_eq!(drops.get(), 2);
    }

    assert_eq!(drops.get(), 2);
}

#[test]
fn try_into_boxed_slice_shrinks_and_transfers_elements() {
    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();
        vec.push_within_capacity(10).unwrap();
        vec.push_within_capacity(20).unwrap();

        let boxed = vec.try_into_boxed_slice().unwrap();

        assert_eq!(&*boxed, &[10, 20]);
        assert_eq!(allocator.shrinks.get(), 1);
        assert_eq!(allocator.deallocations.get(), 0);

        drop(boxed);
    }

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn try_into_boxed_slice_accepts_allocator_excess_after_shrink() {
    let allocator = BucketAllocator::new();

    {
        let mut vec = Vec::<u8, _>::try_with_capacity_in(BUCKET_SIZE * 2, &allocator).unwrap();
        vec.push_within_capacity(42).unwrap();

        let boxed = vec.try_into_boxed_slice().unwrap();

        assert_eq!(&*boxed, &[42]);
        assert_eq!(allocator.shrinks.get(), 1);
        assert_eq!(allocator.deallocations.get(), 0);

        drop(boxed);
    }

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn try_into_boxed_slice_empty_vec_deallocates_before_conversion() {
    let allocator = CountingAllocator::new();

    {
        let vec = Vec::<u16, _>::try_with_capacity_in(4, &allocator).unwrap();
        let boxed = vec.try_into_boxed_slice().unwrap();

        assert_eq!(&*boxed, &[]);
        assert_eq!(allocator.deallocations.get(), 1);

        drop(boxed);
    }

    // allocator-api2 `Box` calls `deallocate` even for the zero-sized boxed
    // slice layout. The first call is the vector's original allocation; the
    // second is the boxed empty slice.
    assert_eq!(allocator.deallocations.get(), 2);
}

#[test]
fn try_into_boxed_slice_handles_zsts_without_allocating() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropZst;

    impl Drop for DropZst {
        fn drop(&mut self) {
            ZST_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);

    ZST_DROPS.store(0, Ordering::Relaxed);

    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::<DropZst, _>::new_in(&allocator);

        for _ in 0..3 {
            vec.push_within_capacity(DropZst).unwrap();
        }

        let boxed = vec.try_into_boxed_slice().unwrap();

        assert_eq!(boxed.len(), 3);
        assert_eq!(allocator.allocations.get(), 0);
        assert_eq!(allocator.deallocations.get(), 0);

        drop(boxed);
    }

    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 3);
    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn try_into_boxed_slice_drops_vec_when_shrink_fails() {
    #[derive(Debug)]
    struct DropCounter<'a>(&'a Cell<usize>);

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    struct FailingShrinkAllocator {
        shrinks: Cell<usize>,
        deallocations: Cell<usize>,
    }

    // SAFETY: this wrapper forwards successful allocation operations to
    // `Global`; `shrink` deliberately reports allocation failure without
    // touching the allocation.
    unsafe impl Allocator for FailingShrinkAllocator {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            Global.allocate(layout)
        }

        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            self.deallocations.set(self.deallocations.get() + 1);
            // SAFETY: the caller upholds `Allocator::deallocate`'s contract;
            // this wrapper forwards the same pointer and layout to `Global`.
            unsafe { Global.deallocate(ptr, layout) }
        }

        unsafe fn grow(
            &self,
            ptr: NonNull<u8>,
            old_layout: Layout,
            new_layout: Layout,
        ) -> Result<NonNull<[u8]>, AllocError> {
            // SAFETY: the caller upholds `Allocator::grow`'s contract; this
            // wrapper forwards the same pointer and layouts to `Global`.
            unsafe { Global.grow(ptr, old_layout, new_layout) }
        }

        unsafe fn shrink(
            &self,
            _ptr: NonNull<u8>,
            _old_layout: Layout,
            _new_layout: Layout,
        ) -> Result<NonNull<[u8]>, AllocError> {
            self.shrinks.set(self.shrinks.get() + 1);
            Err(AllocError)
        }
    }

    let allocator = FailingShrinkAllocator {
        shrinks: Cell::new(0),
        deallocations: Cell::new(0),
    };
    let drops = Cell::new(0);

    let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();
    vec.push_within_capacity(DropCounter(&drops)).unwrap();
    vec.push_within_capacity(DropCounter(&drops)).unwrap();

    let result = vec.try_into_boxed_slice();

    assert!(result.is_err());
    assert_eq!(allocator.shrinks.get(), 1);
    assert_eq!(allocator.deallocations.get(), 1);
    assert_eq!(drops.get(), 2);
}

#[test]
fn dedup_removes_consecutive_duplicates() {
    let mut vec = Vec::try_with_capacity_in(8, Global).unwrap();

    for value in [1, 1, 2, 2, 2, 3, 1, 1] {
        vec.push_within_capacity(value).unwrap();
    }

    vec.dedup();

    assert_eq!(vec.as_slice(), &[1, 2, 3, 1]);
}

#[test]
fn dedup_by_uses_custom_equality() {
    let mut vec = Vec::try_with_capacity_in(6, Global).unwrap();

    for value in [1_i32, -1, 2, -2, -3, 3] {
        vec.push_within_capacity(value).unwrap();
    }

    vec.dedup_by(|a, b| a.abs() == b.abs());

    assert_eq!(vec.as_slice(), &[1, 2, -3]);
}

#[test]
fn dedup_by_key_removes_consecutive_equal_keys() {
    let mut vec = Vec::try_with_capacity_in(6, Global).unwrap();

    for value in [10, 20, 21, 30, 20, 22] {
        vec.push_within_capacity(value).unwrap();
    }

    vec.dedup_by_key(|value| *value / 10);

    assert_eq!(vec.as_slice(), &[10, 20, 30, 20]);
}

#[test]
fn dedup_by_key_can_mutate_elements() {
    let mut vec = Vec::try_with_capacity_in(2, Global).unwrap();

    for value in [10, 20] {
        vec.push_within_capacity(value).unwrap();
    }

    vec.dedup_by_key(|value| {
        *value += 1;
        *value / 10
    });

    assert_eq!(vec.as_slice(), &[11, 21]);
}

#[test]
fn dedup_by_passes_current_element_first() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    let mut calls = [(0, 0); 2];
    let mut count = 0;

    vec.dedup_by(|current, previous| {
        calls[count] = (*current, *previous);
        count += 1;
        false
    });

    assert_eq!(vec.as_slice(), &[10, 20, 30]);
    assert_eq!(calls, [(20, 10), (30, 20)]);
}

#[test]
fn dedup_by_drops_removed_elements_once() {
    #[derive(Debug)]
    struct DropCounter<'a> {
        value: i32,
        drops: &'a Cell<usize>,
    }

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(8, Global).unwrap();

        for value in [1, 1, 2, 2, 2, 3, 1, 1] {
            vec.push_within_capacity(DropCounter {
                value,
                drops: &drops,
            })
            .unwrap();
        }

        vec.dedup_by(|a, b| a.value == b.value);

        assert_eq!(vec.len(), 4);
        assert_eq!(vec.get(0).map(|value| value.value), Some(1));
        assert_eq!(vec.get(1).map(|value| value.value), Some(2));
        assert_eq!(vec.get(2).map(|value| value.value), Some(3));
        assert_eq!(vec.get(3).map(|value| value.value), Some(1));
        assert_eq!(drops.get(), 4);
    }

    assert_eq!(drops.get(), 8);
}

#[test]
fn allocator_returns_underlying_allocator() {
    let allocator = CountingAllocator::new();
    let vec = Vec::<u16, _>::new_in(&allocator);
    let returned = vec.allocator();

    assert!(core::ptr::eq(*returned, &allocator));
}

#[test]
fn as_ref_as_mut_borrow_and_borrow_mut_return_slices() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    let vec_ref = <Vec<i32, Global> as AsRef<Vec<i32, Global>>>::as_ref(&vec);
    assert!(core::ptr::eq(vec_ref, &vec));

    let slice = <Vec<i32, Global> as AsRef<[i32]>>::as_ref(&vec);
    assert_eq!(slice, &[10, 20]);

    let borrowed = <Vec<i32, Global> as core::borrow::Borrow<[i32]>>::borrow(&vec);
    assert_eq!(borrowed, &[10, 20]);

    let vec_mut = <Vec<i32, Global> as AsMut<Vec<i32, Global>>>::as_mut(&mut vec);
    assert!(vec_mut.push_within_capacity(30).is_ok());

    let mut_slice = <Vec<i32, Global> as AsMut<[i32]>>::as_mut(&mut vec);
    mut_slice[1] = 21;

    let borrowed_mut = <Vec<i32, Global> as core::borrow::BorrowMut<[i32]>>::borrow_mut(&mut vec);
    borrowed_mut[2] = 31;

    assert_eq!(vec.as_slice(), &[10, 21, 31]);
}

#[test]
fn debug_formats_like_slice() {
    struct DebugBuffer {
        bytes: [u8; 32],
        len: usize,
    }

    impl DebugBuffer {
        const fn new() -> Self {
            Self {
                bytes: [0; 32],
                len: 0,
            }
        }

        fn as_bytes(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }

    impl core::fmt::Write for DebugBuffer {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let end = self.len + s.len();
            self.bytes[self.len..end].copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    let mut vec = Vec::try_with_capacity_in(2, Global).unwrap();
    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    let mut output = DebugBuffer::new();
    core::fmt::write(&mut output, format_args!("{:?}", vec)).unwrap();

    assert_eq!(output.as_bytes(), b"[10, 20]");
}

#[test]
fn hash_matches_slice_hash() {
    struct TestHasher(u64);

    impl core::hash::Hasher for TestHasher {
        fn finish(&self) -> u64 {
            self.0
        }

        fn write(&mut self, bytes: &[u8]) {
            for byte in bytes {
                self.0 = self.0.wrapping_mul(16_777_619) ^ u64::from(*byte);
            }
        }
    }

    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();
    vec.push_within_capacity(10_u16).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    let mut vec_hasher = TestHasher(0);
    core::hash::Hash::hash(&vec, &mut vec_hasher);

    let mut slice_hasher = TestHasher(0);
    core::hash::Hash::hash(vec.as_slice(), &mut slice_hasher);

    assert_eq!(vec_hasher.finish(), slice_hasher.finish());
}

#[test]
fn equality_and_ordering_match_slices_across_allocators() {
    let allocator = CountingAllocator::new();
    let mut left = Vec::try_with_capacity_in(2, Global).unwrap();
    let mut equal = Vec::try_with_capacity_in(2, &allocator).unwrap();
    let mut greater = Vec::try_with_capacity_in(2, Global).unwrap();

    for value in [10, 20] {
        left.push_within_capacity(value).unwrap();
        equal.push_within_capacity(value).unwrap();
    }
    greater.push_within_capacity(10).unwrap();
    greater.push_within_capacity(30).unwrap();

    assert_eq!(left, equal);
    assert_ne!(left, greater);
    assert!(left < greater);
    assert_eq!(left.partial_cmp(&greater), Some(core::cmp::Ordering::Less));
}

#[test]
fn borrowed_into_iter_delegates_to_slice_iterators() {
    let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();

    for value in [10, 20, 30] {
        vec.push_within_capacity(value).unwrap();
    }

    let mut sum = 0;
    for value in &vec {
        sum += *value;
    }
    assert_eq!(sum, 60);

    for value in &mut vec {
        *value += 1;
    }
    assert_eq!(vec.as_slice(), &[11, 21, 31]);
}

#[test]
fn owned_into_iter_yields_elements_and_deallocates_on_drop() {
    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::try_with_capacity_in(3, &allocator).unwrap();
        vec.push_within_capacity(10).unwrap();
        vec.push_within_capacity(20).unwrap();
        vec.push_within_capacity(30).unwrap();

        let mut iter = vec.into_iter();

        assert_eq!(iter.len(), 3);
        assert_eq!(iter.size_hint(), (3, Some(3)));
        assert_eq!(iter.as_slice(), &[10, 20, 30]);
        assert_eq!(allocator.deallocations.get(), 0);

        assert_eq!(iter.next(), Some(10));
        assert_eq!(iter.next(), Some(20));
        assert_eq!(iter.next(), Some(30));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn owned_into_iter_supports_next_back_and_remaining_slices() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();

    for value in [10, 20, 30, 40] {
        vec.push_within_capacity(value).unwrap();
    }

    let mut iter = vec.into_iter();

    assert_eq!(iter.next(), Some(10));
    assert_eq!(iter.next_back(), Some(40));
    assert_eq!(iter.as_slice(), &[20, 30]);

    iter.as_mut_slice()[0] = 21;

    assert_eq!(iter.next(), Some(21));
    assert_eq!(iter.next_back(), Some(30));
    assert_eq!(iter.next(), None);
}

#[test]
fn owned_into_iter_drops_unconsumed_elements_once() {
    #[derive(Debug)]
    struct DropCounter<'a> {
        value: i32,
        drops: &'a Cell<usize>,
    }

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    let drops = Cell::new(0);
    let first;

    {
        let mut vec = Vec::try_with_capacity_in(3, Global).unwrap();

        for value in [10, 20, 30] {
            vec.push_within_capacity(DropCounter {
                value,
                drops: &drops,
            })
            .unwrap();
        }

        let mut iter = vec.into_iter();
        first = iter.next().unwrap();
        assert_eq!(first.value, 10);
        assert_eq!(drops.get(), 0);

        drop(iter);
        assert_eq!(drops.get(), 2);
    }

    drop(first);
    assert_eq!(drops.get(), 3);
}

#[test]
fn owned_into_iter_deallocates_when_dropping_remaining_element_panics() {
    #[derive(Debug)]
    struct MaybePanicOnDrop<'a> {
        value: i32,
        drops: &'a Cell<usize>,
    }

    impl Drop for MaybePanicOnDrop<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);

            if self.value == 20 {
                panic!("drop panic");
            }
        }
    }

    let allocator = CountingAllocator::new();
    let drops = Cell::new(0);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut vec = Vec::try_with_capacity_in(3, &allocator).unwrap();

        for value in [10, 20, 30] {
            vec.push_within_capacity(MaybePanicOnDrop {
                value,
                drops: &drops,
            })
            .unwrap();
        }

        let mut iter = vec.into_iter();
        let _first = iter.next().unwrap();

        drop(iter);
    }));

    assert!(result.is_err());
    assert_eq!(allocator.deallocations.get(), 1);
    assert_eq!(drops.get(), 3);
}

#[test]
fn owned_into_iter_handles_zsts_without_allocating() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropZst;

    impl Drop for DropZst {
        fn drop(&mut self) {
            ZST_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);

    ZST_DROPS.store(0, Ordering::Relaxed);

    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::<DropZst, _>::new_in(&allocator);

        for _ in 0..3 {
            vec.push_within_capacity(DropZst).unwrap();
        }

        let mut iter = vec.into_iter();
        assert_eq!(iter.len(), 3);
        assert_eq!(iter.as_slice().len(), 3);

        drop(iter.next());
        assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 1);

        drop(iter);
        assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 3);
    }

    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.deallocations.get(), 0);
}

#[test]
fn try_reserve_does_not_grow_when_spare_capacity_is_exact() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();
    vec.push_within_capacity(1).unwrap();
    vec.push_within_capacity(2).unwrap();

    let capacity = vec.capacity();
    let spare = capacity - vec.len();
    vec.try_reserve(spare).unwrap();

    assert_eq!(vec.capacity(), capacity);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_reserve_from_zero_capacity_uses_initial_growth_capacity() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u8, _>::try_with_capacity_in(0, &allocator).unwrap();

    assert_eq!(vec.capacity(), 0);
    assert_eq!(allocator.allocations.get(), 0);

    vec.try_reserve(1).unwrap();

    assert!(vec.capacity() >= 8);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_reserve_uses_growth_factor() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
    let old_capacity = vec.capacity();

    for value in 0..old_capacity {
        vec.push_within_capacity(value).unwrap();
    }

    vec.try_reserve(1).unwrap();

    assert!(vec.capacity() >= old_capacity * 2);
}

#[test]
fn try_shrink_to_fit_without_capacity_is_noop() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u16, _>::new_in(&allocator);

    vec.try_shrink_to_fit().unwrap();

    assert_eq!(vec.capacity(), 0);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.shrinks.get(), 0);
    assert_eq!(allocator.deallocations.get(), 0);
}

#[test]
fn try_shrink_to_fit_empty_vec_deallocates() {
    let allocator = CountingAllocator::new();

    {
        let mut vec = Vec::<u16, _>::try_with_capacity_in(8, &allocator).unwrap();
        assert!(vec.capacity() >= 8);
        assert_eq!(allocator.allocations.get(), 1);

        vec.try_shrink_to_fit().unwrap();

        assert_eq!(vec.capacity(), 0);
        assert_eq!(allocator.shrinks.get(), 0);
        assert_eq!(allocator.deallocations.get(), 1);
    }

    assert_eq!(allocator.deallocations.get(), 1);
}

#[test]
fn try_shrink_to_fit_preserves_elements() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(5, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    vec.try_shrink_to_fit().unwrap();

    assert_eq!(vec.as_slice(), &[10, 20, 30]);
    assert_eq!(vec.capacity(), 3);
    assert_eq!(allocator.shrinks.get(), 1);
}

#[test]
fn try_shrink_to_honors_min_capacity() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(8, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.push_within_capacity(30).unwrap();

    vec.try_shrink_to(5).unwrap();

    assert_eq!(vec.as_slice(), &[10, 20, 30]);
    assert!(vec.capacity() >= 5);
    assert_eq!(allocator.shrinks.get(), 1);
    assert_eq!(
        allocator.last_shrink_new_size.get(),
        5 * core::mem::size_of::<u16>()
    );

    let capacity = vec.capacity();
    vec.try_shrink_to(capacity + 1).unwrap();

    assert_eq!(vec.capacity(), capacity);
    assert_eq!(allocator.shrinks.get(), 1);
}

#[test]
fn try_shrink_to_fit_uses_allocator_excess() {
    let allocator = BucketAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(13, &allocator).unwrap();
    let capacity = vec.capacity();

    for value in 0..13 {
        vec.push_within_capacity(value).unwrap();
    }

    vec.try_shrink_to_fit().unwrap();

    assert_eq!(vec.as_slice(), &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    assert_eq!(vec.capacity(), capacity);
    assert_eq!(allocator.shrinks.get(), 1);
}

#[test]
fn try_shrink_to_keeps_allocator_excess_when_target_stays_in_same_bucket() {
    let allocator = BucketAllocator::new();
    let mut vec = Vec::<u8, _>::try_with_capacity_in(BUCKET_SIZE + 1, &allocator).unwrap();

    assert_eq!(vec.capacity(), BUCKET_SIZE * 2);

    vec.try_shrink_to(BUCKET_SIZE + BUCKET_SIZE / 2).unwrap();

    assert_eq!(vec.capacity(), BUCKET_SIZE * 2);
    assert_eq!(allocator.shrinks.get(), 1);
    assert_eq!(allocator.deallocations.get(), 0);
}

#[test]
fn try_shrink_to_keeps_allocator_excess_when_half_bucket_stays_one_bucket() {
    let allocator = BucketAllocator::new();
    let mut vec = Vec::<u8, _>::try_with_capacity_in(1, &allocator).unwrap();

    assert_eq!(vec.capacity(), BUCKET_SIZE);

    vec.try_shrink_to(BUCKET_SIZE / 2).unwrap();

    assert_eq!(vec.capacity(), BUCKET_SIZE);
    assert_eq!(allocator.shrinks.get(), 1);
    assert_eq!(allocator.deallocations.get(), 0);
}

#[test]
fn try_shrink_to_fit_zst_is_noop() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<(), _>::new_in(&allocator);

    vec.push_within_capacity(()).unwrap();
    vec.push_within_capacity(()).unwrap();
    vec.try_shrink_to_fit().unwrap();

    assert_eq!(vec.len(), 2);
    assert_eq!(vec.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.shrinks.get(), 0);
    assert_eq!(allocator.deallocations.get(), 0);
}

#[test]
fn zero_sized_vec_does_not_allocate() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<(), _>::try_with_capacity_in(4, &allocator).unwrap();

    assert_eq!(vec.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);

    vec.push_within_capacity(()).unwrap();
    vec.push_within_capacity(()).unwrap();
    vec.try_reserve(usize::MAX - vec.len()).unwrap();

    assert_eq!(vec.len(), 2);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn new_in_does_not_allocate() {
    let allocator = CountingAllocator::new();
    let vec = Vec::<u16, _>::new_in(&allocator);

    assert_eq!(vec.capacity(), 0);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);

    let zst = Vec::<(), _>::new_in(&allocator);
    assert_eq!(zst.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_reserve_exact_does_not_grow_when_spare_capacity_is_exact() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();
    vec.push_within_capacity(1).unwrap();
    vec.push_within_capacity(2).unwrap();

    let capacity = vec.capacity();
    let spare = capacity - vec.len();
    vec.try_reserve_exact(spare).unwrap();

    assert_eq!(vec.capacity(), capacity);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_reserve_exact_from_zero_requests_exact_capacity() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u16, _>::new_in(&allocator);

    vec.try_reserve_exact(13).unwrap();

    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(
        allocator.last_allocate_size.get(),
        13 * core::mem::size_of::<u16>()
    );
    assert_eq!(allocator.grows.get(), 0);
    assert!(vec.capacity() >= 13);
}

#[test]
fn try_reserve_exact_grows_to_required_capacity() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(4, &allocator).unwrap();

    for value in 0..4 {
        vec.push_within_capacity(value).unwrap();
    }

    vec.try_reserve_exact(1).unwrap();

    assert_eq!(allocator.grows.get(), 1);
    assert_eq!(
        allocator.last_grow_new_size.get(),
        5 * core::mem::size_of::<u16>()
    );
    assert!(vec.capacity() >= 5);
}

#[test]
fn try_with_capacity_in_uses_allocator_excess() {
    let allocator = BucketAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(13, &allocator).unwrap();

    assert_eq!(vec.capacity(), BUCKET_SIZE / core::mem::size_of::<u16>());
    assert_eq!(allocator.allocations.get(), 1);

    for value in 0..13 {
        vec.push_within_capacity(value).unwrap();
    }
    vec.try_reserve(1).unwrap();

    assert_eq!(vec.len(), 13);
    assert_eq!(vec.capacity(), BUCKET_SIZE / core::mem::size_of::<u16>());
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_grow_uses_allocator_excess() {
    let allocator = BucketAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(0, &allocator).unwrap();

    vec.try_reserve(1).unwrap();

    let page_capacity = BUCKET_SIZE / core::mem::size_of::<u16>();
    assert_eq!(vec.capacity(), page_capacity);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);

    vec.try_reserve(page_capacity + 1).unwrap();

    assert_eq!(vec.capacity(), page_capacity * 2);
    assert_eq!(allocator.grows.get(), 1);
}

#[test]
fn try_reserve_exact_uses_allocator_excess() {
    let allocator = BucketAllocator::new();
    let mut vec = Vec::<u16, _>::new_in(&allocator);

    vec.try_reserve_exact(13).unwrap();

    assert_eq!(vec.capacity(), BUCKET_SIZE / core::mem::size_of::<u16>());
    assert_eq!(allocator.allocations.get(), 1);

    for value in 0..13 {
        vec.push_within_capacity(value).unwrap();
    }
    vec.try_reserve_exact(1).unwrap();

    assert_eq!(vec.len(), 13);
    assert_eq!(vec.capacity(), BUCKET_SIZE / core::mem::size_of::<u16>());
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_from_slice_within_capacity_appends_all_when_it_fits() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();

    let rest = vec.extend_from_slice_within_capacity(&[20, 30, 40]);

    assert_eq!(rest, &[]);
    assert_eq!(vec.as_slice(), &[10, 20, 30, 40]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_from_slice_within_capacity_returns_uninserted_suffix() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(3, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();

    let source = [20, 30, 40, 50];
    let rest = vec.extend_from_slice_within_capacity(&source);

    assert_eq!(rest, &[40, 50]);
    assert_eq!(vec.as_slice(), &[10, 20, 30]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_from_slice_within_capacity_returns_source_when_full() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    let source = [30, 40];
    let rest = vec.extend_from_slice_within_capacity(&source);

    assert!(core::ptr::eq(rest.as_ptr(), source.as_ptr()));
    assert_eq!(rest.len(), source.len());
    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_from_slice_within_capacity_empty_source_is_noop() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();

    let rest = vec.extend_from_slice_within_capacity(&[]);

    assert_eq!(rest, &[]);
    assert_eq!(vec.as_slice(), &[10]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_from_slice_within_capacity_handles_zsts_without_allocating() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::<(), _>::new_in(&allocator);

    let rest = vec.extend_from_slice_within_capacity(&[(), (), ()]);

    assert_eq!(rest, &[]);
    assert_eq!(vec.len(), 3);
    assert_eq!(vec.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_within_capacity_appends_all_when_it_fits() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();

    let mut rest = vec.extend_within_capacity([20, 30, 40]);

    assert_eq!(rest.next(), None);
    assert_eq!(vec.as_slice(), &[10, 20, 30, 40]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_within_capacity_returns_iterator_with_uninserted_values() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(3, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();

    let mut rest = vec.extend_within_capacity([20, 30, 40, 50]);

    assert_eq!(rest.next(), Some(40));
    assert_eq!(rest.next(), Some(50));
    assert_eq!(rest.next(), None);
    assert_eq!(vec.as_slice(), &[10, 20, 30]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_within_capacity_returns_original_iterator_when_full() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    let mut rest = vec.extend_within_capacity([30, 40]);

    assert_eq!(rest.next(), Some(30));
    assert_eq!(rest.next(), Some(40));
    assert_eq!(rest.next(), None);
    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_within_capacity_empty_iterator_is_noop() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();

    let mut rest = vec.extend_within_capacity([]);

    assert_eq!(rest.next(), None);
    assert_eq!(vec.as_slice(), &[10]);
    assert_eq!(allocator.allocations.get(), 1);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_within_capacity_does_not_pull_after_capacity_is_full() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();
    let next_calls = Cell::new(0);

    let iter = CountingIterator {
        next_calls: &next_calls,
        next: 10,
        end: 13,
    };
    let mut rest = vec.extend_within_capacity(iter);

    assert_eq!(next_calls.get(), 2);
    assert_eq!(vec.as_slice(), &[10, 11]);
    assert_eq!(rest.next(), Some(12));
    assert_eq!(rest.next(), None);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn extend_within_capacity_handles_zsts_without_allocating() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::<(), _>::new_in(&allocator);

    let mut rest = vec.extend_within_capacity([(), (), ()]);

    assert_eq!(rest.next(), None);
    assert_eq!(vec.len(), 3);
    assert_eq!(vec.capacity(), usize::MAX);
    assert_eq!(allocator.allocations.get(), 0);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_resize_extends_with_clones_and_grows_if_needed() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.try_resize(4, 7).unwrap();

    assert_eq!(vec.as_slice(), &[10, 7, 7, 7]);
    assert_eq!(allocator.grows.get(), 1);
}

#[test]
fn try_resize_uses_existing_capacity_when_it_fits() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(4, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.try_resize(3, 7).unwrap();

    assert_eq!(vec.as_slice(), &[10, 7, 7]);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_resize_truncates_when_new_len_is_smaller() {
    let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
    let capacity = vec.capacity();

    for value in [10, 20, 30, 40] {
        vec.push_within_capacity(value).unwrap();
    }

    vec.try_resize(2, 99).unwrap();

    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(vec.capacity(), capacity);
}

#[test]
fn try_resize_is_noop_when_len_is_unchanged() {
    let allocator = ExactCountingAllocator::new();
    let mut vec = Vec::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();
    vec.try_resize(2, 99).unwrap();

    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_resize_leaves_vec_unchanged_on_capacity_overflow() {
    let allocator = CountingAllocator::new();
    let mut vec = Vec::<u16, _>::try_with_capacity_in(2, &allocator).unwrap();

    vec.push_within_capacity(10).unwrap();
    vec.push_within_capacity(20).unwrap();

    assert!(vec.try_resize(usize::MAX, 99).is_err());
    assert_eq!(vec.as_slice(), &[10, 20]);
    assert_eq!(allocator.grows.get(), 0);
}

#[test]
fn try_resize_handles_zsts_without_allocating() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropZst;

    impl Clone for DropZst {
        fn clone(&self) -> Self {
            ZST_CLONES.fetch_add(1, Ordering::Relaxed);
            DropZst
        }
    }

    impl Drop for DropZst {
        fn drop(&mut self) {
            ZST_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    static ZST_CLONES: AtomicUsize = AtomicUsize::new(0);
    static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);

    ZST_CLONES.store(0, Ordering::Relaxed);
    ZST_DROPS.store(0, Ordering::Relaxed);

    {
        let allocator = ExactCountingAllocator::new();
        let mut vec = Vec::<DropZst, _>::new_in(&allocator);

        vec.try_resize(3, DropZst).unwrap();

        assert_eq!(vec.len(), 3);
        assert_eq!(vec.capacity(), usize::MAX);
        assert_eq!(allocator.allocations.get(), 0);
        assert_eq!(allocator.grows.get(), 0);
        assert_eq!(ZST_CLONES.load(Ordering::Relaxed), 2);
        assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 0);
    }

    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 3);
}

#[test]
fn try_resize_is_panic_safe_when_clone_panics() {
    #[derive(Debug)]
    struct PanicOnSecondClone<'a> {
        value: i32,
        clones: &'a Cell<usize>,
        drops: &'a Cell<usize>,
    }

    impl Clone for PanicOnSecondClone<'_> {
        fn clone(&self) -> Self {
            let clones = self.clones.get() + 1;
            self.clones.set(clones);

            if clones == 2 {
                panic!("clone panic");
            }

            Self {
                value: self.value,
                clones: self.clones,
                drops: self.drops,
            }
        }
    }

    impl Drop for PanicOnSecondClone<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    let clones = Cell::new(0);
    let drops = Cell::new(0);

    {
        let mut vec = Vec::try_with_capacity_in(4, Global).unwrap();
        vec.push_within_capacity(PanicOnSecondClone {
            value: 1,
            clones: &clones,
            drops: &drops,
        })
        .unwrap();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = vec.try_resize(
                4,
                PanicOnSecondClone {
                    value: 9,
                    clones: &clones,
                    drops: &drops,
                },
            );
        }));

        assert!(result.is_err());
        assert_eq!(clones.get(), 2);
        assert_eq!(drops.get(), 1);
        assert_eq!(vec.len(), 2);
        assert_eq!(vec.get(0).map(|item| item.value), Some(1));
        assert_eq!(vec.get(1).map(|item| item.value), Some(9));
    }

    assert_eq!(drops.get(), 3);
}
