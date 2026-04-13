# libdd-capabilities-impl

Native implementations of `libdd-capabilities` traits.

## Overview

`libdd-capabilities-impl` provides platform-native backends for the capability traits defined in `libdd-capabilities`. It is the concrete implementation used by native leaf crates (FFI bindings, benchmarks, tests) and should **not** be depended on by core library crates.

## Capabilities

- **`NativeHttpClient`**: HTTP client backed by hyper and the `libdd-common` connector infrastructure (supports Unix sockets, HTTPS with rustls, Windows named pipes).
- **`NativeSleepCapability`**: Sleep backed by `tokio::time::sleep`.
- **`NativeSpawnCapability`**: Task spawning backed by `tokio::runtime::Handle::spawn`.

## Types

- **`NativeCapabilities`**: Bundle struct that implements all capability traits using native backends. Delegates to `NativeHttpClient`, `NativeSleepCapability`, and `NativeSpawnCapability`.

## Usage

Native leaf crates pin the concrete capability type:

```rust
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::TraceExporter;

let exporter: TraceExporter<NativeCapabilities> = TraceExporterBuilder::new(/* ... */).build();
```
