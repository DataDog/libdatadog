// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_log::logger;
use datadog_log::logger::{
    logger_configure_file, logger_configure_std, logger_disable_file, logger_disable_std,
    logger_set_log_level,
};
use ddcommon_ffi::{CharSlice, Error};

/// Configuration for standard stream output.
#[repr(C)]
pub struct StdConfig {
    /// Target stream (stdout or stderr)
    pub target: logger::StdTarget,
}

impl From<StdConfig> for logger::StdConfig {
    fn from(config: StdConfig) -> Self {
        logger::StdConfig {
            target: config.target,
        }
    }
}

/// Configures the logger to write to stdout or stderr with the specified configuration.
///
/// # Arguments
/// * `config` - Configuration for standard stream logging including target
///
/// # Errors
/// Returns an error if the logger cannot be configured.
#[no_mangle]
pub extern "C" fn ddog_logger_configure_std(config: StdConfig) -> Option<Box<Error>> {
    let config = logger::StdConfig::from(config);
    logger_configure_std(config).err().map(Box::new)
}

/// Disables logging by configuring a no-op logger.
///
/// # Errors
/// Returns an error if the logger cannot be configured.
#[no_mangle]
pub extern "C" fn ddog_logger_disable_std() -> Option<Box<Error>> {
    logger_disable_std().err().map(Box::new)
}

/// Configuration for file output.
#[repr(C)]
pub struct FileConfig<'a> {
    /// Path to the log file
    pub path: CharSlice<'a>,
    /// Maximum total number of files (current + rotated) to keep on disk.
    /// When this limit is exceeded, the oldest rotated files are deleted.
    /// Set to 0 to disable file cleanup.
    pub max_files: u64,
    /// Maximum size in bytes for each log file.
    /// Set to 0 to disable size-based rotation.
    pub max_size_bytes: u64,
}

impl<'a> From<FileConfig<'a>> for logger::FileConfig {
    fn from(config: FileConfig<'a>) -> Self {
        logger::FileConfig {
            path: config.path.to_string(),
            max_files: config.max_files,
            max_size_bytes: config.max_size_bytes,
        }
    }
}

/// Configures the logger to write to a file with the specified configuration.
///
/// # Arguments
/// * `config` - Configuration for file logging including path
///
/// # Errors
/// Returns an error if the logger cannot be configured.
#[no_mangle]
pub extern "C" fn ddog_logger_configure_file(config: FileConfig) -> Option<Box<Error>> {
    let config = logger::FileConfig::from(config);
    logger_configure_file(config).err().map(Box::new)
}

/// Disables file logging by configuring a no-op file writer.
///
/// # Errors
/// Returns an error if the logger cannot be configured.
#[no_mangle]
pub extern "C" fn ddog_logger_disable_file() -> Option<Box<Error>> {
    logger_disable_file().err().map(Box::new)
}

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
    let level_value = log_level as i8;
    if level_value >= 0 && level_value <= logger::LogEventLevel::Error as i8 {
        logger_set_log_level(log_level).err().map(Box::new)
    } else {
        Some(Box::new(Error::from("Invalid log level")))
    }
}
