// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Log-output capability trait.
//!
//! Lets the trace pipeline emit already-encoded log-exporter output to the
//! platform's log sink. On native targets this writes to process stdout; a wasm
//! consumer implements it by handing the bytes to the host (e.g. JavaScript),
//! since wasm cannot write to stdout directly.

/// Capability for writing encoded log-exporter output to the platform log sink.
pub trait LogWriterCapability {
    /// Write a buffer of newline-delimited log output.
    ///
    /// `bytes` may contain one or more `\n`-terminated JSON lines. Implementations
    /// should write the whole buffer (so individual lines are not interleaved with
    /// other writers) and flush before returning.
    fn write_log_output(&self, bytes: &[u8]) -> std::io::Result<()>;
}
