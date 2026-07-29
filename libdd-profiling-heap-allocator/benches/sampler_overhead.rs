// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// The sampler/allocator items exercised here are Linux-only; on other
// targets the bench compiles to a no-op `main` so workspace-wide
// `cargo check --all-targets` doesn't fail.

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
criterion::criterion_main!(linux_bench::benches);

#[cfg(target_os = "linux")]
mod linux_bench {
    use criterion::{criterion_group, BenchmarkId, Criterion};
    use libdd_profiling_heap_allocator::SampledAllocator;
    use libdd_profiling_heap_sampler::{dd_test_set_profiler_active, dd_tl_state_get_or_init};
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::hint::black_box;
    use std::ptr;

    const SIZES: &[usize] = &[16, 64, 256, 4096, 65_536];
    const ALIGN: usize = 8;

    #[repr(align(4096))]
    struct AlignedBuffer([u8; 128 * 1024]);

    static mut NOOP_BUFFER: AlignedBuffer = AlignedBuffer([0; 128 * 1024]);

    struct NoopAllocator;

    unsafe impl GlobalAlloc for NoopAllocator {
        unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
            // Return a stable aligned pointer with mapped bytes before it.
            // The sampler's free path may inspect header-sized bytes
            // immediately before the user pointer when checking for sampled
            // allocations. Always returns the same fixed pointer - this
            // allocator exists purely to eliminate the real allocator's cost
            // from benchmarks.
            unsafe { ptr::addr_of_mut!(NOOP_BUFFER.0).cast::<u8>().add(4096) }
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    /// # Safety
    ///
    /// Must be called on a thread that isn't concurrently tearing down its
    /// TLS (i.e. not from a destructor); otherwise identical to
    /// `dd_tl_state_get_or_init`'s own safety contract.
    unsafe fn sampler_tl_state() -> *mut libdd_profiling_heap_sampler::dd_tl_state_t {
        unsafe { dd_tl_state_get_or_init() }
    }

    /// Pins this thread's sampler state onto the fast (unsampled) path for
    /// the rest of the benchmark. `remaining_bytes` starts at a huge
    /// negative value and `sampling_interval` at a huge positive one, so
    /// benchmark-sized allocations can never drive `remaining_bytes`
    /// non-negative and trigger the slow/sampled path. Dividing by 4 keeps
    /// headroom against overflow while summing allocation sizes.
    unsafe fn pin_sampler_to_fast_path() {
        let tl = unsafe { sampler_tl_state() };
        if !tl.is_null() {
            unsafe {
                (*tl).sampling_interval = u64::MAX / 4;
                (*tl).remaining_bytes = i64::MIN / 4;
                (*tl).remaining_bytes_initialized = true;
                (*tl).reentry_guard = false;
            }
        }
    }

    /// Forces the next allocation on this thread onto the slow/sampled
    /// path. `512 * 1024` matches `DD_SAMPLING_INTERVAL_DEFAULT` (see
    /// tl_state.h), so this benchmarks the sampled path against the same
    /// interval used in production rather than an arbitrary value.
    unsafe fn force_next_allocation_to_sample() {
        let tl = unsafe { sampler_tl_state() };
        if !tl.is_null() {
            unsafe {
                (*tl).sampling_interval = 512 * 1024;
                (*tl).remaining_bytes = 0;
                (*tl).remaining_bytes_initialized = true;
                (*tl).reentry_guard = false;
            }
        }
    }

    // ── Baseline ───────────────────────────────────────────────────────────
    // Pure system allocator cost with no sampler in the picture.

    fn bench_system_alloc_free(c: &mut Criterion) {
        let mut group = c.benchmark_group("alloc_free/system");
        for &size in SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(size), &layout, |b, &layout| {
                b.iter(|| unsafe {
                    let ptr = System.alloc(layout);
                    black_box(ptr);
                    System.dealloc(ptr, layout);
                });
            });
        }
        group.finish();
    }

    // ── Profiler attached (semaphore ON) ─────────────────────────────────
    // The primary benchmark set. Semaphore is flipped on to simulate a
    // profiler being attached. This is the realistic production scenario.
    //
    // Fast-path: `remaining_bytes` is pinned far from zero so allocations
    // are never sampled. Measures per-allocation overhead when the profiler
    // is attached but this particular alloc isn't selected.
    //
    // Slow-path: `force_next_allocation_to_sample()` triggers sampling
    // every iteration. The USDT probe fires (into a NOP since no real
    // consumer is attached to the uprobe).

    fn bench_fast_path_system(c: &mut Criterion) {
        let alloc = SampledAllocator::new(System);
        let mut group = c.benchmark_group("profiler_attached/fast_path_system");
        unsafe { dd_test_set_profiler_active(true) };
        for &size in SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(size), &layout, |b, &layout| {
                unsafe { pin_sampler_to_fast_path() };
                b.iter(|| unsafe {
                    let ptr = alloc.alloc(layout);
                    black_box(ptr);
                    alloc.dealloc(ptr, layout);
                });
            });
        }
        group.finish();
        unsafe { dd_test_set_profiler_active(false) };
    }

    fn bench_fast_path_noop(c: &mut Criterion) {
        let alloc = SampledAllocator::new(NoopAllocator);
        let mut group = c.benchmark_group("profiler_attached/fast_path_noop");
        unsafe { dd_test_set_profiler_active(true) };
        for &size in SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(size), &layout, |b, &layout| {
                unsafe { pin_sampler_to_fast_path() };
                b.iter(|| unsafe {
                    let ptr = alloc.alloc(layout);
                    black_box(ptr);
                    alloc.dealloc(ptr, layout);
                });
            });
        }
        group.finish();
        unsafe { dd_test_set_profiler_active(false) };
    }

    fn bench_slow_path_system(c: &mut Criterion) {
        let alloc = SampledAllocator::new(System);
        let mut group = c.benchmark_group("profiler_attached/slow_path_system");
        unsafe { dd_test_set_profiler_active(true) };
        for &size in SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(size), &layout, |b, &layout| {
                b.iter(|| unsafe {
                    force_next_allocation_to_sample();
                    let ptr = alloc.alloc(layout);
                    black_box(ptr);
                    alloc.dealloc(ptr, layout);
                });
            });
        }
        group.finish();
        unsafe { dd_test_set_profiler_active(false) };
    }

    fn bench_slow_path_noop(c: &mut Criterion) {
        let alloc = SampledAllocator::new(NoopAllocator);
        let mut group = c.benchmark_group("profiler_attached/slow_path_noop");
        unsafe { dd_test_set_profiler_active(true) };
        for &size in SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(size), &layout, |b, &layout| {
                b.iter(|| unsafe {
                    force_next_allocation_to_sample();
                    let ptr = alloc.alloc(layout);
                    black_box(ptr);
                    alloc.dealloc(ptr, layout);
                });
            });
        }
        group.finish();
        unsafe { dd_test_set_profiler_active(false) };
    }

    // ── Short-circuit regression (semaphore OFF) ─────────────────────────
    // Single benchmark with the semaphore off (no profiler attached).
    // The semaphore check in dd_allocation_requested short-circuits before
    // any TLS access or sampling logic. This validates that the
    // short-circuit path stays near-zero cost.

    fn bench_short_circuit(c: &mut Criterion) {
        let alloc = SampledAllocator::new(System);
        let mut group = c.benchmark_group("no_profiler/short_circuit");
        // Semaphore is off by default - don't flip it on.
        for &size in SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(size), &layout, |b, &layout| {
                b.iter(|| unsafe {
                    let ptr = alloc.alloc(layout);
                    black_box(ptr);
                    alloc.dealloc(ptr, layout);
                });
            });
        }
        group.finish();
    }

    criterion_group!(
        benches,
        bench_system_alloc_free,
        bench_fast_path_system,
        bench_fast_path_noop,
        bench_slow_path_system,
        bench_slow_path_noop,
        bench_short_circuit,
    );
} // mod linux_bench
