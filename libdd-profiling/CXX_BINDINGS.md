# CXX Bindings for libdd-profiling Exporter

This document describes the C++ (CXX) bindings for the libdd-profiling reqwest-based exporter, providing a safer and more idiomatic C++ API compared to traditional C FFI.

## Features

The CXX bindings provide safe C++ access to:

- **ProfileExporter** - Modern async-based HTTP exporter
- **EncodedProfile** - Profiling data ready for export  
- **Multiple Endpoint Types**:
  - Agent-based (standard Datadog agent)
  - Agentless (direct to Datadog intake)
  - File-based (for debugging/testing)
- **Flexible Configuration** - Via struct or convenience constructors
- **Tags Support** - Both static and profile-specific tags
- **File Attachments** - Send additional files alongside profiles

## Building with CXX Support

Add the `cxx` feature when building:

```bash
cargo build -p libdd-profiling --features cxx
```

## Usage Example

See [`examples/cxx/profiling_exporter.cpp`](../../examples/cxx/profiling_exporter.cpp) for a complete working example demonstrating:

- Creating exporters with different endpoint types (agent, agentless, file-based)
- Sending profiles with `send_blocking()`
- Adding additional files and tags
- Using cancellation tokens with `send_blocking_with_cancel()`
- C++20 designated initializers for clean configuration

To build and run the example:
```bash
cd examples/cxx
./build-and-run-profiling.sh    # Unix
./build-and-run-profiling.ps1   # Windows
```

## Quick Reference

### Creating Exporters

The library provides four ways to create an exporter:

1. **`ProfileExporter::create_agent(...)`** - Connect to local Datadog agent (most common)
2. **`ProfileExporter::create_agentless(...)`** - Direct to Datadog intake with API key
3. **`ProfileExporter::create_file(...)`** - Save HTTP request to file for debugging
4. **`ProfileExporter::create(...)`** - Custom configuration via `ExporterConfig` struct

### Sending Profiles

- **`send_blocking(profile, files, tags)`** - Send profile and wait for result
- **`send_blocking_with_cancel(profile, files, tags, token)`** - Send with cancellation support

### Creating Test Data

- **`EncodedProfile::create_test_profile()`** - Generate a test profile for examples/testing

## Types

### ExporterConfig

Configuration struct for creating an exporter:

```cpp
struct ExporterConfig {
    std::string profiling_library_name;     // e.g., "dd-trace-cpp"
    std::string profiling_library_version;  // e.g., "1.0.0"
    std::string family;                     // e.g., "cpp", "rust", "python"
    std::vector<std::string> tags;          // format: "key:value"
    std::string endpoint_url;               // HTTP(S) URL or file:// path
    std::string api_key;                    // Datadog API key (for agentless)
    uint64_t timeout_ms;                    // Request timeout in milliseconds
};
```

### ExporterFile

Represents an additional file to attach to the profile:

```cpp
struct ExporterFile {
    std::string name;           // Filename
    std::vector<uint8_t> bytes; // File contents
};
```

## Tag Format

Tags follow the Datadog tagging convention:
- Format: `"key:value"` or just `"value"`
- Example: `"env:production"`, `"service:my-app"`, `"version:1.0.0"`
- Tags are validated and invalid tags will cause an error

## Error Handling

All factory methods and `send_blocking` methods return `rust::Result<T>`. On error, they throw `rust::Error` exceptions. See the example code for error handling patterns.

## Threading Model

- **ProfileExporter** is not thread-safe. Create one instance per thread or use external synchronization.
- **send_blocking()** blocks the calling thread until the HTTP request completes.
- The method creates a temporary Tokio runtime internally for the async operation.

## Debugging with File Export

The file-based exporter (`create_file()`) captures the raw HTTP request to a file, useful for:
- Inspecting exact request format and headers
- Debugging multipart form encoding
- Verifying profile contents
- Testing without a running agent

Files are saved with a timestamp suffix (e.g., `profile_dump_20231209_123456_789.http`) and contain the complete HTTP request including all headers and multipart form data.

## Requirements & Limitations

- **C++20** or later (for designated initializers; C++11+ works with explicit member initialization)
- **Rust toolchain** and CXX code generation
- Currently only supports the reqwest-based exporter (not the legacy hyper exporter)
- Unix platforms only for `file://` debug export (uses Unix domain sockets)

## Implementation Details

- Uses the [CXX](https://cxx.rs/) library for safe Rust-C++ interop
- Zero-copy where possible (references to Rust data)
- Automatic memory management (no manual cleanup required)
- Exception-safe (uses RAII and C++ smart pointers)

## See Also

- [CXX Documentation](https://cxx.rs/)
- [Datadog Profiling Documentation](https://docs.datadoghq.com/profiler/)
- `libdd-crashtracker/src/crash_info/cxx.rs` - Similar CXX bindings pattern

