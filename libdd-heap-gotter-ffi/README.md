# libdd-heap-gotter-ffi

C FFI bindings for `libdd-heap-gotter`.

## Overview

`libdd-heap-gotter-ffi` exposes a small C ABI for installing heap-profiling GOT table interposition from language runtimes such as Python and Ruby.

The API installs hooks for supported allocator symbols and updates them after new libraries are loaded.

## API

- `ddog_heap_gotter_install()` - install heap GOT overrides in the current process.
- `ddog_heap_gotter_update()` - re-scan loaded libraries and patch newly introduced GOT entries.
- `ddog_heap_gotter_is_installed()` - return whether overrides are currently installed.

Installation is permanent - there is deliberately no uninstall to avoid problems associated with tagged memory!

## Important lifetime note

The shared library containing these hooks must remain loaded for the life of the process. Patched GOT entries point at functions in this library, so unloading it would leave dangling function pointers and crash the process.

TODO: consider hooking `dlclose` too, so we can at least partially protect against unloading this library while its hooks are still installed.

## Building

This crate follows the standard libdatadog FFI layout and can produce `staticlib` and `cdylib` artifacts.

```bash
cargo build -p libdd-heap-gotter-ffi
```

The C header is generated with cbindgen by the libdatadog release tooling, not by ordinary `cargo build`.

## Dynamic-loading demo

The `cdylib_demo` example loads the generated shared library with `dlopen`, resolves the C ABI symbols with `dlsym`, installs the GOT hooks, and produces allocation pressure.

```bash
cargo run -p libdd-heap-gotter-ffi --example cdylib_demo
```

If the cdylib is somewhere else, set:

```bash
DDOG_HEAP_GOTTER_FFI_CDYLIB=/path/to/liblibdd_heap_gotter_ffi.so \
  cargo run -p libdd-heap-gotter-ffi --example cdylib_demo
```
