# libdd-alloc

Custom `no_std` compatible memory allocators for specialized allocation patterns.

## Overview

`libdd-alloc` provides high-performance arena-style allocators and virtual memory allocators designed for use in profiling, crash tracking, and other contexts where standard allocation may not be safe or efficient.

## Features

- **`no_std` compatible**: Works in signal handlers and crash handlers
- **Linear Allocator**: Fast bump/arena allocator with no per-allocation overhead
- **Chain Allocator**: Automatically chains new arenas when full
- **Virtual Allocator**: Page-based allocator for large memory chunks
- **allocator_api2**: Implements standard allocator traits

## Allocators

### LinearAllocator

A simple arena allocator that bump-allocates from a fixed-size buffer. Individual deallocations are no-ops; memory is freed when the entire allocator is dropped.

```rust
use libdd_alloc::{LinearAllocator, VirtualAllocator};
use std::alloc::Layout;

let layout = Layout::from_size_align(4096, 4096)?;
let allocator = LinearAllocator::new_in(layout, VirtualAllocator::default())?;

// Allocate from the arena
let memory = allocator.allocate(Layout::new::<u64>())?;
```

### ChainAllocator

An arena allocator that automatically creates new linear allocators when the current one is full, chaining them together.

```rust
use libdd_alloc::ChainAllocator;
use std::alloc::System;

let allocator = ChainAllocator::new_in(4096, System)?;
// Automatically grows as needed
```

### VirtualAllocator

Allocates entire pages of virtual memory using OS-specific APIs (`mmap` on Unix, `VirtualAlloc` on Windows).

```rust
use libdd_alloc::VirtualAllocator;

let allocator = VirtualAllocator::default();
// Allocates in page-sized chunks
```

## Use Cases

- **Profiling**: Allocate memory for profiling data without interfering with allocation tracking
- **Signal Handlers**: Allocate memory in signal-safe contexts
- **Crash Handlers**: Allocate memory during crash handling when the heap may be corrupted
- **Performance**: Reduce allocation overhead for short-lived data

