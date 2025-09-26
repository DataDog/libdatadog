// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Crash Tracker Unix Socket Communication Protocol
//!
//! This module documents the Unix domain socket communication protocol used between the crash
//! tracker's collector and receiver processes. The crash tracker uses a two-process architecture
//! where the collector (a fork of the crashing process) communicates crash data to the receiver
//! (a fork+execve process) via an anonymous Unix domain socket pair.
//!
//! ## Overview
//!
//! The communication protocol ensures reliable crash data collection and transmission even when
//! the main process is in an unstable state, providing robust crash reporting capabilities for
//! production systems.
//!
//! ## Socket Creation and Setup
//!
//! The communication channel is established using [`socketpair()`] to create an anonymous Unix
//! domain socket pair:
//!
//! ```rust,no_run
//! use nix::sys::socket;
//!
//! let (uds_parent, uds_child) = socket::socketpair(
//!     socket::AddressFamily::Unix,
//!     socket::SockType::Stream,
//!     None,
//!     socket::SockFlag::empty(),
//! )?;
//! # Ok::<(), nix::Error>(())
//! ```
//!
//! **Location**: [`collector::receiver_manager`]
//!
//! ### File Descriptor Management
//!
//! 1. **Parent Process**: Retains `uds_parent` for tracking
//! 2. **Collector Process**: Inherits `uds_parent` as the write end
//! 3. **Receiver Process**: Gets `uds_child` redirected to stdin via `dup2(uds_child, 0)`
//!
//! ## Communication Protocol
//!
//! ### Data Format
//!
//! The crash data is transmitted as a structured text stream with distinct sections delimited
//! by markers defined in [`shared::constants`].
//!
//! ### Message Structure
//!
//! Each crash report follows this sequence:
//!
//! 1. **Metadata Section** - Application context, tags, and environment information
//! 2. **Configuration Section** - Crash tracking settings, endpoint information, processing options
//! 3. **Signal Information Section** - Signal details from `siginfo_t` structure
//! 4. **Process Context Section** - Processor state at crash time from `ucontext_t`
//! 5. **Process Information Section** - Process ID of the crashing process
//! 6. **Counters Section** - Internal crash tracker counters and metrics
//! 7. **Spans Section** - Active distributed tracing spans at crash time
//! 8. **Additional Tags Section** - Additional tags collected at crash time
//! 9. **Traces Section** - Active trace information
//! 10. **Memory Maps Section** (Linux only) - Memory mapping information from `/proc/self/maps`
//! 11. **Stack Trace Section** - Stack frames with optional symbol resolution
//! 12. **Completion Marker** - End of crash report transmission
//!
//! ### Section Details
//!
//! #### 1. Metadata Section
//! ```text
//! DD_CRASHTRACK_BEGIN_METADATA
//! {JSON metadata object}
//! DD_CRASHTRACK_END_METADATA
//! ```
//!
//! Contains serialized `Metadata` object with application context, tags, and environment information.
//!
//! #### 2. Configuration Section
//! ```text
//! DD_CRASHTRACK_BEGIN_CONFIG
//! {JSON configuration object}
//! DD_CRASHTRACK_END_CONFIG
//! ```
//!
//! Contains serialized `CrashtrackerConfiguration` with crash tracking settings, endpoint
//! information, and processing options.
//!
//! #### 3. Signal Information Section
//! ```text
//! DD_CRASHTRACK_BEGIN_SIGINFO
//! {
//!   "si_code": <signal_code>,
//!   "si_code_human_readable": "<description>",
//!   "si_signo": <signal_number>,
//!   "si_signo_human_readable": "<signal_name>",
//!   "si_addr": "<fault_address>" // Optional, for memory faults
//! }
//! DD_CRASHTRACK_END_SIGINFO
//! ```
//!
//! Contains signal details extracted from `siginfo_t` structure.
//! **Implementation**: [`collector::emitters`] (lines 223-263)
//!
//! #### 4. Process Context Section (ucontext)
//! ```text
//! DD_CRASHTRACK_BEGIN_UCONTEXT
//! <platform-specific context dump>
//! DD_CRASHTRACK_END_UCONTEXT
//! ```
//!
//! Contains processor state at crash time from `ucontext_t`. Format varies by platform:
//! - **Linux**: Direct debug print of `ucontext_t`
//! - **macOS**: Includes both `ucontext_t` and machine context (`mcontext`)
//!
//! **Implementation**: [`collector::emitters`] (lines 190-221)
//!
//! #### 5. Process Information Section
//! ```text
//! DD_CRASHTRACK_BEGIN_PROCINFO
//! {"pid": <process_id>}
//! DD_CRASHTRACK_END_PROCINFO
//! ```
//!
//! Contains the process ID of the crashing process.
//!
//! #### 6. Counters Section
//! ```text
//! DD_CRASHTRACK_BEGIN_COUNTERS
//! <counter data>
//! DD_CRASHTRACK_END_COUNTERS
//! ```
//!
//! Contains internal crash tracker counters and metrics.
//!
//! #### 7. Spans Section
//! ```text
//! DD_CRASHTRACK_BEGIN_SPANS
//! <span data>
//! DD_CRASHTRACK_END_SPANS
//! ```
//!
//! Contains active distributed tracing spans at crash time.
//!
//! #### 8. Additional Tags Section
//! ```text
//! DD_CRASHTRACK_BEGIN_TAGS
//! <tag data>
//! DD_CRASHTRACK_END_TAGS
//! ```
//!
//! Contains additional tags collected at crash time.
//!
//! #### 9. Traces Section
//! ```text
//! DD_CRASHTRACK_BEGIN_TRACES
//! <trace data>
//! DD_CRASHTRACK_END_TRACES
//! ```
//!
//! Contains active trace information.
//!
//! #### 10. Memory Maps Section (Linux Only)
//! ```text
//! DD_CRASHTRACK_BEGIN_FILE /proc/self/maps
//! <contents of /proc/self/maps>
//! DD_CRASHTRACK_END_FILE "/proc/self/maps"
//! ```
//!
//! Contains memory mapping information from `/proc/self/maps` for symbol resolution.
//! **Implementation**: [`collector::emitters`] (lines 184-187)
//!
//! #### 11. Stack Trace Section
//! ```text
//! DD_CRASHTRACK_BEGIN_STACKTRACE
//! {"ip": "<instruction_pointer>", "module_base_address": "<base>", "sp": "<stack_pointer>", "symbol_address": "<addr>"}
//! {"ip": "<instruction_pointer>", "module_base_address": "<base>", "sp": "<stack_pointer>", "symbol_address": "<addr>", "function": "<name>", "file": "<path>", "line": <number>}
//! ...
//! DD_CRASHTRACK_END_STACKTRACE
//! ```
//!
//! Each line represents one stack frame. Frame format depends on symbol resolution setting:
//!
//! - **Disabled/Receiver-only**: Only addresses (`ip`, `sp`, `symbol_address`, optional `module_base_address`)
//! - **In-process symbols**: Includes debug information (`function`, `file`, `line`, `column`)
//!
//! Stack frames with stack pointer less than the fault stack pointer are filtered out to exclude crash tracker frames.
//! **Implementation**: [`collector::emitters`] (lines 45-117)
//!
//! #### 12. Completion Marker
//! ```text
//! DD_CRASHTRACK_DONE
//! ```
//!
//! Indicates end of crash report transmission.
//!
//! ## Communication Flow
//!
//! ### 1. Collector Side (Write End)
//!
//! **File**: [`collector::collector_manager`]
//!
//! ```rust,no_run
//! use std::os::unix::net::UnixStream;
//! use std::os::unix::io::FromRawFd;
//!
//! let mut unix_stream = unsafe { UnixStream::from_raw_fd(uds_fd) };
//!
//! let report = emit_crashreport(
//!     &mut unix_stream,
//!     config,
//!     config_str,
//!     metadata_str,
//!     sig_info,
//!     ucontext,
//!     ppid,
//! );
//! # let _: () = report; // suppress unused warning for doc test
//! ```
//!
//! The collector:
//! 1. Creates `UnixStream` from inherited file descriptor
//! 2. Calls `emit_crashreport()` to serialize and write all crash data
//! 3. Flushes the stream after each section for reliability
//! 4. Exits with `libc::_exit(0)` on completion
//!
//! ### 2. Receiver Side (Read End)
//!
//! **File**: [`receiver::entry_points`]
//!
//! ```rust,no_run
//! use std::time::Duration;
//! use tokio::io::AsyncBufReadExt;
//!
//! pub async fn receiver_entry_point(
//!     timeout: Duration,
//!     stream: impl AsyncBufReadExt + std::marker::Unpin,
//! ) -> anyhow::Result<()> {
//!     if let Some((config, mut crash_info)) = receive_report_from_stream(timeout, stream).await? {
//!         // Process crash data
//!         if let Err(e) = resolve_frames(&config, &mut crash_info) {
//!             crash_info.log_messages.push(format!("Error resolving frames: {e}"));
//!         }
//!         if config.demangle_names() {
//!             if let Err(e) = crash_info.demangle_names() {
//!                 crash_info.log_messages.push(format!("Error demangling names: {e}"));
//!             }
//!         }
//!         crash_info.async_upload_to_endpoint(config.endpoint()).await?;
//!     }
//!     Ok(())
//! }
//! # fn resolve_frames(_config: &(), _crash_info: &mut ()) -> Result<(), &'static str> { Ok(()) }
//! # fn receive_report_from_stream(_timeout: Duration, _stream: impl AsyncBufReadExt + std::marker::Unpin) -> impl std::future::Future<Output = anyhow::Result<Option<((), ())>>> { async { Ok(None) } }
//! # struct CrashInfo { log_messages: Vec<String> }
//! # impl CrashInfo {
//! #     fn demangle_names(&mut self) -> Result<(), &'static str> { Ok(()) }
//! #     async fn async_upload_to_endpoint(&self, _endpoint: ()) -> anyhow::Result<()> { Ok(()) }
//! # }
//! ```
//!
//! The receiver:
//! 1. Reads from stdin (Unix socket via `dup2`)
//! 2. Parses the structured stream into `CrashInfo` and `CrashtrackerConfiguration`
//! 3. Performs symbol resolution if configured
//! 4. Uploads formatted crash report to backend
//!
//! ### 3. Stream Parsing
//!
//! **File**: [`receiver::receive_report`]
//!
//! The receiver parses the stream by:
//! 1. Reading line-by-line with timeout protection
//! 2. Matching delimiter patterns to identify sections
//! 3. Accumulating section data between delimiters
//! 4. Deserializing JSON sections into appropriate data structures
//! 5. Handling the `DD_CRASHTRACK_DONE` completion marker
//!
//! ## Error Handling and Reliability
//!
//! ### Signal Safety
//! - All collector operations use only async-signal-safe functions
//! - No memory allocation in signal handler context
//! - Pre-prepared data structures (`PreparedExecve`) to avoid allocations
//!
//! ### Timeout Protection
//! - Receiver has configurable timeout (default: 4000ms)
//! - Environment variable: `DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS`
//! - Prevents hanging on incomplete/corrupted streams
//!
//! ### Process Cleanup
//! - Parent process uses `wait_for_pollhup()` to detect socket closure
//! - Kills child processes with `SIGKILL` if needed
//! - Reaps zombie processes to prevent resource leaks
//!
//! **File**: [`collector::process_handle`]
//!
//! ### Data Integrity
//! - Each section is flushed immediately after writing
//! - Structured delimiters allow detection of incomplete transmissions
//! - Error messages are accumulated rather than failing fast
//!
//! ## Alternative Communication Modes
//!
//! ### Named Socket Mode
//! When `unix_socket_path` is configured, the collector connects to an existing Unix socket
//! instead of using the fork+execve receiver:
//!
//! ```rust,no_run
//! # struct Receiver;
//! # impl Receiver {
//! #     fn spawn_from_stored_config() -> Result<Self, &'static str> { Ok(Receiver) }
//! #     fn from_socket(_path: &str) -> Result<Self, &'static str> { Ok(Receiver) }
//! # }
//! # let unix_socket_path = "";
//! let receiver = if unix_socket_path.is_empty() {
//!     Receiver::spawn_from_stored_config()?  // Fork+execve mode
//! } else {
//!     Receiver::from_socket(unix_socket_path)?  // Named socket mode
//! };
//! # let _: Receiver = receiver; // suppress unused warning
//! # Ok::<(), &str>(())
//! ```
//!
//! This allows integration with long-lived receiver processes.
//!
//! **Linux Abstract Sockets**: On Linux, socket paths not starting with `.` or `/` are treated
//! as abstract socket names.
//!
//! ## Security Considerations
//!
//! ### File Descriptor Isolation
//! - Collector closes stdio file descriptors (0, 1, 2)
//! - Receiver redirects socket to stdin, stdout/stderr to configured files
//! - Minimizes attack surface during crash processing
//!
//! ### Process Isolation
//! - Fork+execve provides strong process boundary
//! - Crash in collector doesn't affect receiver
//! - Signal handlers are reset in receiver child
//!
//! ### Resource Limits
//! - Timeout prevents resource exhaustion
//! - Fixed buffer sizes for file operations
//! - Immediate flushing prevents large memory usage
//!
//! ## Debugging and Monitoring
//!
//! ### Log Output
//! - Receiver can be configured with `stdout_filename` and `stderr_filename`
//! - Error messages are accumulated in crash report
//! - Debug assertions validate critical operations
//!
//! ### Environment Variables
//! - `DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS`: Receiver timeout
//! - Standard Unix environment passed through execve
//!
//! [`socketpair()`]: nix::sys::socket::socketpair
//! [`collector::receiver_manager`]: crate::collector::receiver_manager
//! [`shared::constants`]: crate::shared::constants
//! [`collector::emitters`]: crate::collector::emitters
//! [`collector::collector_manager`]: crate::collector::collector_manager
//! [`receiver::entry_points`]: crate::receiver::entry_points
//! [`receiver::receive_report`]: crate::receiver::receive_report
//! [`collector::process_handle`]: crate::collector::process_handle

// This module is pure documentation - no actual code needed