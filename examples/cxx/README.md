# CXX Bindings Example for libdd-crashtracker

This example demonstrates how to use the CXX bindings for the libdd-crashtracker crate, providing a safer and more idiomatic C++ API compared to the traditional C FFI.

## Features

The CXX bindings provide access to:

### Core Types
- `CrashInfoBuilder` - Builder for constructing crash information
- `StackFrame` - Individual stack frame with debug info and addresses
- `StackTrace` - Collection of stack frames
- `CrashInfo` - Final crash information object
- `Metadata` - Library metadata (name, version, tags)
- `ProcInfo` - Process information
- `OsInfo` - Operating system information

### Enums
- `ErrorKind` - Type of error (Panic, UnhandledException, UnixSignal)
- `BuildIdType` - Build ID format (GNU, GO, PDB, SHA1)
- `FileType` - Binary file format (APK, ELF, PE)

### Key API

**Object Creation:**
```cpp
auto builder = CrashInfoBuilder::create();
auto frame = StackFrame::create();
auto stacktrace = StackTrace::create();
```

**CrashInfoBuilder Methods:**
- `set_kind(CxxErrorKind)` - Set error type (Panic, UnhandledException, UnixSignal)
- `with_message(String)` - Set error message
- `with_counter(String, i64)` - Add a named counter
- `with_log_message(String, bool)` - Add a log message
- `with_fingerprint(String)` - Set crash fingerprint
- `with_incomplete(bool)` - Mark as incomplete
- `set_metadata(Metadata)` - Set library metadata
- `set_proc_info(ProcInfo)` - Set process information
- `set_os_info(OsInfo)` - Set OS information
- `add_stack(Box<StackTrace>)` - Add a stack trace
- `with_timestamp_now()` - Set current timestamp
- `with_file(String)` - Add a file to the report

**StackFrame Methods:**
- `with_function(String)`, `with_file(String)`, `with_line(u32)`, `with_column(u32)` - Set debug info
- `with_ip(usize)`, `with_sp(usize)` - Set instruction/stack pointers
- `with_module_base_address(usize)`, `with_symbol_address(usize)` - Set base addresses
- `with_build_id(String)` - Set build ID
- `build_id_type(CxxBuildIdType)` - Set build ID format (GNU, GO, PDB, SHA1)
- `file_type(CxxFileType)` - Set binary format (APK, ELF, PE)
- `with_path(String)` - Set module path
- `with_relative_address(usize)` - Set relative address

**StackTrace Methods:**
- `add_frame(Box<StackFrame>, bool)` - Add a frame (bool = incomplete)
- `mark_complete()` - Mark trace as complete

**Building & Output:**
```cpp
auto crash_info = crashinfo_build(std::move(builder));
auto json = crash_info->to_json();
```

## Building and Running

### Unix (Linux/macOS)

The `build-and-run-crashinfo.sh` script handles the entire build process:

```bash
./examples/cxx/build-and-run-crashinfo.sh
```

### Windows

The `build-and-run-crashinfo.ps1` PowerShell script handles the build process on Windows:

```powershell
.\examples\cxx\build-and-run-crashinfo.ps1
```

**Prerequisites for Windows:**
- Either MSVC (via Visual Studio) or MinGW/LLVM with C++ compiler
- PowerShell 5.0 or later (comes with Windows 10+)
- Rust toolchain

The build script will:
1. Build libdd-crashtracker with the `cxx` feature enabled
2. Find the CXX bridge headers and libraries
3. Compile the C++ example (automatically detects MSVC or MinGW/Clang)
4. Run the example and display the output

## Example Output

The example creates a crash report with:
- Error kind and message
- Library metadata with tags
- Process and OS information
- A stack trace with multiple frames (debug info + binary addresses)
- Counters and log messages
- Timestamp

The output is a JSON object that can be sent to Datadog's crash tracking service.

## Notes

- The CXX bindings use `rust::String` types which need to be converted to `std::string` for use with standard C++ streams
- All functions that can fail will use exceptions (standard C++ exception handling)
- The bindings are type-safe and prevent many common C FFI errors
- Memory is managed automatically through RAII and smart pointers

## Comparison to C FFI

The CXX bindings provide several advantages over the traditional C FFI:

1. **Type Safety**: No void pointers, proper type checking at compile time
2. **Memory Safety**: Automatic memory management through smart pointers
3. **Ergonomics**: More natural C++ idioms, no need for manual handle management
4. **Error Handling**: Exceptions instead of error codes
5. **String Handling**: Seamless `rust::String` ↔ C++ string interop

## Requirements

- C++20 or later
- Rust toolchain
- Platform: macOS, Linux, or Windows
  - Windows: Requires MSVC (via Visual Studio) or MinGW/LLVM
