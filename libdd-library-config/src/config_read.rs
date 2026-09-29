// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::io;
use alloc::vec::Vec;

/// Maximum allowed config file size (100 MB).
pub const MAX_CONFIG_FILE_SIZE: usize = 100 * 1024 * 1024;

/// Trait for reading configuration files from a filesystem or virtual filesystem.
///
/// Implement this to provide custom file access for environments where `std::fs`
/// is not available (e.g. no_std, sandboxed, or in-memory configurations).
pub trait ConfigRead {
    /// Read the entire contents of the configuration file at `path`, like `std::fs::read`.
    ///
    /// A missing file must be reported as [`io::ErrorKind::NotFound`]; the configuration layer is
    /// then treated as empty. Implementations **should** return [`io::ErrorKind::FileTooLarge`] for
    /// files exceeding [`MAX_CONFIG_FILE_SIZE`] to avoid unnecessary allocations; the
    /// configurator also checks the returned bytes as a safety net. Both are skipped with a
    /// debug log, while any other error aborts config loading.
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;
}

/// Standard filesystem implementation of [`ConfigRead`].
#[cfg(feature = "std")]
pub struct StdConfigRead;

#[cfg(feature = "std")]
impl ConfigRead for StdConfigRead {
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        // Compare as u64 so 32-bit targets can't truncate an oversized length.
        if len > MAX_CONFIG_FILE_SIZE as u64 {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        // `len` is bounded by MAX_CONFIG_FILE_SIZE, so it fits in usize.
        let mut buf = Vec::with_capacity(len as usize);
        std::io::Read::read_to_end(&mut &file, &mut buf)?;
        Ok(buf)
    }
}
