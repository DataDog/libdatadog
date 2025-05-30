// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_log::logger;
use datadog_log::logger::{logger_configure, logger_set_log_level};
use ddcommon_ffi::{CharSlice, Error};

/// Sets the global log level.
///
/// # Arguments
/// * `log_level` - The minimum level for events to be logged
///
/// # Errors
/// Returns an error if the log level cannot be set.
#[no_mangle]
pub extern "C" fn ddog_logger_set_log_level(
    log_level: logger::LogEventLevel,
) -> Option<Box<Error>> {
    logger_set_log_level(log_level).err().map(Box::new)
}

/// Configures the global logger.
///
/// # Arguments
/// * `config` - Configuration for the logger including level and output destination
///
/// # Errors
/// Returns an error if the logger cannot be configured.
#[no_mangle]
pub extern "C" fn ddog_logger_configure(config: LoggerConfig) -> Option<Box<Error>> {
    logger_configure(logger::LoggerConfig::from(config))
        .err()
        .map(Box::new)
}

/// Configuration for the logger.
#[repr(C)]
pub struct LoggerConfig<'a> {
    /// Minimum level for events to be logged
    pub level: logger::LogEventLevel,
    /// Output configuration
    pub writer: WriterConfig<'a>,
}

/// Configuration for log output destination.
#[repr(C)]
pub struct WriterConfig<'a> {
    /// Type of output destination
    pub kind: WriterKind,
    /// File configuration, required when `kind` is [`WriterKind::File`]
    pub file: Option<&'a FileConfig<'a>>,
}

/// Configuration for file output.
#[repr(C)]
pub struct FileConfig<'a> {
    /// Path to the log file
    pub path: CharSlice<'a>,
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

impl From<LoggerConfig<'_>> for logger::LoggerConfig {
    fn from(config: LoggerConfig) -> Self {
        logger::LoggerConfig {
            level: config.level,
            writer: logger::WriterConfig::from(config.writer),
        }
    }
}

impl From<WriterConfig<'_>> for logger::WriterConfig {
    fn from(config: WriterConfig) -> Self {
        match config.kind {
            WriterKind::Noop => logger::WriterConfig::Noop,
            WriterKind::Stdout => logger::WriterConfig::Stdout,
            WriterKind::File => {
                match config.file {
                    Some(file_config) => {
                        logger::WriterConfig::File(logger::FileConfig::from(FileConfig {
                            path: file_config.path,
                        }))
                    }
                    None => logger::WriterConfig::Noop, /* If no file config is provided,
                                                         * fallback to Noop */
                }
            }
        }
    }
}

impl From<FileConfig<'_>> for logger::FileConfig {
    fn from(file_config: FileConfig) -> Self {
        logger::FileConfig {
            path: file_config.path.to_string(),
        }
    }
}
