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

## API Overview

### Creating an Exporter

#### Option 1: Agent-based Exporter (Most Common)

```cpp
auto exporter = ProfileExporter::create_agent(
    "dd-trace-cpp",           // library name
    "1.0.0",                  // library version
    "cpp",                    // language family
    {                         // tags
        "env:production",
        "service:my-service",
        "version:2.1.0"
    },
    "http://localhost:8126"   // agent URL
);
```

#### Option 2: Agentless (Direct to Intake)

```cpp
auto exporter = ProfileExporter::create_agentless(
    "dd-trace-cpp",           // library name
    "1.0.0",                  // library version
    "cpp",                    // language family
    {"env:staging"},          // tags
    "datadoghq.com",         // site
    "YOUR_API_KEY"           // Datadog API key
);
```

#### Option 3: File-based (Debugging)

```cpp
auto exporter = ProfileExporter::create_file(
    "dd-trace-cpp",           // library name
    "1.0.0",                  // library version
    "cpp",                    // language family
    {"env:development"},      // tags
    "/tmp/profile_dump.http"  // output file path
);
```

The file-based exporter captures the raw HTTP request that would be sent to Datadog, including all headers and multipart form data. Each request is saved with a timestamp suffix (e.g., `profile_dump_20231209_123456_789.http`).

#### Option 4: Custom Configuration

```cpp
ExporterConfig config;
config.profiling_library_name = "dd-trace-cpp";
config.profiling_library_version = "2.0.0";
config.family = "cpp";
config.tags = {"env:test", "region:us-east-1"};
config.endpoint_url = "http://localhost:8126";
config.api_key = "";           // optional
config.timeout_ms = 10000;     // 10 seconds

auto exporter = ProfileExporter::create(config);
```

### Sending Profiles

#### Basic Send

```cpp
// Get a profile (in real usage, this comes from your profiling code)
auto profile = create_test_profile();

// Prepare additional files (optional)
std::vector<ExporterFile> files;
files.push_back(ExporterFile{
    .name = "metadata.json",
    .bytes = {/* ... your data ... */}
});

// Prepare profile-specific tags (optional)
std::vector<std::string> additional_tags = {
    "profile_type:cpu",
    "duration_seconds:60"
};

// Send the profile (blocking call)
auto status_code = exporter->send_blocking(
    std::move(profile),
    files,
    additional_tags
);

std::cout << "Profile sent! HTTP status: " << status_code << std::endl;
```

#### Send with Cancellation Support

```cpp
// Create a cancellation token
auto cancel_token = CancellationToken::create();

// In another thread, you might cancel the operation:
std::thread([&cancel_token]() {
    std::this_thread::sleep_for(std::chrono::seconds(5));
    cancel_token->cancel();
}).detach();

// Send with cancellation support
try {
    auto status_code = exporter->send_blocking_with_cancel(
        std::move(profile),
        files,
        additional_tags,
        *cancel_token
    );
    std::cout << "Profile sent! HTTP status: " << status_code << std::endl;
} catch (const rust::Error& e) {
    if (cancel_token->is_cancelled()) {
        std::cout << "Operation was cancelled" << std::endl;
    }
}
```

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

## Example

See `examples/cxx/profiling_exporter.cpp` for a complete working example that demonstrates:

- Creating exporters with different endpoint types
- Sending test profiles
- Adding custom tags
- Attaching additional files
- Error handling

## Building the Example

```bash
# From the libdatadog root directory
cargo build -p libdd-profiling --features cxx
cd examples/cxx
# Build instructions TBD (requires CXX build setup)
```

## Tag Format

Tags follow the Datadog tagging convention:
- Format: `"key:value"` or just `"value"`
- Example: `"env:production"`, `"service:my-app"`, `"version:1.0.0"`
- Tags are validated and invalid tags will cause an error

## Error Handling

All factory methods and `send_blocking` return `rust::Result<T>` which can throw `rust::Error` exceptions:

```cpp
try {
    auto exporter = ProfileExporter::create_agent(/* ... */);
    auto status = exporter->send_blocking(/* ... */);
} catch (const rust::Error& e) {
    std::cerr << "Error: " << e.what() << std::endl;
}
```

## Threading Model

- **ProfileExporter** is not thread-safe. Create one instance per thread or use external synchronization.
- **send_blocking()** blocks the calling thread until the HTTP request completes.
- The method creates a temporary Tokio runtime internally for the async operation.

## Debugging with File Export

The file-based exporter is particularly useful for:
- Inspecting the exact HTTP request format
- Debugging multipart form encoding
- Verifying profile contents before sending to Datadog
- Integration testing without needing a running agent

Example output file structure:
```
POST /v1/input HTTP/1.1
connection: close
dd-evp-origin: dd-trace-cpp
dd-evp-origin-version: 1.0.0
content-type: multipart/form-data; boundary=...

--boundary
Content-Disposition: form-data; name="event"; filename="event.json"

{"attachments":["profile.pprof"],"tags_profiler":"..."}
--boundary
Content-Disposition: form-data; name="profile.pprof"; filename="profile.pprof"

[binary pprof data]
--boundary--
```

## Limitations

- Currently only supports the reqwest-based exporter (not the legacy hyper exporter)
- Requires Rust toolchain and CXX code generation
- C++11 or later required
- Unix platforms only for file:// debug export (uses Unix domain sockets)

## Implementation Details

- Uses the [CXX](https://cxx.rs/) library for safe Rust-C++ interop
- Zero-copy where possible (references to Rust data)
- Automatic memory management (no manual cleanup required)
- Exception-safe (uses RAII and C++ smart pointers)

## See Also

- [CXX Documentation](https://cxx.rs/)
- [Datadog Profiling Documentation](https://docs.datadoghq.com/profiler/)
- `libdd-crashtracker/src/crash_info/cxx.rs` - Similar CXX bindings pattern

