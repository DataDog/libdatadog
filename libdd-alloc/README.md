# libdd-alloc

Custom memory allocators for specialized allocation patterns.

## Overview

`libdd-alloc` provides specialized memory allocators designed for use in constrained environments such as signal handlers, crash handlers, and performance-critical code paths where standard allocation may not be suitable.

## Features

- **`no_std` Compatible**: Works in environments without standard library support
- **Linear Allocator**: Bump allocator with minimal per-allocation overhead
- **Chain Allocator**: Growable arena allocator that automatically expands
- **Virtual Allocator**: Page-based memory allocation using OS-specific APIs
- **Allocator API**: Implements standard allocator traits for compatibility
