# libdd-profiling-ffi

C FFI bindings for the libdd-profiling library.

# Datadog Profiling FFI Notes

## \#[must_use] on functions

Many FFI functions should use `#[must_use]`. As an example, there are many
Result types which need to be used for correctness reasons:

```rust
#[repr(C)]
pub enum ProfileAddResult {
    Ok(u64),
    Err(Error),
}
```

Then on `ddog_prof_Profile_add` which returns a `ProfileAddResult`, there is a
`#[must_use]`. If the C user of this API doesn't touch the return value, then
they'll get a warning, something like:

> warning: ignoring return value of function declared with
> 'warn_unused_result' attribute [-Wunused-result]

Additionally, many types (including Error types) have memory that needs to
be dropped. If the user doesn't use the result, then they definitely leak.

It would be nice if we could put `#[must_use]` directly on the type, rather
than on the functions which return them. At the moment, cbindgen doesn't
handle this case, so we have to put `#[must_use]` on functions.

## Overview

`libdd-profiling-ffi` provides C-compatible Foreign Function Interface (FFI) bindings for `libdd-profiling`, allowing integration with applications written in C, C++, PHP, Ruby, Python, and other languages that can call C libraries.

## Features

- **C API**: Complete C bindings for profile creation and management
- **Static and Dynamic Libraries**: Available as both `.a` and `.so`/`.dylib`/`.dll`
- **cbindgen Headers**: Auto-generated C headers for type safety
- **Memory Management**: Safe memory management across FFI boundaries
- **Error Handling**: C-compatible error handling
- **pkg-config**: Integration with build systems via pkg-config

## Library Types

The crate can be built as:
- `staticlib`: Static library (`.a`)
- `cdylib`: Dynamic library (`.so`, `.dylib`, `.dll`)
- `lib`: Rust library for linking

## Generated Headers

The C header file is automatically generated using cbindgen and provides:
- Profile creation and management functions
- Sample collection APIs
- Export functionality
- Type definitions and enums

## Build Artifacts

- **Headers**: `include/datadog/profiling.h`
- **Static lib**: `libdatadog_profiling.a`
- **Dynamic lib**: `libdatadog_profiling.so` (Linux), `libdatadog_profiling.dylib` (macOS), `datadog_profiling.dll` (Windows)
- **pkg-config**: `.pc` files for easy integration

## Example Integration

### Using pkg-config

```bash
gcc myapp.c $(pkg-config --cflags --libs datadog_profiling) -o myapp
```

### Manual Linking

```c
#include <datadog/profiling.h>

int main() {
    // Create profile
    // Collect samples
    // Export to Datadog
    return 0;
}
```
