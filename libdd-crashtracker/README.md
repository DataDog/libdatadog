# libdd-crashtracker

Crash detection and reporting library for Datadog APM.

## Overview

`libdd-crashtracker` detects program crashes and generates detailed crash reports with stack traces, metadata, and system information, then sends them to the Datadog backend.

## Features

- **Crash Detection**: Catches segfaults, uncaught exceptions, and abnormal terminations
- **Stack Traces**: Captures stack traces with symbol resolution
- **Signal Handling**: Handles Unix signals (SIGSEGV, SIGABRT, SIGBUS, etc.)
- **Windows Support**: SEH (Structured Exception Handling) on Windows
- **Metadata Collection**: Gathers crash context, environment, and tags
- **Symbolication**: Symbol resolution using blazesym
- **Telemetry Integration**: Reports crashes via Datadog telemetry
- **Receiver Binary**: Standalone crash receiver process

## Architecture

The crashtracker uses a two-process architecture:
1. **Collector** (in-process): Detects crashes and collects information
2. **Receiver** (separate process): Processes crash data and sends reports

This ensures crash reports are sent even if the main process is corrupted.

## Features Flags

- `collector` (default): Enable in-process crash collection
- `receiver` (default): Enable crash receiver functionality  
- `collector_windows` (default): Windows crash collection
- `benchmarking`: Enable benchmark functionality

## Example Usage

```rust
use libdd_crashtracker;

// Initialize crash tracker
// let config = CrashTrackerConfig::new(...);
// crashtracker::init(config)?;

// Your application runs...
// Crashes are automatically detected and reported
```

## Platform Support

- **Linux**: Signal-based crash detection with blazesym symbolication
- **macOS**: Signal-based crash detection
- **Windows**: SEH-based crash detection

## Receiver Binary

The crate includes a `crashtracker-receiver` binary that runs as a separate process to ensure crash reports are sent even when the main process crashes.

## Independently supervised receiver

On Unix, the default collector starts a receiver and waits for it to finish, including report delivery, up to the collector timeout.
A separate process does not by itself let the receiver survive workload shutdown.

To let the collector exit before delivery completes, run the receiver outside the workload shutdown boundary.
Serve each connection with `async_receiver_entry_point_unix_listener`.
Configure the collector with the same socket path using `CrashtrackerConfigurationBuilder::unix_socket_path`.
Use a socket that only trusted collectors can access.
The receiver closes the collector connection after capture and frame resolution, before it uploads the report.

