// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::collections::alloc::Global;
use core::cell::Cell;
use rust_alloc::rc::Rc;

#[test]
fn push_pop_extend_and_truncate() {
    let mut vec = TryVec::new_in(Global);
    assert!(vec.is_empty());
    for i in 0..100_u32 {
        vec.try_push(i).unwrap();
    }
    assert_eq!(vec.len(), 100);
    assert!(vec.iter().copied().eq(0..100));
    assert_eq!(vec.pop(), Some(99));
    vec.try_extend_from_slice(&[7, 8]).unwrap();
    assert_eq!(&vec[99..], &[7, 8]);
    vec.truncate(3);
    assert_eq!(vec.as_slice(), &[0, 1, 2]);
    vec.try_resize(5, &9).unwrap();
    assert_eq!(vec.as_slice(), &[0, 1, 2, 9, 9]);
    let copy = vec.try_clone().unwrap();
    assert_eq!(copy, vec);
    vec.clear();
    assert!(vec.is_empty() && vec.capacity() >= 5);
}

#[test]
fn push_within_capacity_never_allocates() {
    let mut vec = TryVec::try_with_capacity_in(2, Global).unwrap();
    assert_eq!(vec.push_within_capacity(1_u8), Ok(()));
    assert_eq!(vec.push_within_capacity(2), Ok(()));
    assert_eq!(vec.push_within_capacity(3), Err(3));
    assert_eq!(vec.capacity(), 2);
}

#[test]
fn elements_are_dropped_once() {
    struct Counted(Rc<Cell<usize>>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let drops = Rc::new(Cell::new(0));
    let mut vec = TryVec::new_in(Global);
    for _ in 0..10 {
        vec.try_push(Counted(drops.clone())).unwrap();
    }
    drop(vec.pop());
    assert_eq!(drops.get(), 1);
    vec.truncate(4);
    assert_eq!(drops.get(), 6);
    drop(vec);
    assert_eq!(drops.get(), 10);
}

#[test]
fn reserve_overflow_is_an_error() {
    let mut vec = TryVec::<u64, Global>::new_in(Global);
    assert_eq!(vec.try_reserve(usize::MAX), Err(AllocError));
    vec.try_push(1).unwrap();
    assert_eq!(vec.try_reserve(usize::MAX), Err(AllocError));
    assert_eq!(vec.as_slice(), &[1]);
}

/// Counts which allocation entry points are used, forwarding to `Global`.
#[derive(Clone, Default)]
struct Counting {
    plain: Rc<Cell<usize>>,
    zeroed: Rc<Cell<usize>>,
}

// SAFETY: forwards to `Global`, which upholds the contract.
unsafe impl Allocator for Counting {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.plain.set(self.plain.get() + 1);
        Global.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.zeroed.set(self.zeroed.get() + 1);
        Global.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the block came from `Global` with `layout`.
        unsafe { Global.deallocate(ptr, layout) }
    }
}

#[test]
fn zeroed_vectors_come_from_allocate_zeroed() {
    let alloc = Counting::default();
    let vec = TryVec::<u16, _>::try_zeroed_in(1000, alloc.clone()).unwrap();
    assert_eq!(vec.len(), 1000);
    assert_eq!(vec.capacity(), 1000);
    assert!(vec.iter().all(|&x| x == 0));
    assert_eq!((alloc.plain.get(), alloc.zeroed.get()), (0, 1));

    // Empty: nothing allocated.
    let empty = TryVec::<u8, _>::try_zeroed_in(0, alloc.clone()).unwrap();
    assert!(empty.is_empty());
    assert_eq!(alloc.zeroed.get(), 1);

    // Ordinary use afterwards: growing past the zeroed length works.
    let mut vec = vec;
    vec.try_push(7).unwrap();
    assert_eq!(vec.len(), 1001);
    assert_eq!(vec[1000], 7);
}

#[test]
fn zeroed_overflow_is_an_error() {
    assert!(TryVec::<u64, Global>::try_zeroed_in(usize::MAX, Global).is_err());
}
