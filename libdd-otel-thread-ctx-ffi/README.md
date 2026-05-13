# libdd-otel-thread-ctx-ffi

FFI bindings for the OTel thread-level context publisher. Exposes a C API for
attaching, detaching, and updating per-thread OpenTelemetry context records
that external readers (e.g. the eBPF profiler) can discover.

Currently Linux-only (x86-64 and aarch64).

## Optimized build (cross-language inlining)

The OTel thread-level conext sharing specification requires the use of the
TLSDESC dialect for the thread-local variable that holds the current context.
Because (stable) `rustc` doesn't currently provide a way to control the TLS
dialect, we need to use a small C shim that defines the variable and expose a
one-line getter. This unfortunately adds one level of indirection (a function
call) when attaching or detaching a context.

With the right toolchain, it's possible to use Link-Time Optimization (LTO) to
inline the C wrapper at link time. The requirements are:

- `clang` is available to compile the C shim to LLVM IR (version requirements
  aren't clear -- tested with clang18 and clang20, but ideally the version
  should be the same or close to the LLVM version shipped with `rustc`)
- Either the Rust toolchain ships `lld` or there's a system-wide `lld` install
  (Rust ships `rust-lld` for a long time, something like since 1.53+, however
  some musl-based distro like Alpine might have the Rust toolchain without LLD)
- `lld` version is at least 19 (TLSDESC support)

If those requirements are met, you can use the small wrapper script provided in
this directory to build an optimized release version where the C shim is
inlined. A wrapper script is needed because cross-language LTO requires two
`rustc` codegen flags (`-Clinker-plugin-lto` and `-Clinker=clang`) that cannot
be set from a Cargo build script: they must come from `RUSTFLAGS` or
`.cargo/config.toml`. The script sets them via the target-scoped
`CARGO_TARGET_<TRIPLE>_RUSTFLAGS` env var so they don't leak to build scripts
or proc-macros.

```bash
./build-optimized.sh
```

The script auto-detects the host triple. To cross-compile:

```bash
./build-optimized.sh --target aarch64-unknown-linux-gnu
```

Extra arguments are forwarded to `cargo build`.
