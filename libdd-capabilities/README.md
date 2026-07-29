# libdd-capabilities

Portable capability traits for cross-platform libdatadog.

## Overview

`libdd-capabilities` defines abstract traits that decouple core libdatadog crates from platform-specific I/O. Core crates are generic over a capability parameter `C` that implements these traits, allowing native and wasm targets to provide different backends without changing shared logic.

This crate has **zero platform dependencies**: it compiles on any target including `wasm32-unknown-unknown`.

## Traits

- **`HttpClientCapability`**: Async HTTP request/response using `http::Request<Bytes>` / `http::Response<Bytes>`.
- **`MaybeSend`**: Conditional `Send` bound: equivalent to `Send` on native, auto-implemented for all types on wasm. This bridges the gap between tokio's multi-threaded runtime (requires `Send` futures) and wasm's single-threaded model (where JS interop types are `!Send`).

## Architecture

Three-layer design:

1. **Trait definitions** (this crate): Pure traits, no platform deps.
2. **Core crates** (`libdd-trace-utils`, `libdd-data-pipeline`): Generic over `C: HttpClientCapability`. Depend only on this crate for trait bounds.
3. **Leaf crates** (FFI, wasm bindings): Pin a concrete type, `NativeCapabilities` from `libdd-capabilities-impl` on native, `WasmCapabilities` from the Node.js binding crate on wasm.

## Usage

```rust
use libdd_capabilities::{HttpClientCapability, MaybeSend};

async fn fetch<C: HttpClientCapability>(client: &C, req: http::Request<bytes::Bytes>) {
    let response = client.request(req).await.unwrap();
    println!("status: {}", response.status());
}
```

**Critical rule**: never use `+ Send` directly in trait bounds for async functions in wasm-compatible code. Always use `+ MaybeSend` instead.
