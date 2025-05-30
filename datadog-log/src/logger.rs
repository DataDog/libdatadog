// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::writers::{FileWriter, NoopWriter, StdoutWriter};
use ddcommon_ffi::Error;
use once_cell::sync::Lazy;
use std::cmp::PartialOrd;
use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, Registry};

/// Log level for filtering log events.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogEventLevel {
    /// The "trace" level.
    ///
    /// Designates very low priority, often extremely verbose, information.
    Trace = 0,
    /// The "debug" level.
    ///
    /// Designates lower priority information.
    Debug = 1,
    /// The "info" level.
    ///
    /// Designates useful information.
    Info = 2,
    /// The "warn" level.
    ///
    /// Designates hazardous situations.
    Warn = 3,
    /// The "error" level.
    ///
    /// Designates very serious errors.
    Error = 4,
}

/// Configuration for file-based logging.
pub struct FileConfig {
    /// Path where log files will be written.
    pub path: String,
}

/// Specifies where log output should be written.
pub enum WriterConfig {
    /// Discards all log output.
    Noop,
    /// Writes to standard output.
    Stdout,
    /// Writes to a file with specified configuration.
    File(FileConfig),
}

/// Configuration for the logger.
pub struct LoggerConfig {
    /// Minimum level for log events to be processed.
    pub level: LogEventLevel,
    /// Where to write the log output.
    pub writer: WriterConfig,
}

/// A writer that can be dynamically changed at runtime.
/// Used internally by the logger to support switching between different output destinations.
#[derive(Clone)]
pub struct DynamicWriter {
    pub(crate) writer: Arc<Mutex<Box<dyn Write + Send + 'static>>>,
}

impl Default for DynamicWriter {
    fn default() -> Self {
        DynamicWriter {
            writer: Arc::new(Mutex::new(Box::new(NoopWriter))),
        }
    }
}

impl Write for DynamicWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer
            .lock()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .lock()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .flush()
    }
}

impl DynamicWriter {
    /// Updates the underlying writer with a new implementation.
    /// This allows changing the log destination at runtime.
    pub fn update_writer(&self, new_writer: Box<dyn Write + Send + 'static>) -> Result<(), Error> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| Error::from(format!("Failed to acquire lock: {}", e)))?;
        *writer = new_writer;
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for DynamicWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[derive(Clone)]
struct Logger {
    writer: DynamicWriter,
    filter_handle: reload::Handle<LevelFilter, Registry>,
}

impl Logger {
    fn with_writer(writer: Box<dyn Write + Send + 'static>) -> Self {
        let level = Level::INFO;
        let dynamic_writer = DynamicWriter {
            writer: Arc::new(Mutex::new(writer)),
        };

        let format = tracing_subscriber::fmt::format()
            .with_ansi(false)
            .with_level(true)
            .with_target(false)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_file(true)
            .with_line_number(true)
            .json();

        let filter = LevelFilter::from_level(level);
        let (filter_layer, reload_handle) = reload::Layer::new(filter);

        let fmt_layer = fmt::Layer::default()
            .event_format(format)
            .with_writer(dynamic_writer.clone());

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .init();

        Self {
            writer: dynamic_writer,
            filter_handle: reload_handle,
        }
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::with_writer(Box::new(NoopWriter))
    }
}

impl Logger {
    pub fn configure(&mut self, config: LoggerConfig) -> Result<(), Error> {
        // Ensure writes are flushed before changing the writer
        self.writer
            .flush()
            .map_err(|e| Error::from(format!("Failed to flush logger: {}", e)))?;

        // Update the log level
        self.filter_handle
            .reload(LevelFilter::from(config.level))
            .map_err(|e| Error::from(format!("Failed to set log level: {}", e)))?;

        // Update the writer
        match config.writer {
            WriterConfig::Noop => {
                let writer = Box::new(NoopWriter);
                self.writer.update_writer(writer)?;
            }
            WriterConfig::Stdout => {
                let writer = Box::new(StdoutWriter::new());
                self.writer.update_writer(writer)?;
            }
            WriterConfig::File(file_config) => {
                let file_path = Path::new(&file_config.path);
                let file_writer = FileWriter::new(file_path)
                    .map_err(|e| Error::from(format!("Failed to create file writer: {}", e)))?;
                self.writer.update_writer(Box::new(file_writer))?;
            }
        }

        Ok(())
    }
}

impl From<LogEventLevel> for LevelFilter {
    fn from(level: LogEventLevel) -> Self {
        match level {
            LogEventLevel::Trace => LevelFilter::TRACE,
            LogEventLevel::Debug => LevelFilter::DEBUG,
            LogEventLevel::Info => LevelFilter::INFO,
            LogEventLevel::Warn => LevelFilter::WARN,
            LogEventLevel::Error => LevelFilter::ERROR,
        }
    }
}

static LOGGER: Lazy<Mutex<Option<Logger>>> = Lazy::new(|| Mutex::new(None));

pub fn logger_configure(config: LoggerConfig) -> Result<(), Error> {
    let mut logger_guard = LOGGER
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    if let Some(logger) = logger_guard.as_mut() {
        // Logger exists, configure it
        logger.configure(config)
    } else {
        // Create and configure new logger
        let mut new_logger = Logger::default();
        new_logger.configure(config)?;
        *logger_guard = Some(new_logger);
        Ok(())
    }
}

pub fn logger_set_log_level(log_level: LogEventLevel) -> Result<(), Error> {
    let level_filter = LevelFilter::from(log_level);
    let logger = LOGGER
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    logger
        .as_ref()
        .ok_or_else(|| Error::from("Logger not initialized"))?
        .filter_handle
        .reload(level_filter)
        .map_err(|e| Error::from(format!("Failed to set log level: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;
    use tracing::{debug, error, info, warn};

    /// Waits for a specific message to appear in a log file.
    ///
    /// Since the logger uses a non-blocking async writer, writes to the log file happen
    /// asynchronously. This function polls the file until either:
    /// - The message is found (returns true)
    /// - The timeout is reached (returns false)
    ///
    /// # Arguments
    /// * `path` - Path to the log file to monitor
    /// * `message` - The message to look for in the file
    ///
    /// # Returns
    /// * `true` if the message was found within the timeout period
    fn wait_for_log_message(path: &Path, message: &str) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok(contents) = fs::read_to_string(path) {
                if contents.contains(message) {
                    println!("Found message '{}' after {:?}", message, start.elapsed());
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("Timeout waiting for message: '{}'", message);
        false
    }

    #[test]
    fn test_global_logger() {
        // Now test with file writer
        println!("\nTesting file writer...");
        let temp_dir = tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");
        println!("Writing logs to: {:?}", log_path);

        let config = LoggerConfig {
            level: LogEventLevel::Info,
            writer: WriterConfig::File(FileConfig {
                path: log_path.to_string_lossy().into_owned(),
            }),
        };
        assert!(logger_configure(config).is_ok());

        // Test logging with file writer
        let test_message = "test log to file";
        debug!("{} at debug", test_message); // Should not appear due to Info level
        info!("{} at info", test_message);
        warn!("{} at warn", test_message);
        error!("{} at error", test_message);

        // Debug message should not appear due to Info level
        assert!(
            !wait_for_log_message(&log_path, "test log to file at debug"),
            "Debug log should not appear"
        );

        // Info and above should appear
        assert!(
            wait_for_log_message(&log_path, "test log to file at info"),
            "Info log should appear"
        );
        assert!(
            wait_for_log_message(&log_path, "test log to file at warn"),
            "Warn log should appear"
        );
        assert!(
            wait_for_log_message(&log_path, "test log to file at error"),
            "Error log should appear"
        );

        // Test changing log level
        assert!(logger_set_log_level(LogEventLevel::Error).is_ok());

        // Log more messages
        let test_message = "after level change";
        info!("{} at info", test_message); // Should not appear
        warn!("{} at warn", test_message); // Should not appear
        error!("{} at error", test_message); // Should appear

        // Only error level should appear after level change
        assert!(
            !wait_for_log_message(&log_path, "after level change at info"),
            "Info log should not appear after level change"
        );
        assert!(
            !wait_for_log_message(&log_path, "after level change at warn"),
            "Warn log should not appear after level change"
        );
        assert!(
            wait_for_log_message(&log_path, "after level change at error"),
            "Error log should appear after level change"
        );
    }
}
