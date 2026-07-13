# libdd-profiling-heap-jemalloc-preload

An `LD_PRELOAD`-able shared object that makes `jemalloc` the process allocator **and** turns on [libdd-profiling-heap-jemalloc](../libdd-profiling-heap-jemalloc)'s sampling hooks — for programs you can't or don't want to recompile with `#[global_allocator]` and a call to `install()`.

```sh
cargo build -p libdd-profiling-heap-jemalloc-preload --release
LD_PRELOAD=target/release/libdd_profiling_heap_jemalloc_preload.so ./your-program
```

The preloaded program's `malloc`/`free`/etc. bind to `jemalloc`, and the crate installs the `prof_sample`/`prof_sample_free` hooks at load time, so `ddheap:alloc`/`ddheap:free` USDTs start firing with no change to the program itself.

## How it works

Two small, Linux-only pieces (see `src/lib.rs`):

1. **Allocator interposition.** Links the (prefixed) `jemalloc` from `tikv-jemalloc-sys` and re-exports the libc allocator entry points as thin `#[no_mangle]` forwarders to `jemalloc`'s prefixed symbols. Those forwarders are what the dynamic linker binds to — a plain `cdylib` can't re-export the statically-linked `jemalloc` symbols directly, because `rustc` emits a version script that marks every non-Rust symbol local. No allocator logic is reimplemented; each shim is a one-line hand-off.
2. **Hook installation at load.** An `.init_array` constructor calls `libdd_profiling_heap_jemalloc::install()` as the library loads.

## Configuration

Same knobs as `libdd-profiling-heap-jemalloc`: `install` resets `jemalloc` to the 512 KiB default interval and flips `prof.active` on, and `DD_HEAP_SAMPLING_ENABLED=0` disables the integration (`install` becomes a no-op). The required `prof:true,prof_active:false` `MALLOC_CONF` is baked into the `tikv-jemalloc-sys` build by its `profiling_hooks` feature.

## Caveats

- **Linux only.** Interposition relies on plain libc allocator symbols; on macOS `jemalloc` needs the malloc-zone registration trick, which this crate does not do. On non-Linux targets it builds to an inert empty cdylib.
- The forwarded symbols (`malloc`, `calloc`, `realloc`, `free`, `posix_memalign`, `aligned_alloc`, `malloc_usable_size`) cover normal programs. A program using more exotic entry points (`memalign`, `valloc`, ...) would need those forwarders added too.
