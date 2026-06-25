# libdd-heap-gotter-ffi

C FFI bindings for `libdd-heap-gotter`.

## Overview

`libdd-heap-gotter-ffi` exposes a small C ABI for installing heap-profiling GOT table interposition from language runtimes such as Python and Ruby.

The API installs hooks for supported allocator symbols, updates hooks after new libraries are loaded, and restores patched GOT entries when profiling is disabled.

## API

- `ddog_heap_gotter_install()` - install heap GOT overrides in the current process.
- `ddog_heap_gotter_update()` - re-scan loaded libraries and patch newly introduced GOT entries.
- `ddog_heap_gotter_restore()` - restore every GOT entry patched by install/update.
- `ddog_heap_gotter_is_installed()` - return whether overrides are currently installed.

## Important lifetime note

The shared library containing these hooks must remain loaded while overrides are installed. Patched GOT entries point at functions in this library, so unloading it before calling `ddog_heap_gotter_restore()` can leave dangling function pointers and crash the process.

## Building

This crate follows the standard libdatadog FFI layout: it builds `staticlib` and `cdylib` artifacts and generates a C header with cbindgen.

```bash
cargo check -p libdd-heap-gotter-ffi
```

## Dynamic-loading demo

The `cdylib_demo` example loads the generated shared library with `dlopen`, resolves the C ABI symbols with `dlsym`, installs the GOT hooks, and produces allocation pressure.

```bash
cargo build -p libdd-heap-gotter-ffi
cargo run -p libdd-heap-gotter-ffi --example cdylib_demo
```

If the cdylib is somewhere else, set:

```bash
DDOG_HEAP_GOTTER_FFI_CDYLIB=/path/to/liblibdd_heap_gotter_ffi.so \
  cargo run -p libdd-heap-gotter-ffi --example cdylib_demo
```
