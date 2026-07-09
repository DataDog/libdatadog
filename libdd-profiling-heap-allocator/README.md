# libdd-heap-allocator

Rust `GlobalAlloc` wrapper with USDT-based heap profiling, effectively implementing [libdd-heap-sampler](../libdd-heap-sampler) for Rust apps at compile time. This lets Rust users quickly setup sampled heap profiling within their application regardless of the particular allocator they are using.

For this to work _well_, you should make sure everything passes through the global allocator!

Usage:

```rust
use libdd_heap_allocator::SampledAllocator;
use std::alloc::System;

// Wrap the default system allocator 
#[global_allocator]
static ALLOC: SampledAllocator<System> = SampledAllocator::<System>::DEFAULT;
```

To wrap a custom allocator instead:

```rust
#[global_allocator]
static ALLOC: SampledAllocator<MyAllocator> = SampledAllocator::new(MyAllocator::new());
```

For profiling, prefer wrapping the allocator that is actually installed as the
process global allocator. Heap profiling is most useful when all allocations in
the process pass through the sampled wrapper.

See [`examples/usdt_demo.rs`](examples/usdt_demo.rs) for a runnable demo that fires USDT probes in a loop for `bpftrace` to observe.

## Benchmarking sampler overhead

The `sampler_overhead` Criterion benchmark measures the allocator/sampler hot path without installing `SampledAllocator` as the process global allocator. It compares direct `System` allocation, `SampledAllocator<System>`, a no-op allocator, `SampledAllocator<NoopAllocator>`, and direct sampler calls.

```sh
cargo bench -p libdd-heap-allocator --bench sampler_overhead
```

One quick validation run produced these fast-path results:

| Size | Base: `System` alloc/free | Sampled fast path | Overhead | Overhead % |
|---:|---:|---:|---:|---:|
| 16 B | 5.9719 ns | 10.916 ns | +4.9441 ns | +82.8% |
| 64 B | 5.9309 ns | 12.405 ns | +6.4741 ns | +109.2% |
| 256 B | 5.9639 ns | 10.827 ns | +4.8631 ns | +81.5% |
| 4096 B | 23.237 ns | 29.402 ns | +6.1650 ns | +26.5% |
| 65536 B | 23.880 ns | 28.496 ns | +4.6160 ns | +19.3% |

The no-op allocator comparison isolates the wrapper/sampler fast path at roughly **+5–6 ns per alloc/free pair**.
