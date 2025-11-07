# libdd-crashtracker-ffi

C FFI bindings for the libdd-crashtracker library.

## Overview

`libdd-crashtracker-ffi` provides C-compatible Foreign Function Interface (FFI) bindings for `libdd-crashtracker`, allowing crash tracking in applications written in C, C++, PHP, Ruby, Python, and other languages.

## Features

- **C API**: Complete C bindings for crash tracking
- **Cross-platform**: Windows, Linux, macOS support
- **Signal-safe**: Safe to use in signal handlers
- **cbindgen Headers**: Auto-generated C headers
- **Static and Dynamic**: Available as both `.a` and `.so`/`.dylib`/`.dll`
- **Metadata**: Support for custom tags and metadata
- **Configuration**: Flexible crash tracking configuration

## API Functions

The C API provides functions for:
- Initialization and configuration
- Crash tracking setup
- Metadata attachment
- Profiling integration
- Error handling
- Cleanup

## Example Integration

### C/C++

```c
#include <datadog/crashtracker.h>

int main() {
    // Initialize crash tracker
    ddog_crashtracker_config_t config = {
        // ... configuration ...
    };
    
    ddog_crashtracker_init(&config);
    
    // Your application runs...
    // Crashes are automatically tracked
    
    ddog_crashtracker_shutdown();
    return 0;
}
```

### Build Integration

```bash
# Using pkg-config
gcc myapp.c $(pkg-config --cflags --libs datadog_crashtracker) -o myapp

# Or link directly
gcc myapp.c -ldatadog_crashtracker -o myapp
```

## Library Files

- **Header**: `include/datadog/crashtracker.h`
- **Static**: `libdatadog_crashtracker.a`
- **Dynamic**: `libdatadog_crashtracker.so` (Linux), `.dylib` (macOS), `.dll` (Windows)

## Platform-Specific Notes

### Linux
- Uses signal handlers for crash detection
- Requires `blazesym` for symbolication

### Windows
- Uses SEH (Structured Exception Handling)
- Includes Windows-specific exception handling

### macOS
- Signal-based crash detection
- Supports both x86_64 and aarch64

## Safety

The crash tracker is designed to be signal-safe and can safely operate in crash conditions when memory may be corrupted.

