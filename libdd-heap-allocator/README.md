# libdd-heap-allocator

Rust `GlobalAlloc` wrapper with USDT-based heap profiling, effectively implementing [libdd-heap-sampler](../libdd-heap-sampler) for Rust apps at compile time.

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

## Running the demo

The demo builds a small Rust binary that allocates in a loop, fires USDT probes via `libdd-heap-sampler`, and lets you observe them live with `bpftrace`. USDT + eBPF require Linux, so the Makefile runs everything inside a [Lima](https://lima-vm.io/) VM — Lima handles the macOS ↔ Linux boundary transparently by mounting your home directory into the VM.

From this directory:

```sh
# Apple Silicon
make lima-demo-arm64

# Intel
make lima-demo-amd64
```

This will start the Lima VM if it isn't already running, build the demo inside it, launch the binary, and attach `bpftrace` — you'll see `alloc`/`free` USDT events printed as they fire. `Ctrl+C` tears everything down.