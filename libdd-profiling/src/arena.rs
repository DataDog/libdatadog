// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Profiling arena used by the string table and the profile sets.
//!
//! This is `ChainAllocator<VirtualAllocator>`. Only under the test-only cfg
//! `allocation_chaos_test` (see `build.rs`) are logical requests and backing-chunk growth
//! wrapped, so the allocation-failure harness can fail either one. Nothing here is public API.

use libdd_alloc::{ChainAllocator, VirtualAllocator};

#[cfg(not(allocation_chaos_test))]
pub(crate) type ProfilingArena = ChainAllocator<VirtualAllocator>;

#[cfg(allocation_chaos_test)]
pub(crate) type ProfilingArena = chaos::ChaosAllocator<
    ChainAllocator<chaos::ChaosAllocator<VirtualAllocator, chaos::ArenaBacking>>,
    chaos::ArenaRequest,
>;

pub(crate) fn profiling_arena(chunk_size_hint: usize) -> ProfilingArena {
    #[cfg(not(allocation_chaos_test))]
    {
        ChainAllocator::new_in(chunk_size_hint, VirtualAllocator {})
    }
    #[cfg(allocation_chaos_test)]
    {
        let backing = chaos::ChaosAllocator::new(VirtualAllocator {});
        chaos::ChaosAllocator::new(ChainAllocator::new_in(chunk_size_hint, backing))
    }
}

/// Test-only proxy in front of an arena allocator. It asks the harness observer whether this
/// allocation must fail, then forwards the call. `deallocate` is never observed and never fails.
#[cfg(allocation_chaos_test)]
pub(crate) mod chaos {
    use crate::collections::string_table::ArenaAllocator;
    use core::alloc::Layout;
    use core::marker::PhantomData;
    use core::ptr::NonNull;
    use core::sync::atomic::{AtomicPtr, Ordering};
    use libdd_alloc::{AllocError, Allocator, ChainAllocator};

    const DOMAIN_ARENA_REQUEST: u8 = 1;
    const DOMAIN_ARENA_BACKING: u8 = 2;
    const OP_ALLOC: u8 = 0;
    const OP_ALLOC_ZEROED: u8 = 1;

    /// `fn(domain, operation, size, align) -> fail`, stored by the allocation-failure harness.
    /// The harness reaches it by symbol name, so it is not part of the Rust API. Null means
    /// every call is forwarded.
    #[unsafe(no_mangle)]
    static ALLOC_CHAOS_OBSERVER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

    #[inline(always)]
    fn observe(domain: u8, op: u8, size: usize, align: usize) -> bool {
        let ptr = ALLOC_CHAOS_OBSERVER.load(Ordering::Relaxed);
        if ptr.is_null() {
            return false;
        }
        // SAFETY: the harness only stores a `fn(u8, u8, usize, usize) -> bool`.
        let observe: fn(u8, u8, usize, usize) -> bool = unsafe { core::mem::transmute(ptr) };
        observe(domain, op, size, align)
    }

    #[derive(Clone, Copy)]
    pub(crate) struct ArenaRequest;

    #[derive(Clone, Copy)]
    pub(crate) struct ArenaBacking;

    pub(crate) trait ChaosDomain {
        const ID: u8;
    }

    impl ChaosDomain for ArenaRequest {
        const ID: u8 = DOMAIN_ARENA_REQUEST;
    }

    impl ChaosDomain for ArenaBacking {
        const ID: u8 = DOMAIN_ARENA_BACKING;
    }

    #[derive(Clone, Copy)]
    pub(crate) struct ChaosAllocator<A, D> {
        inner: A,
        _domain: PhantomData<D>,
    }

    impl<A, D> ChaosAllocator<A, D> {
        pub(crate) const fn new(inner: A) -> Self {
            Self {
                inner,
                _domain: PhantomData,
            }
        }
    }

    unsafe impl<A: Allocator, D: ChaosDomain> Allocator for ChaosAllocator<A, D> {
        #[inline(always)]
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            if observe(D::ID, OP_ALLOC, layout.size(), layout.align()) {
                return Err(AllocError);
            }
            self.inner.allocate(layout)
        }

        #[inline(always)]
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

    impl<A: Allocator + Clone> ArenaAllocator for ChaosAllocator<ChainAllocator<A>, ArenaRequest> {}
}

/// Production builds compile only `ChainAllocator::new_in`. The chaos wrapper, its observer
/// load, and `ALLOC_CHAOS_OBSERVER` are not in this crate unless `allocation_chaos_test` is set,
/// so there is no runtime flag, branch, or TLS on the allocator path.
#[cfg(all(test, not(allocation_chaos_test)))]
mod production {
    use super::profiling_arena;
    use libdd_alloc::{ChainAllocator, VirtualAllocator};

    #[test]
    fn arena_is_the_original_allocator() {
        let arena = profiling_arena(4096);
        let _typed: &ChainAllocator<VirtualAllocator> = &arena;
    }
}
