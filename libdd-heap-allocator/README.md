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

To wrap a custom allocator instead; note that this is kind of ill-advised; we want to see _all_ allocations for the process:

```rust
#[global_allocator]
static ALLOC: SampledAllocator<MyAllocator> = SampledAllocator::new(MyAllocator::new());
```

See [`examples/usdt_demo.rs`](examples/usdt_demo.rs) for a runnable demo that fires USDT probes in a loop for `bpftrace` to observe.