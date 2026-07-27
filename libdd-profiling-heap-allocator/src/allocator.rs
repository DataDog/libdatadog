// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::alloc::{GlobalAlloc, Layout};
use std::alloc::System;

#[cfg(target_os = "linux")]
use libdd_profiling_heap_sampler::{
    dd_allocation_created, dd_allocation_freed, dd_allocation_requested,
};

/// `GlobalAlloc` wrapper that routes each alloc/dealloc through
/// `libdd-profiling-heap-sampler` before forwarding to the inner allocator `A`.
///
/// The default `realloc` / `alloc_zeroed` impls from [`GlobalAlloc`] are
/// inherited; they dispatch back to `alloc` / `dealloc`, so sampling
/// still fires for those paths.
///
/// On non-Linux targets the sampler integration is a no-op and this is
/// just a transparent forwarder to `inner`; callers can use the same
/// `#[global_allocator]` setup unconditionally.
pub struct SampledAllocator<A> {
    inner: A,
}

impl<A> SampledAllocator<A> {
    /// Wrap an allocator. `const` so it's usable directly in a
    /// `#[global_allocator]` static.
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

impl SampledAllocator<System> {
    /// Default wrap of the system allocator, usable directly in a
    /// `#[global_allocator]` static.
    pub const DEFAULT: Self = Self { inner: System };
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for SampledAllocator<A> {
    #[cfg(target_os = "linux")]
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // when sampling is disabled via
        // DD_HEAP_SAMPLING_ENABLED, forward straight to the inner allocator
        // so we're indistinguishable from an unwrapped allocator. realloc /
        // alloc_zeroed are inherited from GlobalAlloc and dispatch back
        // through alloc/dealloc, so they pass through too.
        if !libdd_profiling_heap_sampler::heap_sampling_enabled() {
            return self.inner.alloc(layout);
        }
        let req = dd_allocation_requested(layout.size(), layout.align());
        // Sampled paths may bump the size for inline flag storage;
        // forward the returned size to the inner allocator verbatim.
        let inner_layout = Layout::from_size_align_unchecked(req.size, layout.align());
        let raw = self.inner.alloc(inner_layout);
        dd_allocation_created(raw.cast(), req).cast()
    }

    #[cfg(not(target_os = "linux"))]
    #[inline(always)]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.inner.alloc(layout)
    }

    #[cfg(target_os = "linux")]
    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Honour bypass
        if !libdd_profiling_heap_sampler::heap_sampling_enabled() {
            return self.inner.dealloc(ptr, layout);
        }
        let freed = dd_allocation_freed(ptr.cast(), layout.size(), layout.align());
        // `layout.align()` is reused here rather than anything derived from
        // `freed`: alignment never changes between the original `alloc` and
        // this `dealloc`, so pairing `freed.size` with the caller's own
        // alignment is exactly what `GlobalAlloc::dealloc`'s safety contract
        // already requires (the layout passed here must match the one used
        // for the original allocation). This isn't a guarantee `SampledAllocator`
        // adds — it's just satisfying the contract our caller is on the hook for.
        let inner_layout = Layout::from_size_align_unchecked(freed.size, layout.align());
        self.inner.dealloc(freed.ptr.cast(), inner_layout);
    }

    #[cfg(not(target_os = "linux"))]
    #[inline(always)]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner.dealloc(ptr, layout);
    }
}

// Tests dispatch through the wrapped allocator and (on Linux) into the
// sampler's C-side TLS primitives via FFI; miri can't execute those.
#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(target_os = "linux")]
    use libdd_profiling_heap_sampler::dd_tl_state_get;

    /// Minimal `GlobalAlloc` that forwards to `System` while recording
    /// counters so tests can assert the sampled wrapper passed the right
    /// size/align through.
    struct CountingSystem {
        alloc_count: AtomicUsize,
        dealloc_count: AtomicUsize,
        last_alloc_size: AtomicUsize,
    }

    impl CountingSystem {
        const fn new() -> Self {
            Self {
                alloc_count: AtomicUsize::new(0),
                dealloc_count: AtomicUsize::new(0),
                last_alloc_size: AtomicUsize::new(0),
            }
        }
    }

    unsafe impl GlobalAlloc for CountingSystem {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            self.alloc_count.fetch_add(1, Ordering::Relaxed);
            self.last_alloc_size.store(layout.size(), Ordering::Relaxed);
            System.alloc(layout)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            self.dealloc_count.fetch_add(1, Ordering::Relaxed);
            System.dealloc(ptr, layout);
        }
    }

    #[test]
    fn alloc_dealloc_forwards_to_inner() {
        let sampled = SampledAllocator::new(CountingSystem::new());
        let layout = Layout::from_size_align(128, 16).unwrap();

        unsafe {
            let p = sampled.alloc(layout);
            assert!(!p.is_null());

            assert_eq!(sampled.inner.alloc_count.load(Ordering::Relaxed), 1);
            // Sampler may bump size for flag storage once that's wired up;
            // today it returns the requested size verbatim.
            assert!(sampled.inner.last_alloc_size.load(Ordering::Relaxed) >= 128);

            sampled.dealloc(p, layout);
            assert_eq!(sampled.inner.dealloc_count.load(Ordering::Relaxed), 1);
        }
    }

    // Touches sampler TLS internals, which only exist on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn lazy_init_populates_tls_on_first_alloc() {
        // Spin a fresh thread so we start with uninitialized sampler TLS.
        std::thread::spawn(|| unsafe {
            assert!(
                dd_tl_state_get().is_null(),
                "fresh thread should have NULL sampler TLS"
            );

            let sampled = SampledAllocator::<System>::DEFAULT;
            let layout = Layout::from_size_align(64, 8).unwrap();
            let p = sampled.alloc(layout);
            assert!(!p.is_null());

            assert!(
                !dd_tl_state_get().is_null(),
                "TLS should be populated after the first alloc"
            );

            sampled.dealloc(p, layout);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn default_matches_new_system() {
        let _ = SampledAllocator::<System>::DEFAULT;
        let _ = SampledAllocator::new(System);
    }
}
