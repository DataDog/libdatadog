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
    use libdd_profiling_heap_sampler::{
        dd_allocation_created, dd_allocation_freed, dd_allocation_requested,
        dd_tl_state_get_or_init,
    };
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
            // Return a stable aligned pointer with mapped bytes before it. The sampler's free path
            // may inspect header-sized bytes immediately before the user pointer when
            // checking for sampled allocations.
            //
            // Always returns the same fixed pointer. This allocator isn't tracking real
            // capacity or state; it exists purely to eliminate the real allocator's cost
            // from the benchmark so it measures the sampler's own overhead.
            unsafe { ptr::addr_of_mut!(NOOP_BUFFER.0).cast::<u8>().add(4096) }
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    unsafe fn noop_user_ptr() -> *mut u8 {
        unsafe { ptr::addr_of_mut!(NOOP_BUFFER.0).cast::<u8>().add(4096) }
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

    fn bench_sampled_system_alloc_free(c: &mut Criterion) {
        let alloc = SampledAllocator::new(System);
        let mut group = c.benchmark_group("alloc_free/sampled_system_fast_path");
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
    }

    fn bench_noop_alloc_free(c: &mut Criterion) {
        let alloc = NoopAllocator;
        let mut group = c.benchmark_group("alloc_free/noop");
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

    fn bench_sampled_noop_alloc_free(c: &mut Criterion) {
        let alloc = SampledAllocator::new(NoopAllocator);
        let mut group = c.benchmark_group("alloc_free/sampled_noop_fast_path");
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
    }

    fn bench_sampler_only(c: &mut Criterion) {
        let mut group = c.benchmark_group("sampler_only/fast_path");
        for &size in SIZES {
            group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
                unsafe { pin_sampler_to_fast_path() };
                b.iter(|| unsafe {
                    let req = dd_allocation_requested(black_box(size), black_box(ALIGN));
                    let user = dd_allocation_created(black_box(noop_user_ptr()).cast(), req);
                    let freed = dd_allocation_freed(user, black_box(size), black_box(ALIGN));
                    black_box(freed);
                });
            });
        }
        group.finish();
    }

    fn bench_sampled_system_slow_path(c: &mut Criterion) {
        let alloc = SampledAllocator::new(System);
        let mut group = c.benchmark_group("alloc_free/sampled_system_slow_path");
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
    }

    fn bench_sampled_noop_slow_path(c: &mut Criterion) {
        let alloc = SampledAllocator::new(NoopAllocator);
        let mut group = c.benchmark_group("alloc_free/sampled_noop_slow_path");
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
    }

    fn bench_sampler_only_slow_path(c: &mut Criterion) {
        let mut group = c.benchmark_group("sampler_only/slow_path");
        for &size in SIZES {
            group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
                b.iter(|| unsafe {
                    force_next_allocation_to_sample();
                    let req = dd_allocation_requested(black_box(size), black_box(ALIGN));
                    let user = dd_allocation_created(black_box(noop_user_ptr()).cast(), req);
                    let freed = dd_allocation_freed(user, black_box(size), black_box(ALIGN));
                    black_box(freed);
                });
            });
        }
        group.finish();
    }

    criterion_group!(
        benches,
        bench_system_alloc_free,
        bench_sampled_system_alloc_free,
        bench_noop_alloc_free,
        bench_sampled_noop_alloc_free,
        bench_sampler_only,
        bench_sampled_system_slow_path,
        bench_sampled_noop_slow_path,
        bench_sampler_only_slow_path,
    );
} // mod linux_bench
