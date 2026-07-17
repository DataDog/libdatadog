// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Linux implementation of the [OTEL process
//! context specification](https://github.com/open-telemetry/opentelemetry-specification/blob/main/oteps/profiles/4719-process-ctx.md).
//!
//! The implementation follows the discovery method described in the specification: it uses a
//! memfd or a named mapping with the name `OTEL_CTX`.
//!
//! The update/read protocol is seqlock-style: the publisher marks the mapping as unavailable,
//! writes the payload metadata, publishes a non-zero version, and readers accept a copy only if
//! the version they observed before copying still matches afterward. The general algorithm and
//! the C++ memory-model constraints are described in Boehm's
//! [Can Seqlocks Get Along With Programming Language Memory Models?](https://web.archive.org/web/20211106170334/https://www.hpl.hp.com/techreports/2012/HPL-2012-68.pdf).
//! Linux has its own [seqlock/seqcount implementation](https://github.com/torvalds/linux/blob/master/include/linux/seqlock.h),
//! but its barriers are specified by the Linux kernel memory model, not by the C++/Rust models.
//!
//! This implementation differs from the usual odd/even counter form in two ways: `0` is the
//! in-progress sentinel, and each non-zero `monotonic_published_at_ns` value is the
//! reader-visible version. Updates force that timestamp to advance so readers can detect torn
//! reads even when the clock returns the same value twice. Concurrent writers are rejected, and
//! retry policy is left to the reader's caller.

#[cfg(any(target_os = "linux", all(unix, feature = "process-context-reader")))]
use std::io;

#[cfg(feature = "process-context-reader")]
pub(crate) mod reader;
#[cfg(feature = "process-context-writer")]
pub(crate) mod writer;
#[cfg(all(target_os = "linux", not(target_has_atomic = "64")))]
compile_error!("OTel process context requires 64-bit atomics on Linux");
#[cfg(target_os = "linux")]
pub mod linux;

/// Current version of the process context format
pub const PROCESS_CTX_VERSION: u32 = 2;
/// Signature bytes for identifying process context mappings
pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
/// Sentinel timestamp indicating that the context is unpublished or being updated.
pub(crate) const UNPUBLISHED_OR_UPDATING: u64 = 0;

#[cfg(all(target_os = "linux", feature = "process-context-reader"))]
/// Reader for the current process's OTel process context.
pub type ProcessContextSelfReader = reader::ProcessContextReader<reader::linux::LinuxReaderBackend>;

#[cfg(all(target_os = "linux", feature = "process-context-writer"))]
static PROCESS_CONTEXT_WRITER: writer::ProcessContextWriter<writer::linux::LinuxWriterBackend> =
    writer::ProcessContextWriter::new();

#[cfg(all(target_os = "linux", feature = "process-context-writer"))]
/// Publishes or updates the process context through the OTel Linux discovery mechanism.
pub fn publish(
    context: &libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext,
) -> io::Result<()> {
    PROCESS_CONTEXT_WRITER.publish(context)
}

#[cfg(all(target_os = "linux", feature = "process-context-writer"))]
/// Removes the process-context publication and releases its header allocation.
pub fn unpublish() -> io::Result<()> {
    PROCESS_CONTEXT_WRITER.unpublish()
}

#[cfg(any(
    feature = "process-context-reader",
    all(
        feature = "process-context-writer",
        any(target_os = "macos", target_os = "windows")
    )
))]
#[cold]
pub(crate) fn last_error(context: &'static str) -> std::io::Error {
    let error = std::io::Error::last_os_error();
    std::io::Error::new(error.kind(), format!("{context}: {error}"))
}

/// Runs an operation until it succeeds or fails for a reason other than `EINTR`.
#[cfg(any(
    all(unix, feature = "process-context-reader"),
    all(target_os = "linux", feature = "process-context-writer")
))]
pub(crate) fn retry_on_eintr<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match operation() {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

#[cfg(all(
    test,
    target_os = "linux",
    feature = "process-context-reader",
    feature = "process-context-writer"
))]
pub(crate) mod tests {
    pub(crate) fn publish_raw_payload(payload: Vec<u8>) -> std::io::Result<()> {
        crate::otel_process_ctx::PROCESS_CONTEXT_WRITER.publish_raw_payload(payload)
    }
}
