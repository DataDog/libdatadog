// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Constants used for the Unix socket communication protocol between crash tracker collector and receiver.
//!
//! This module contains all the delimiter constants that structure the crash report data stream.
//! These constants are used to mark the beginning and end of different sections in the crash report,
//! allowing the receiver to properly parse and reconstruct the crash information.
//!
//! For complete protocol documentation, see [`crate::shared::unix_socket_communication`].

use std::time::Duration;

// Section delimiters for the crash report stream protocol

/// Marks the beginning of the metadata section containing application context, tags, and environment information.
/// The section contains a JSON-serialized `Metadata` object.
pub const DD_CRASHTRACK_BEGIN_METADATA: &str = "DD_CRASHTRACK_BEGIN_METADATA";
/// Marks the end of the metadata section.
pub const DD_CRASHTRACK_END_METADATA: &str = "DD_CRASHTRACK_END_METADATA";

/// Marks the beginning of the configuration section containing crash tracking settings.
/// The section contains a JSON-serialized `CrashtrackerConfiguration` object with endpoint information and processing options.
pub const DD_CRASHTRACK_BEGIN_CONFIG: &str = "DD_CRASHTRACK_BEGIN_CONFIG";
/// Marks the end of the configuration section.
pub const DD_CRASHTRACK_END_CONFIG: &str = "DD_CRASHTRACK_END_CONFIG";

/// Marks the beginning of the signal information section containing crash signal details.
/// The section contains JSON with signal code, number, human-readable names, and fault address (if applicable).
pub const DD_CRASHTRACK_BEGIN_SIGINFO: &str = "DD_CRASHTRACK_BEGIN_SIGINFO";
/// Marks the end of the signal information section.
pub const DD_CRASHTRACK_END_SIGINFO: &str = "DD_CRASHTRACK_END_SIGINFO";

/// Marks the beginning of the process context section containing processor state at crash time.
/// The section contains platform-specific context dump from `ucontext_t`.
pub const DD_CRASHTRACK_BEGIN_UCONTEXT: &str = "DD_CRASHTRACK_BEGIN_UCONTEXT";
/// Marks the end of the process context section.
pub const DD_CRASHTRACK_END_UCONTEXT: &str = "DD_CRASHTRACK_END_UCONTEXT";

/// Marks the beginning of the process information section containing the PID of the crashing process.
/// The section contains JSON with the process ID.
pub const DD_CRASHTRACK_BEGIN_PROCINFO: &str = "DD_CRASHTRACK_BEGIN_PROCESSINFO";
/// Marks the end of the process information section.
pub const DD_CRASHTRACK_END_PROCINFO: &str = "DD_CRASHTRACK_END_PROCESSINFO";

/// Marks the beginning of the counters section containing internal crash tracker metrics.
pub const DD_CRASHTRACK_BEGIN_COUNTERS: &str = "DD_CRASHTRACK_BEGIN_COUNTERS";
/// Marks the end of the counters section.
pub const DD_CRASHTRACK_END_COUNTERS: &str = "DD_CRASHTRACK_END_COUNTERS";

/// Marks the beginning of the spans section containing active distributed tracing spans at crash time.
pub const DD_CRASHTRACK_BEGIN_SPAN_IDS: &str = "DD_CRASHTRACK_BEGIN_SPAN_IDS";
/// Marks the end of the spans section.
pub const DD_CRASHTRACK_END_SPAN_IDS: &str = "DD_CRASHTRACK_END_SPAN_IDS";

/// Marks the beginning of the additional tags section containing extra tags collected at crash time.
pub const DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS: &str = "DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS";
/// Marks the end of the additional tags section.
pub const DD_CRASHTRACK_END_ADDITIONAL_TAGS: &str = "DD_CRASHTRACK_END_ADDITIONAL_TAGS";

/// Marks the beginning of the traces section containing active trace information.
pub const DD_CRASHTRACK_BEGIN_TRACE_IDS: &str = "DD_CRASHTRACK_BEGIN_TRACE_IDS";
/// Marks the end of the traces section.
pub const DD_CRASHTRACK_END_TRACE_IDS: &str = "DD_CRASHTRACK_END_TRACE_IDS";

/// Marks the beginning of a file section (e.g., `/proc/self/maps` on Linux).
/// Used for memory mapping information needed for symbol resolution.
pub const DD_CRASHTRACK_BEGIN_FILE: &str = "DD_CRASHTRACK_BEGIN_FILE";
/// Marks the end of a file section.
pub const DD_CRASHTRACK_END_FILE: &str = "DD_CRASHTRACK_END_FILE";

/// Marks the beginning of the stack trace section containing stack frames.
/// Each line in this section represents a stack frame with addresses and optional debug information.
/// Frame format depends on symbol resolution settings.
pub const DD_CRASHTRACK_BEGIN_STACKTRACE: &str = "DD_CRASHTRACK_BEGIN_STACKTRACE";
/// Marks the end of the stack trace section.
pub const DD_CRASHTRACK_END_STACKTRACE: &str = "DD_CRASHTRACK_END_STACKTRACE";

/// Marks the completion of the entire crash report transmission.
/// This is the final marker sent by the collector to indicate all data has been transmitted.
pub const DD_CRASHTRACK_DONE: &str = "DD_CRASHTRACK_DONE";

/// Default timeout for receiver operations in milliseconds.
/// This prevents the receiver from hanging indefinitely on incomplete or corrupted streams.
/// Can be overridden by the `DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS` environment variable.
pub const DD_CRASHTRACK_DEFAULT_TIMEOUT: Duration = Duration::from_millis(5_000);
