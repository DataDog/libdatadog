# CXX Bindings Examples

This directory contains C++ examples demonstrating the CXX bindings for libdatadog components.

CXX bindings provide a safer and more idiomatic C++ API compared to the traditional C FFI bindings, with automatic memory management and exception handling.

## Examples

### Crashtracker (`crashinfo.cpp`)

Demonstrates building crash reports using the CXX bindings for `libdd-crashtracker`.

**Build and run:**

Unix (Linux/macOS):
```bash
./build-and-run-crashinfo.sh
```

Windows:
```powershell
.\build-and-run-crashinfo.ps1
```

**Key features:**
- Type-safe crash report builder API
- Support for stack traces, frames, and metadata
- Process and OS information
- Automatic memory management
- Exception-based error handling

**Core Types:**
- `CrashInfoBuilder` - Builder for constructing crash information
- `StackFrame` - Individual stack frame with debug info and addresses
- `StackTrace` - Collection of stack frames
- `CrashInfo` - Final crash information object
- `Metadata` - Library metadata (name, version, tags)
- `ProcInfo` - Process information
- `OsInfo` - Operating system information

**Enums:**
- `CxxErrorKind` - Type of error (Panic, UnhandledException, UnixSignal)
- `CxxBuildIdType` - Build ID format (GNU, GO, PDB, SHA1)
- `CxxFileType` - Binary file format (APK, ELF, PE)

### Profiling (`profiling.cpp`)

Demonstrates building profiling data and exporting to Datadog using the CXX bindings for `libdd-profiling`.

**Build and run:**

Unix (Linux/macOS):
```bash
./build-profiling.sh
```

Windows:
```powershell
.\build-profiling.ps1
```

**Key features:**
- Type-safe API for building profiles
- Support for samples, locations, mappings, and labels
- String interning for efficient memory usage
- Upscaling rules (Poisson and Proportional)
- Endpoint tracking for web service profiling
- Pprof format serialization with zstd compression
- **Export to Datadog** via agent or agentless mode
- Support for attaching additional compressed files
- Per-profile tags and metadata
- Automatic memory management
- Exception-based error handling
- Modern C++20 syntax with designated initializers and `std::format`

**Core Types:**
- `Profile` - Profile builder for collecting samples
- `ProfileExporter` - Exporter for sending profiles to Datadog
- `Tag` - Key-value tags for profile metadata
- `AttachmentFile` - Additional file to attach to profile (name + data bytes)

**Export Modes:**

By default, the example saves the profile to `profile.pprof`. To export to Datadog, set environment variables:

1. **Agent mode**: Sends profiles to the local Datadog agent
   ```bash
   DD_AGENT_URL=http://localhost:8126 ./build-profiling.sh
   ```

2. **Agentless mode**: Sends profiles directly to Datadog intake
   ```bash
   DD_API_KEY=your-api-key DD_SITE=datadoghq.com ./build-profiling.sh
   ```

**API Example:**

See [`profiling.cpp`](profiling.cpp) for a complete example showing profile creation, sample collection, and exporting to Datadog with optional attachments and metadata.

### Profiling api2 (`profiling_api2.cpp`)

Demonstrates building profiling data with the dictionary-backed api2 CXX API.
This is closer to the low-overhead profiler hot path used by language tracers:
strings, functions, and mappings are inserted into a `ProfilesDictionary`, and
samples carry opaque ids instead of string payloads for locations and label keys.

**Build and run:**

Unix (Linux/macOS):
```bash
./build-profiling-api2.sh
```

Windows:
```powershell
.\build-profiling-api2.ps1
```

**Key features:**
- `ProfilesDictionary` creation and dictionary string insertion
- Dictionary-backed `Mapping2`, `Function2`, `Location2`, `Label2`, and `Sample2`
- `Profile::create_with_dictionary`
- `Profile::add_sample2`
- Timestamped samples via nonzero `endtime_ns`
- Non-timestamped samples via `endtime_ns == 0`
- Pprof serialization to `profile_api2.pprof`

See [`profiling_api2.cpp`](profiling_api2.cpp) for a focused dictionary-backed api2 sample creation and serialization example.

**Requirements:**
- C++20 compiler
- For agent mode: Datadog agent running (default: localhost:8126)
- For agentless mode: Valid Datadog API key

## Build Scripts

The examples use a consolidated build system:

- **Unix (Linux/macOS)**: `build-and-run.sh <crate-name> <example-name>`
- **Windows**: `build-and-run.ps1 -CrateName <crate-name> -ExampleName <example-name>`

Convenience wrappers are provided for each example:
- `build-and-run-crashinfo.sh` / `build-and-run-crashinfo.ps1`
- `build-profiling.sh` / `build-profiling.ps1`
- `build-profiling-api2.sh` / `build-profiling-api2.ps1`

## Requirements

- C++20 or later
- Rust toolchain
- C++ compiler (clang++ or g++)
- Platform: macOS, Linux, or Windows
  - Windows: Requires MSVC (via Visual Studio) or MinGW/LLVM
