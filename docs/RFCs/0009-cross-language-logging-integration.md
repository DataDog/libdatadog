# RFC: Cross-Language Logging Integration for libdatadog

Status: Approved (.NET)

## Overview

**Document Purpose:**

This document outlines the design of exposing libdatadog’s internal logs to integrating languages. Currently, logs generated within datadog are not visible to the calling systems, making it difficult to diagnose issues. Taking inspiration from PHP sidecar’s implementation, this document proposes a mechanism to centralize all the logs (both from libdatadog and tracer) in one place, providing a unified view of the system’s behavior.

**Background**

The `TraceExporter` component in libdatadog handles sending traces to the agent. This component generates important diagnostic logs during trace processing and export operations, but these logs are currently invisible in production environments as libdatadog lacks a logging sink.

The .NET tracer has an established logging system that writes to dedicated log files, maintaining a clean separation between application and tracer logs. However, we currently cannot see libdatadog’s internal logs, making it difficult to diagnose issues. Also, not logging in `TraceExporter` is a behavior breaking change.

Reading and correlating logs becomes challenging when tracer logs are scattered across different files, especially as components execute in [mixed mode](https://learn.microsoft.com/en-us/visualstudio/debugger/how-to-debug-in-mixed-mode?view=vs-2022). This proposal aims to create a unified logging approach that makes log analysis intuitive by centralizing logs from both libdatadog and the tracer, while preserving the existing clean separation from application logs.

**Context**

This design builds upon libdatadog’s existing architecture, leveraging established error handling patterns and type definitions. It uses existing primitives like `CharSlice` for string handling and same error code based error handling. This ensures consistency with the rest of the codebase and minimizes the introduction of new patterns or types.

## Goals

* **Primary Goals**:
  * Implement a unified logging interface between libdatadog and language-specific logging libraries
  * Support concurrent logging from multiple threads
  * Maintain memory safety across FFI boundaries
* **Non-Goals:**
  * Automatic log collection (i.e. telemetry)
  * Custom log formatting and processing within libdatadog

## Technical Design Summary

The logging integration implements a bridge between libdatadog and language-specific logging libraries using Foreign Function Interface (FFI). Key components include:

* A callback-based architecture for log forwarding
* Thread-safe logging mechanisms
* Configurable log levels

## Detailed Design

The integration leverages Rust's log crate capabilities through FFI, exposing three primary functions:

* Log callback registration
* Log level configuration

### APIs

```c
/// Define field key-value structure
#[repr(C)]
pub struct LogField<'a> {
    /// Field key (e.g., "error_code")
    pub key: CharSlice<'a>,
    /// Field value (e.g., "404")
    pub value: CharSlice<'a>,
}

/// Log event structure containing level, message, and fields
#[repr(C)]
pub struct LogEvent<'a> {
    /// Log level of the event
    pub level: LogLevel,
    /// Log message without formatting
    pub message: CharSlice<'a>,
    /// Additional fields for structured logging
    pub fields: ddcommon_ffi::Vec<LogField<'a>>,
}

/// Log level enumeration
#[repr(C)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

/// Thread-safe callback function for log message handling
/// @param event: Log event containing level, message, and fields
/// Note: This callback will be invoked from multiple threads concurrently
type LogCallback = extern "C" fn(event: LogEvent);

/// Thread-safe function to initialize the logger with a specified log level and callback
/// Can be safely called from any thread, but should only be called once
/// Calling this function multiple times will result in an error
/// @param log_level: Initial maximum log level
/// @param callback: Thread-safe function to handle log events
/// @returns: None if successful, Error if initialization fails
/// Note: If an error is returned, it must be released using `ddog_Error_drop`.
#[no_mangle]
pub extern "C" fn ddog_log_logger_init(log_level: LogLevel, callback: LogCallback) -> Option<Box<Error>> {
    // Implementation details
}

/// Thread-safe function to set the maximum log level
/// Can be safely called from any thread after logger initialization
/// @param log_level: Maximum log level to record
/// @returns: None if successful, Error if setting log level fails
/// Note: If an error is returned, it must be released using `ddog_Error_drop`.
#[no_mangle]
pub extern "C" fn ddog_log_logger_set_log_level(log_level: LogLevel) -> Option<Box<Error>> {
    // Implementation details
}
```

If there is an unsupported log level, languages can decide to map it to the closest supported log level.

The language specific logging library can directly log the `message` without any additional processing. The `fields` can be added as additional context to the log message for structured logging. This separation allows `libdatadog` to have access to telemetry and error tracking features without additional work (if available on the Tracer side). On the Rust side, we make sure that `message` only contains static strings to avoid including any PII in the logs.

eg. using `tracing` crate macros [https://docs.rs/tracing/latest/tracing/\#shorthand-macros](https://docs.rs/tracing/latest/tracing/#shorthand-macros)

```c
error!(status_code = 404, "Resource not found");
```

On the integration side, languages can build their languages specific templates

```c
logger.Error("Resource not found {status_code}, status_code)
```

When components like `TraceExporter` are initialized and used as managed code, any logs produced by the native layer are handled consistently with other tracer logs. This unified logging approach simplifies debugging by providing a single, coherent view of the system's behavior.

A reference implementation in C\# is provided at [https://github.com/DataDog/libdatadog/pull/947](https://github.com/DataDog/libdatadog/pull/947)

### Data Flow

```c
+------------+              +------------+
| C#/lang    |              | Rust (FFI) |
+------------+              +------------+
      |                              |
      | Initialize Logger            |
      | & Setup Callback             |
      |----------------------------->|
      |                              |
      | (Optional) Set Log Level     |
      |----------------------------->|
      |                              |
      |                              | Rust Code Runs
      |                              | & Produce a Log
      |                              |
      |                              |
      | Logger Calls                 |
      | Callback (Setup              |
      | During Init)                 |
      |<-----------------------------|
      |                              |
      | Handle Log in C#             |
      | (e.g., Write to File)        |
      |                              |
      |----------------------------->|
      |                              |
```

When components like `TraceExporter` are initialized and used as managed code, any logs produced by the native layer are handled consistently with other tracer logs. This unified logging approach simplifies debugging by providing a single, coherent view of the system's behavior.

## Performance and Scalability

Cross language communication can be expensive, especially in this case if we are generating tens of thousands of log messages per seconds. Given the number of logs currently generated, this problem doesn’t exist in libdatadog. However, it is important to keep the number of log messages to minimum to avoid any performance issues.

Languages can modify their logging implementations or sinks without impacting the core libdatadog design. The FFI interface is extensible to support future features while maintaining backward compatibility.

## Alternatives Considered

### Directly log to a file

```c
/// Initializes file logger
/// @param path: Null-terminated UTF-8 string for log file path
/// @returns: 0 on success, error code otherwise
#[no_mangle]
pub extern "C" fn logger_init(path: *const c_char) -> c_int {
    // Implementation details
}

/// Sets maximum log level for file logging
/// @param level: Max level (1=ERROR to 5=TRACE)
#[no_mangle]
pub extern "C" fn logger_set_max_level(level: c_int) {
    // Implementation details
}
```

Logging directly to a file makes the integration simpler but doesn't provide the flexibility to integrate with language specific logging libraries. Since, logging ecosystems are different in different languages, it is important to provide a seamless integration with the language specific logging libraries.

Additionally, we have to implement any additional logic and processing today done by the tracer which can impact our timeline.

### Buffering log messages in the native library and forwarding them to the language specific logging library in batches.

This is an improvement over the proposed design as it reduces the number of cross language calls, but it adds another layer of complexity managing the buffer and batching the log messages. It also increases the probability of losing log messages in case of a crash.

### Using formatted strings for logging instead of structured logging

Formatted strings can contain PII and other sensitive information which breaks the telemetry and error tracking features. Additionally, it is impossible to capture the template and arguments separately when using standard macros for logging.

### Using `log` crate to log messages in Rust and then forwarding them to the language specific logging library.

This approach is similar to the proposed design but \`log\` is designed around plain text messages with levels and doesn't support structured logging directly.

## Appendix

References

* [https://docs.rs/log/0.4.26/log/fn.set\_logger.html](https://docs.rs/log/0.4.26/log/fn.set_logger.html)
* POC: [https://github.com/DataDog/libdatadog/compare/main...ganeshnj/poc/logging](https://github.com/DataDog/libdatadog/compare/main...ganeshnj/poc/logging)

