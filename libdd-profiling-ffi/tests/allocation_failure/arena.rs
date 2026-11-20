// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Arena wiring of the harness.
//!
//! Built with `LIBDD_ALLOCATION_CHAOS_TEST=1`, `libdd-profiling` wraps its arenas in a
//! crate-private proxy that calls the observer stored in `ALLOC_CHAOS_OBSERVER`. Without it,
//! profiling arenas are not observed and the arena-domain checks are skipped. The direct arena
//! scenarios always use the same wrapping built here, around a plain `ChainAllocator`, so that
//! the chunk hint can be small.

use crate::allocator::{self, Domain, Operation};
use libdd_alloc::{AllocError, Allocator, ChainAllocator, VirtualAllocator};
use std::alloc::Layout;
use std::marker::PhantomData;
use std::ptr::NonNull;

/// Whether `libdd-profiling` arenas report to the harness in this build.
pub const PROFILING_ARENAS_OBSERVED: bool = cfg!(allocation_chaos_test);

#[cfg(allocation_chaos_test)]
unsafe extern "Rust" {
    static ALLOC_CHAOS_OBSERVER: std::sync::atomic::AtomicPtr<()>;
}

/// Routes the `libdd-profiling` arena proxy to the allocator's counter. Call it first in `main`.
pub fn register_observer() {
    #[cfg(allocation_chaos_test)]
    {
        let observe: fn(u8, u8, usize, usize) -> bool = allocator::observe_allocation;
        // SAFETY: under this cfg, `libdd-profiling` defines the static as an `AtomicPtr<()>`.
        unsafe {
            ALLOC_CHAOS_OBSERVER.store(
                observe as *const () as *mut (),
                std::sync::atomic::Ordering::Relaxed,
            )
        };
    }
}

pub trait ArenaDomain {
    const DOMAIN: Domain;
}

#[derive(Clone, Copy)]
pub struct Request;

#[derive(Clone, Copy)]
pub struct Backing;

impl ArenaDomain for Request {
    const DOMAIN: Domain = Domain::ArenaRequest;
}

impl ArenaDomain for Backing {
    const DOMAIN: Domain = Domain::ArenaBacking;
}

#[derive(Clone, Copy)]
pub struct Chaos<A, D> {
    inner: A,
    _domain: PhantomData<D>,
}

impl<A, D> Chaos<A, D> {
    pub fn new(inner: A) -> Self {
        Self {
            inner,
            _domain: PhantomData,
        }
    }

    pub fn inner(&self) -> &A {
        &self.inner
    }
}

unsafe impl<A: Allocator, D: ArenaDomain> Allocator for Chaos<A, D> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if allocator::observe(D::DOMAIN, Operation::Alloc, layout.size(), layout.align()) {
            return Err(AllocError);
        }
        self.inner.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if allocator::observe(
            D::DOMAIN,
            Operation::AllocZeroed,
            layout.size(),
            layout.align(),
        ) {
            return Err(AllocError);
        }
        self.inner.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded unchanged to the allocator that made the block.
        unsafe { self.inner.deallocate(ptr, layout) }
    }
}

/// Same shape as the `libdd-profiling` arena with the feature on.
pub type RequestArena = Chaos<ChainAllocator<Chaos<VirtualAllocator, Backing>>, Request>;

pub fn request_arena(chunk_size_hint: usize) -> RequestArena {
    Chaos::new(ChainAllocator::new_in(
        chunk_size_hint,
        Chaos::new(VirtualAllocator {}),
    ))
}
