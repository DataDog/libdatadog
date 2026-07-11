# libdd-profiling-heap-jemalloc

Drives [libdd-profiling-heap-sampler](../libdd-profiling-heap-sampler)'s `ddheap:alloc`/`ddheap:free` USDTs from `jemalloc`'s own experimental sampling hooks, instead of wrapping the allocator with a second, independent sampler like [libdd-profiling-heap-allocator](../libdd-profiling-heap-allocator) does.

`jemalloc` already runs a Poisson sampler internally to decide which allocations to profile. Rather than duplicate that decision, this crate installs `jemalloc`'s `experimental.hooks.prof_sample`/`prof_sample_free` hooks (exposed by [`tikv-jemalloc-ctl`'s `profiling_hooks` feature](https://github.com/scottgerring/jemallocator)) and fires the same USDTs an external profiler (e.g. the eBPF full host profiler) already knows how to consume. It also installs a no-op backtrace hook, so `jemalloc` stops walking the stack itself — the external profiler captures its own.

Usage:

```rust
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    libdd_profiling_heap_jemalloc::install().unwrap();
    // ... rest of the application ...
}
```

`jemalloc` must actually be the process's allocator, built with `profiling_hooks` enabled (which also bakes in `prof:true,prof_active:false`, so profiling is installable but inert until a consumer flips `prof.active` — `install` does this for you).

See [`examples/jemalloc_demo.rs`](examples/jemalloc_demo.rs) for a runnable demo that fires USDT probes in a loop for `bpftrace` to observe.

## `tikv-jemalloc-ctl` dependency

This crate currently points at [scottgerring/jemallocator](https://github.com/scottgerring/jemallocator)'s `scottgerring/profiling-hooks` branch (a fork of `tikv/jemallocator`), which adds the `profiling_hooks` feature these hooks depend on. Switch back to a released `tikv-jemalloc-ctl` once that feature lands upstream — see [tikv/jemallocator#160](https://github.com/tikv/jemallocator/issues/160).

## Sample weight

`install` resets `jemalloc`'s sampling interval to `libdd-profiling-heap-sampler`'s own `DD_SAMPLING_INTERVAL_DEFAULT` (512 KiB) before installing the hooks. Since `jemalloc` invokes `prof_sample` exactly once per sampling decision (there's no per-hook-call "this represents N samples" concept on `jemalloc`'s side), every hook call is `nsamples = 1` against that known interval — so the weight passed to `dd_probe_alloc` is just the interval itself, matching `libdd-profiling-heap-sampler`'s own `nsamples * interval` weight contract exactly, with no approximation and no dependence on `usable_size`.

The one caveat: if something else calls `prof.reset` after `install` runs, `jemalloc`'s live interval changes underneath this crate and the fixed weight it reports is wrong until `install` runs again. See the doc comment on `install` in `src/lib.rs`.
