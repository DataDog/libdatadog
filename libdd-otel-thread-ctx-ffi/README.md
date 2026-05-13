# libdd-otel-thread-ctx-ffi

FFI bindings for the OTel thread-level context publisher. Exposes a C API
for attaching, detaching, and updating per-thread OpenTelemetry context records
that external readers (e.g. the eBPF profiler) can discover via the dynamic
symbol table.

Currently Linux-only (x86-64 and aarch64).

## Building

### Default build

```bash
cargo build --release -p libdd-otel-thread-ctx-ffi
```

The C TLS shim is compiled with the system `cc` (gcc or clang). On x86-64,
`-mtls-dialect=gnu2` forces TLSDESC. No cross-language inlining occurs.

### Optimized build (cross-language LTO)

```bash
./build-optimized.sh
```

This sets `LIBDD_OTEL_THREAD_CTX_INLINE=1` and the appropriate target-scoped
`RUSTFLAGS`, enabling cross-language LTO so the C TLS shim is inlined directly
into the Rust FFI functions. Requires `clang` and `lld` (the toolchain's
bundled `rust-lld` is used automatically when available).

The script auto-detects the host triple. To cross-compile:

```bash
./build-optimized.sh --target aarch64-unknown-linux-gnu
```

Extra arguments are forwarded to `cargo build`.
