// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_log::logger::{logger_init, logger_set_log_level, LogCallback, LogLevel};
use ddcommon_ffi::Error;

/// Updates the log level for the logger.
///
/// Only events at or above the specified level will be passed to the callback.
///
/// # Arguments
///
/// * `log_level` - The new log level to capture
///
/// # Returns
///
/// Returns `None` on success, or a boxed `Error` if the update fails.
#[no_mangle]
pub extern "C" fn ddog_log_set_log_level(log_level: LogLevel) -> Option<Box<Error>> {
    logger_set_log_level(log_level).err().map(Box::new)
}

/// Initializes the logger with the specified log level and callback function.
///
/// This function sets up the global logger with the given log level and callback.
/// It must be called before any logging can occur.
///
/// <div class="warning">
/// Calling this function multiple times will result in an error.
/// </div>
///
/// # Arguments
///
/// * `log_level` - The log level to capture. Events below this level will be filtered out.
/// * `callback` - The function to call for each log event that passes the level filter.
///
/// # Returns
///
/// Returns `None` on success, or a boxed `Error` if initialization fails.
#[no_mangle]
pub extern "C" fn ddog_log_init(log_level: LogLevel, callback: LogCallback) -> Option<Box<Error>> {
    logger_init(log_level, callback).err().map(Box::new)
}
