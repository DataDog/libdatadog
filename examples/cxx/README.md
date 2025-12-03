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

### Key Functions

**Builder Creation:**
- `crashinfo_builder_new()` - Create a new builder
- `stackframe_new()` - Create a new stack frame
- `stacktrace_new()` - Create a new stack trace

**Builder Methods:**
- `crashinfo_with_kind()` - Set error type
- `crashinfo_with_message()` - Set error message
- `crashinfo_with_metadata()` - Set library metadata
- `crashinfo_with_proc_info()` - Set process info
- `crashinfo_with_os_info()` - Set OS info
- `crashinfo_with_counter()` - Add a named counter
- `crashinfo_with_file()` - Add a file to the report
- `crashinfo_with_stack()` - Set the stack trace
- `crashinfo_with_timestamp_now()` - Set current timestamp
- `crashinfo_build()` - Build the final CrashInfo

**StackFrame Methods:**
- `stackframe_with_function()`, `stackframe_with_file()`, `stackframe_with_line()`, `stackframe_with_column()` - Set debug info
- `stackframe_with_ip()`, `stackframe_with_sp()` - Set absolute addresses
- `stackframe_with_build_id()`, `stackframe_with_path()` - Set binary info
- `stackframe_with_relative_address()` - Set relative address

**StackTrace Methods:**
- `stacktrace_push_frame()` - Add a frame to the trace
- `stacktrace_set_complete()` - Mark trace as complete

**Output:**
- `crashinfo_to_json()` - Convert CrashInfo to JSON string

## Building and Running

The `build-and-run.sh` script handles the entire build process:

```bash
./examples/cxx/build-and-run.sh
```

This will:
1. Build libdd-crashtracker with the `cxx` feature enabled
2. Find the CXX bridge headers and libraries
3. Compile the C++ example
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

- C++14 or later
- Rust toolchain
- macOS (this example) or Linux
