// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Test-only proxy in front of an [`Allocator`]. It asks the process-wide observer whether this
//! allocation must fail, then forwards the call. `deallocate` is never observed and never fails.
//!
//! The observer lives in the allocation-failure harness and shares its ordinal counter with the
//! global allocator proxy, so one trace can hold `Global`, `ArenaRequest` and `ArenaBacking`
//! events. With no observer registered, every call is forwarded.

use crate::{AllocError, Allocator};
use core::alloc::Layout;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

pub const DOMAIN_ARENA_REQUEST: u8 = 1;
pub const DOMAIN_ARENA_BACKING: u8 = 2;
pub const OP_ALLOC: u8 = 0;
pub const OP_ALLOC_ZEROED: u8 = 1;

/// `(domain, operation, size, align) -> fail`. Must not allocate.
pub type ObserveFn = fn(u8, u8, usize, usize) -> bool;

static OBSERVE: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub fn register_observer(observe: ObserveFn) {
    OBSERVE.store(observe as *const () as *mut (), Ordering::Relaxed);
}

#[inline(always)]
fn observe(domain: u8, op: u8, size: usize, align: usize) -> bool {
    let ptr = OBSERVE.load(Ordering::Relaxed);
    if ptr.is_null() {
        return false;
    }
    // SAFETY: `ptr` was stored from an `ObserveFn` by `register_observer`.
    let observe: ObserveFn = unsafe { core::mem::transmute(ptr) };
    observe(domain, op, size, align)
}

/// Marker: count this allocator's calls as logical arena requests.
#[derive(Clone, Copy, Debug)]
pub struct ArenaRequest;

/// Marker: count this allocator's calls as arena backing-chunk growth.
#[derive(Clone, Copy, Debug)]
pub struct ArenaBacking;

trait ChaosDomain {
    const ID: u8;
}

impl ChaosDomain for ArenaRequest {
    const ID: u8 = DOMAIN_ARENA_REQUEST;
}

impl ChaosDomain for ArenaBacking {
    const ID: u8 = DOMAIN_ARENA_BACKING;
}

/// Forwards to `A`, failing allocations the observer selects.
#[derive(Clone, Copy)]
pub struct ChaosAllocator<A, D> {
    inner: A,
    _domain: PhantomData<D>,
}

impl<A, D> ChaosAllocator<A, D> {
    pub const fn new(inner: A) -> Self {
        Self {
            inner,
            _domain: PhantomData,
        }
    }

    pub fn inner(&self) -> &A {
        &self.inner
    }
}

unsafe impl<A: Allocator, D: ChaosDomain> Allocator for ChaosAllocator<A, D> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if observe(D::ID, OP_ALLOC, layout.size(), layout.align()) {
            return Err(AllocError);
        }
        self.inner.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if observe(D::ID, OP_ALLOC_ZEROED, layout.size(), layout.align()) {
            return Err(AllocError);
        }
        self.inner.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller guarantees `ptr` and `layout` match an allocation from this
        // allocator, which forwarded it to `inner`.
        unsafe { self.inner.deallocate(ptr, layout) }
    }
}
