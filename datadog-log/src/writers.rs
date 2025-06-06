// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::logger::StdTarget;
use std::io::Write;
use std::path::Path;
use std::{fs, io};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::fmt::MakeWriter;

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

impl<'a> MakeWriter<'a> for FileWriter {
    type Writer = NonBlocking;

    fn make_writer(&'a self) -> Self::Writer {
        self.non_blocking.clone()
    }
}

/// A writer that writes log output to standard output or standard error.
pub struct StdWriter {
    target: StdTarget,
}

impl StdWriter {
    /// Creates a new writer that writes to the specified standard stream.
    pub fn new(target: StdTarget) -> Self {
        Self { target }
    }
}

impl Write for StdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.target {
            StdTarget::Out => io::stdout().write(buf),
            StdTarget::Err => io::stderr().write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.target {
            StdTarget::Out => io::stdout().flush(),
            StdTarget::Err => io::stderr().flush(),
        }
    }
}

impl<'a> MakeWriter<'a> for StdWriter {
    type Writer = StdWriter;

    fn make_writer(&'a self) -> Self::Writer {
        StdWriter::new(self.target)
    }
}
