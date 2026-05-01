// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::vec::Vec;

/// Maximum allowed config file size (100 MB).
pub const MAX_CONFIG_FILE_SIZE: usize = 100 * 1024 * 1024;

/// Error returned by [`ConfigRead::read`].
///
/// This enum classifies all failure modes so the configurator can decide which
/// are fatal (abort) and which are gracefully skipped:
///
/// - [`NotFound`](Self::NotFound) — the file does not exist; the configuration layer is simply
///   absent and will be treated as empty.
/// - [`TooLarge`](Self::TooLarge) — the file exceeds [`MAX_CONFIG_FILE_SIZE`]; skipped with a debug
///   log.
/// - [`Io`](Self::Io) — any other I/O or access error; aborts config loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigReadError<E: core::fmt::Display + core::fmt::Debug> {
    /// File does not exist at the given path.
    #[error("file not found")]
    NotFound,
    /// File exceeds [`MAX_CONFIG_FILE_SIZE`].
    #[error("file is too large (> 100mb)")]
    TooLarge,
    /// An I/O or platform-specific error.
    #[error("{0}")]
    Io(E),
}

/// Trait for reading configuration files from a filesystem or virtual filesystem.
///
/// Implement this to provide custom file access for environments where `std::fs`
/// is not available (e.g. no_std, sandboxed, or in-memory configurations).
pub trait ConfigRead {
    /// The platform-specific error type carried by [`ConfigReadError::Io`].
    type IoError: core::fmt::Display + core::fmt::Debug;

    /// Read the entire contents of the configuration file at `path`.
    ///
    /// Implementations **should** return [`ConfigReadError::TooLarge`] for files
    /// exceeding [`MAX_CONFIG_FILE_SIZE`] to avoid unnecessary allocations.
    /// The configurator also checks the returned bytes as a safety net.
    fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>>;
}

/// Standard filesystem implementation of [`ConfigRead`].
#[cfg(feature = "std")]
pub struct StdConfigRead;

#[cfg(feature = "std")]
impl ConfigRead for StdConfigRead {
    type IoError = std::io::Error;

    fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<std::io::Error>> {
        use std::{fs, io};

        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ConfigReadError::NotFound),
            Err(e) => return Err(ConfigReadError::Io(e)),
        };
        // Compare as u64 first so 32-bit targets can't truncate an oversized length down
        // into the allowed range.
        let len = match file.metadata() {
            Ok(m) if m.len() > MAX_CONFIG_FILE_SIZE as u64 => {
                return Err(ConfigReadError::TooLarge)
            }
            Ok(m) => m.len() as usize,
            Err(e) => return Err(ConfigReadError::Io(e)),
        };
        let mut buf = Vec::with_capacity(len);
        io::Read::read_to_end(&mut &file, &mut buf).map_err(ConfigReadError::Io)?;
        // TOCTOU: the file may have grown between metadata() and read_to_end().
        if buf.len() > MAX_CONFIG_FILE_SIZE {
            return Err(ConfigReadError::TooLarge);
        }
        Ok(buf)
    }
}
