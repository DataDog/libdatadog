# RFC: logging in libdatadog

## Overview

**Document Purpose:**

This document outlines the design for adding logging sink capabilities to libdatadog. Currently, logs generated within libdatadog are not visible to the calling systems, making it difficult for Datadog to diagnose issues in customer environments. This document proposes implementing logging directly within libdatadog, with support for multiple output destinations including stdout and file-based output. These logs are intended solely for Datadog's internal troubleshooting purposes and are not meant to be used by customers for their application logging needs.

**Background**

The libdatadog `TraceExporter` is responsible for exporting traces to the Datadog agent. While processing and exporting traces, it generates important diagnostic logs which are currently not visible in production environments. This makes the `TraceExporter` integration backward incompatible with existing Trace SDKs.

Some Trace SDKs, like .NET, only allow file-based logging, while others like Python follow a bring-your-own-logging (BYOL) approach where the SDK does not provide any logging capabilities and relies on the application to handle logging. These logs are typically separated from the application logs to avoid confusion for the end users and to ensure that the logs are only used for Datadog's internal troubleshooting purposes.

This RFC proposes two common logging sinks for libdatadog while keeping the APIs flexible enough to allow future extensions. This design builds upon libdatadog's existing architecture, leveraging established error handling patterns and type definitions. It uses existing primitives like `CharSlice` for string handling and `Error` for error handling.

## Goals

* **Primary Goals:**
  * Support multiple output destinations:
    * No output (Noop) for when logging is not needed
    * Standard output (Stdout) for console logging
    * File-based output for persistent logging
  * Provide configurable log levels at runtime
* **Non-Goals:**
  * Automatic log collection (i.e., telemetry)

## Technical Design Summary

The logging system provides a simple and flexible public interface for configuring logging behavior in libdatadog. The interface consists of:

* Two primary configuration functions:
  * `ddog_logger_configure` - For setting up the logger with desired output destination and level
  * `ddog_logger_set_log_level` - For updating the minimum log level at runtime
  * These methods must be implemented in a thread-safe manner
* Three supported output destinations:
  * Noop - For disabling logging (since libdatadog allows disabling logging)
  * Stdout - For console output
  * File - For writing to a specified file
* Configuration structures that provide:
  * Log level selection
  * Output destination selection
  * File path configuration when using file output

The public API is designed to be simple to use while providing the necessary flexibility for different logging needs.

## Detailed Design

The integration exposes two primary functions through FFI for configuring logging behavior:

### Public APIs

```rust
/// Sets the global log level.
///
/// # Arguments
/// * `log_level` - The minimum level for events to be logged
///
/// # Errors
/// Returns an error if the log level cannot be set.
#[no_mangle]
pub extern "C" fn ddog_logger_set_log_level(
    log_level: LogEventLevel,
) -> Option<Box<Error>>;

/// Configures the global logger.
///
/// # Arguments
/// * `config` - Configuration for the logger including level and output destination
///
/// # Errors
/// Returns an error if the logger cannot be configured.
#[no_mangle]
pub extern "C" fn ddog_logger_configure(config: LoggerConfig) -> Option<Box<Error>>;

/// Configuration for the logger.
#[repr(C)]
pub struct LoggerConfig<'a> {
    /// Minimum level for events to be logged
    pub level: LogEventLevel,
    /// Output configuration
    pub writer: WriterConfig<'a>,
}

/// Configuration for log output destination.
#[repr(C)]
pub struct WriterConfig<'a> {
    /// Type of output destination
    pub kind: WriterKind,
    /// File configuration, required when `kind` is WriterKind::File
    pub file: Option<&'a FileConfig<'a>>,
}

/// Type of log output destination.
#[repr(C)]
pub enum WriterKind {
    /// Discard all output
    Noop,
    /// Write to standard output
    Stdout,
    /// Write to file
    File,
}

/// Configuration for file output.
#[repr(C)]
pub struct FileConfig<'a> {
    /// Path to the log file
    pub path: CharSlice<'a>,
}
```

### Example Usage

Check the [example usage](../../examples/ffi/trace_exporter.c) in the `trace_exporter.c` file.

### Performance and Scalability

The logging implementation follows established patterns for logging in other APM libraries as outlined in the [tracer logging RFC](https://github.com/DataDog/architecture/blob/891eda680d70b9825fec58dc90553c5d4557058a/rfcs/apm/integrations/tracer-logging/rfc.md).

It also uses structured logging to make it easier to parse and analyze logs.

## Alternatives Considered

### Callback-based API

An alternative design considered exposing a callback-based API where users would provide their own logging function. This was rejected because:

1. It would make the API more complex, requiring native code to call into the managed code for final logging
2. This is particularly complex for languages like Python where the Global Interpreter Lock (GIL) must be held to call into the managed code
3. Error handling across FFI boundaries would be more complicated
4. Performance overhead of crossing FFI boundaries for each log message

### Environment Variable Configuration

Another alternative considered was using environment variables for logger configuration. This was rejected because:

1. It would make runtime reconfiguration more difficult
2. Environment variables are global state and could lead to conflicts
3. Some deployment environments restrict environment variable access
4. More difficult to validate configuration at runtime

## Appendix 1

References:

* [Rust log crate documentation](https://docs.rs/log/0.4.26/log/fn.set_logger.html)
* POC Implementation: [libdatadog PR #XXX](https://github.com/DataDog/libdatadog/compare/main...ganeshnj/poc/logging)