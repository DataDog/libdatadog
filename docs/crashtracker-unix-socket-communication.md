# Crash Tracker Unix Socket Communication Protocol

**Date**: September 23, 2025

## Overview

This document describes the Unix domain socket communication protocol used between the crash tracker's collector and receiver processes. The crash tracker uses a two-process architecture where the collector (a fork of the crashing process) communicates crash data to the receiver (a fork+execve process) via an anonymous Unix domain socket pair.

## Socket Creation and Setup

The communication channel is established using `socketpair()` to create an anonymous Unix domain socket pair:

```rust
let (uds_parent, uds_child) = socket::socketpair(
    socket::AddressFamily::Unix,
    socket::SockType::Stream,
    None,
    socket::SockFlag::empty(),
)?;
```

**Location**: `datadog-crashtracker/src/collector/receiver_manager.rs:78-85`

### File Descriptor Management

1. **Parent Process**: Retains `uds_parent` for tracking
2. **Collector Process**: Inherits `uds_parent` as the write end
3. **Receiver Process**: Gets `uds_child` redirected to stdin via `dup2(uds_child, 0)`

## Communication Protocol

### Data Format

The crash data is transmitted as a structured text stream with distinct sections delimited by markers defined in `datadog-crashtracker/src/shared/constants.rs`.

### Message Structure

Each crash report follows this sequence:

1. **Metadata Section**
2. **Configuration Section**
3. **Signal Information Section**
4. **Process Context Section**
5. **Process Information Section**
6. **Counters Section**
7. **Spans Section**
8. **Additional Tags Section**
9. **Traces Section**
10. **Memory Maps Section** (Linux only)
11. **Stack Trace Section**
12. **Completion Marker**

### Section Details

#### 1. Metadata Section
```
DD_CRASHTRACK_BEGIN_METADATA
{JSON metadata object}
DD_CRASHTRACK_END_METADATA
```

Contains serialized `Metadata` object with application context, tags, and environment information.

#### 2. Configuration Section
```
DD_CRASHTRACK_BEGIN_CONFIG
{JSON configuration object}
DD_CRASHTRACK_END_CONFIG
```

Contains serialized `CrashtrackerConfiguration` with crash tracking settings, endpoint information, and processing options.

#### 3. Signal Information Section
```
DD_CRASHTRACK_BEGIN_SIGINFO
{
  "si_code": <signal_code>,
  "si_code_human_readable": "<description>",
  "si_signo": <signal_number>,
  "si_signo_human_readable": "<signal_name>",
  "si_addr": "<fault_address>" // Optional, for memory faults
}
DD_CRASHTRACK_END_SIGINFO
```

Contains signal details extracted from `siginfo_t` structure.

**Implementation**: `datadog-crashtracker/src/collector/emitters.rs:223-263`

#### 4. Process Context Section (ucontext)
```
DD_CRASHTRACK_BEGIN_UCONTEXT
<platform-specific context dump>
DD_CRASHTRACK_END_UCONTEXT
```

Contains processor state at crash time from `ucontext_t`. Format varies by platform:
- **Linux**: Direct debug print of `ucontext_t`
- **macOS**: Includes both `ucontext_t` and machine context (`mcontext`)

**Implementation**: `datadog-crashtracker/src/collector/emitters.rs:190-221`

#### 5. Process Information Section
```
DD_CRASHTRACK_BEGIN_PROCINFO
{"pid": <process_id>}
DD_CRASHTRACK_END_PROCINFO
```

Contains the process ID of the crashing process.

#### 6. Counters Section
```
DD_CRASHTRACK_BEGIN_COUNTERS
<counter data>
DD_CRASHTRACK_END_COUNTERS
```

Contains internal crash tracker counters and metrics.

#### 7. Spans Section
```
DD_CRASHTRACK_BEGIN_SPANS
<span data>
DD_CRASHTRACK_END_SPANS
```

Contains active distributed tracing spans at crash time.

#### 8. Additional Tags Section
```
DD_CRASHTRACK_BEGIN_TAGS
<tag data>
DD_CRASHTRACK_END_TAGS
```

Contains additional tags collected at crash time.

#### 9. Traces Section
```
DD_CRASHTRACK_BEGIN_TRACES
<trace data>
DD_CRASHTRACK_END_TRACES
```

Contains active trace information.

#### 10. Memory Maps Section (Linux Only)
```
DD_CRASHTRACK_BEGIN_FILE /proc/self/maps
<contents of /proc/self/maps>
DD_CRASHTRACK_END_FILE "/proc/self/maps"
```

Contains memory mapping information from `/proc/self/maps` for symbol resolution.

**Implementation**: `datadog-crashtracker/src/collector/emitters.rs:184-187`

#### 11. Stack Trace Section
```
DD_CRASHTRACK_BEGIN_STACKTRACE
{"ip": "<instruction_pointer>", "module_base_address": "<base>", "sp": "<stack_pointer>", "symbol_address": "<addr>"}
{"ip": "<instruction_pointer>", "module_base_address": "<base>", "sp": "<stack_pointer>", "symbol_address": "<addr>", "function": "<name>", "file": "<path>", "line": <number>}
...
DD_CRASHTRACK_END_STACKTRACE
```

Each line represents one stack frame. Frame format depends on symbol resolution setting:

- **Disabled/Receiver-only**: Only addresses (`ip`, `sp`, `symbol_address`, optional `module_base_address`)
- **In-process symbols**: Includes debug information (`function`, `file`, `line`, `column`)

Stack frames with stack pointer less than the fault stack pointer are filtered out to exclude crash tracker frames.

**Implementation**: `datadog-crashtracker/src/collector/emitters.rs:45-117`

#### 12. Completion Marker
```
DD_CRASHTRACK_DONE
```

Indicates end of crash report transmission.

## Communication Flow

### 1. Collector Side (Write End)

**File**: `datadog-crashtracker/src/collector/collector_manager.rs:92-102`

```rust
let mut unix_stream = unsafe { UnixStream::from_raw_fd(uds_fd) };

let report = emit_crashreport(
    &mut unix_stream,
    config,
    config_str,
    metadata_str,
    sig_info,
    ucontext,
    ppid,
);
```

The collector:
1. Creates `UnixStream` from inherited file descriptor
2. Calls `emit_crashreport()` to serialize and write all crash data
3. Flushes the stream after each section for reliability
4. Exits with `libc::_exit(0)` on completion

### 2. Receiver Side (Read End)

**File**: `datadog-crashtracker/src/receiver/entry_points.rs:97-119`

```rust
pub(crate) async fn receiver_entry_point(
    timeout: Duration,
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<()> {
    if let Some((config, mut crash_info)) = receive_report_from_stream(timeout, stream).await? {
        // Process crash data
        if let Err(e) = resolve_frames(&config, &mut crash_info) {
            crash_info.log_messages.push(format!("Error resolving frames: {e}"));
        }
        if config.demangle_names() {
            if let Err(e) = crash_info.demangle_names() {
                crash_info.log_messages.push(format!("Error demangling names: {e}"));
            }
        }
        crash_info.async_upload_to_endpoint(config.endpoint()).await?;
    }
    Ok(())
}
```

The receiver:
1. Reads from stdin (Unix socket via `dup2`)
2. Parses the structured stream into `CrashInfo` and `CrashtrackerConfiguration`
3. Performs symbol resolution if configured
4. Uploads formatted crash report to backend

### 3. Stream Parsing

**File**: `datadog-crashtracker/src/receiver/receive_report.rs`

The receiver parses the stream by:
1. Reading line-by-line with timeout protection
2. Matching delimiter patterns to identify sections
3. Accumulating section data between delimiters
4. Deserializing JSON sections into appropriate data structures
5. Handling the `DD_CRASHTRACK_DONE` completion marker

## Error Handling and Reliability

### Signal Safety
- All collector operations use only async-signal-safe functions
- No memory allocation in signal handler context
- Pre-prepared data structures (`PreparedExecve`) to avoid allocations

### Timeout Protection
- Receiver has configurable timeout (default: 4000ms)
- Environment variable: `DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS`
- Prevents hanging on incomplete/corrupted streams

### Process Cleanup
- Parent process uses `wait_for_pollhup()` to detect socket closure
- Kills child processes with `SIGKILL` if needed
- Reaps zombie processes to prevent resource leaks

**File**: `datadog-crashtracker/src/collector/process_handle.rs:19-40`

### Data Integrity
- Each section is flushed immediately after writing
- Structured delimiters allow detection of incomplete transmissions
- Error messages are accumulated rather than failing fast

## Alternative Communication Modes

### Named Socket Mode
When `unix_socket_path` is configured, the collector connects to an existing Unix socket instead of using the fork+execve receiver:

```rust
let receiver = if unix_socket_path.is_empty() {
    Receiver::spawn_from_stored_config()?  // Fork+execve mode
} else {
    Receiver::from_socket(unix_socket_path)?  // Named socket mode
};
```

This allows integration with long-lived receiver processes.

**Linux Abstract Sockets**: On Linux, socket paths not starting with `.` or `/` are treated as abstract socket names.

## Security Considerations

### File Descriptor Isolation
- Collector closes stdio file descriptors (0, 1, 2)
- Receiver redirects socket to stdin, stdout/stderr to configured files
- Minimizes attack surface during crash processing

### Process Isolation
- Fork+execve provides strong process boundary
- Crash in collector doesn't affect receiver
- Signal handlers are reset in receiver child

### Resource Limits
- Timeout prevents resource exhaustion
- Fixed buffer sizes for file operations
- Immediate flushing prevents large memory usage

## Debugging and Monitoring

### Log Output
- Receiver can be configured with `stdout_filename` and `stderr_filename`
- Error messages are accumulated in crash report
- Debug assertions validate critical operations

### Environment Variables
- `DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS`: Receiver timeout
- Standard Unix environment passed through execve

This communication protocol ensures reliable crash data collection and transmission even when the main process is in an unstable state, providing robust crash reporting capabilities for production systems.
