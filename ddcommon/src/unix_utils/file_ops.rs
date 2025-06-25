// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fs::{File, OpenOptions};
use std::os::fd::{IntoRawFd, RawFd};

/// Opens a file for writing (in append mode) or opens /dev/null
/// * If the filename is provided, it will try to open (creating if needed) the specified file.
///   Failure to do so is an error.
/// * If the filename is not provided, it will open /dev/null Some systems can fail to provide
///   `/dev/null` (e.g., chroot jails), so this failure is also an error.
/// * Using Stdio::null() is more direct, but it will cause a panic in environments where /dev/null
///   is not available.
pub fn open_file_or_quiet(filename: Option<&str>) -> std::io::Result<RawFd> {
    let file = filename.map_or_else(
        || File::open("/dev/null"),
        |f| OpenOptions::new().append(true).create(true).open(f),
    )?;
    Ok(file.into_raw_fd())
}
