// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::path::Path;
use std::{fs, io};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};

/// A writer that discards all output.
pub struct NoopWriter;

impl Write for NoopWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A non-blocking writer that writes log output to a file.
///
/// Uses a background thread to handle writes asynchronously, which improves
/// performance by not blocking the logging thread. The background thread is
/// managed by the internal `WorkerGuard`.
pub struct FileWriter {
    non_blocking: NonBlocking,
    /// The WorkerGuard is crucial for the non-blocking writer's functionality.
    ///
    /// The guard represents ownership of the background worker thread that processes
    /// writes asynchronously. When the guard is dropped, it ensures:
    /// 1. All pending writes are flushed
    /// 2. The worker thread is properly shut down
    /// 3. No writes are lost
    ///
    /// If we don't keep the guard alive for the entire lifetime of the writer:
    /// - The worker thread might be shut down prematurely
    /// - Pending writes could be lost
    /// - The non-blocking writer would stop functioning
    ///
    /// That's why we store it in the struct and name it with a leading underscore
    /// to indicate it's intentionally unused but must be kept alive.
    _guard: WorkerGuard,
}

impl FileWriter {
    /// Creates a new file writer that writes to the specified path.
    ///
    /// If the parent directory doesn't exist, it will be created.
    /// The file will be opened in append mode.
    pub fn new(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file_appender = tracing_appender::rolling::never(
            path.parent().unwrap_or_else(|| Path::new(".")),
            path.file_name().unwrap_or_default(),
        );
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        Ok(Self {
            non_blocking,
            _guard: guard,
        })
    }
}

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.non_blocking.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.non_blocking.flush()
    }
}

/// A writer that writes log output to standard output.
pub struct StdoutWriter(io::Stdout);

impl StdoutWriter {
    /// Creates a new writer that writes to standard output.
    pub fn new() -> Self {
        Self(io::stdout())
    }
}

impl Write for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
