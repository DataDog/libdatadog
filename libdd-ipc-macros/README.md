# libdd-ipc-macros

Procedural macros for libdd-ipc.

## Overview

`libdd-ipc-macros` provides procedural macros used by `libdd-ipc` for IPC interface generation and compile-time code generation.

## Macros

The crate provides procedural macros for:
- IPC service interface generation
- RPC method generation
- Serialization helpers

## Usage

This crate is typically used as an implementation detail of `libdd-ipc` and not used directly.

```rust
// Used via libdd-ipc
use libdd_ipc_macros::*;
```

