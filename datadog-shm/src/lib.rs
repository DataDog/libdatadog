// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Cross-platform named shared memory primitives for Datadog SDKs.
//!
//! This crate provides a thin, ergonomic layer on top of
//! [`datadog_ipc::platform::NamedShmHandle`] for creating, writing, and
//! reading named shared memory segments. It hides platform-specific
//! details:
//!
//! - **Unix (Linux)**: `memfd_create` or `shm_open` with `/tmp/libdatadog`
//!   fallback (e.g. AWS Lambda)
//! - **Unix (macOS)**: `shm_open` with reserve/commit pattern
//! - **Windows**: `CreateFileMapping` in the `Local\` namespace
//!
//! ## Intended use cases
//!
//! Any scenario where a Datadog SDK process needs to share a small blob
//! of data with related processes (children, sidecars, etc.) via a
//! well-known name:
//!
//! - Propagating session / instance identifiers across `fork`/`exec`
//! - Sharing configuration snapshots between tracer and sidecar
//! - Publishing lightweight status that other processes can poll
//!
//! ## Naming convention
//!
//! Callers supply a plain name (e.g. `"dd-session-12345"`). The name
//! **must** start with `/` (POSIX requirement); this crate will prepend
//! one if missing.  On Windows the name is automatically translated to
//! `Local\…` by the underlying `NamedShmHandle`.
//!
//! ## Example
//!
//! ```rust,no_run
//! use datadog_shm::{ShmWriter, ShmReader};
//!
//! // Writer — create a segment and keep it alive
//! let writer = ShmWriter::create("dd-session-42", b"hello world")?;
//!
//! // Reader — open and read (returns None if segment doesn't exist)
//! if let Some(reader) = ShmReader::open("dd-session-42")? {
//!     assert_eq!(reader.as_bytes(), b"hello world");
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use std::ffi::CString;

/// Default maximum segment size (64 KiB). Callers can override via
/// [`ShmWriter::create_with_capacity`].
pub const DEFAULT_MAX_SIZE: usize = 65_536;

// -- helpers ----------------------------------------------------------------

fn normalize_name(name: &str) -> anyhow::Result<CString> {
    let normalized = if name.starts_with('/') {
        name.to_owned()
    } else {
        format!("/{name}")
    };
    Ok(CString::new(normalized)?)
}

/// Platform-specific check for "segment does not exist" OS errors.
#[cfg(unix)]
fn is_not_found_error(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(raw) => raw == libc::ENOENT || raw == libc::ENOSYS || raw == libc::ENOTSUP,
        None => e.kind() == std::io::ErrorKind::NotFound,
    }
}

#[cfg(windows)]
fn is_not_found_error(e: &std::io::Error) -> bool {
    // ERROR_FILE_NOT_FOUND = 2
    e.raw_os_error() == Some(2) || e.kind() == std::io::ErrorKind::NotFound
}

// -- writer -----------------------------------------------------------------

/// A named shared memory segment open for writing.
///
/// The segment remains accessible to other processes for as long as the
/// `ShmWriter` is alive.  Dropping it unmaps (and on POSIX unlinks)
/// the segment.
pub struct ShmWriter {
    mapped: MappedMem<NamedShmHandle>,
    len: usize,
}

impl ShmWriter {
    /// Create a new named SHM segment containing `data`.
    ///
    /// The segment is sized to [`DEFAULT_MAX_SIZE`] or `data.len()`,
    /// whichever is larger. Use [`create_with_capacity`](Self::create_with_capacity)
    /// for explicit control.
    pub fn create(name: &str, data: &[u8]) -> anyhow::Result<Self> {
        let cap = DEFAULT_MAX_SIZE.max(data.len());
        Self::create_with_capacity(name, data, cap)
    }

    /// Create a new named SHM segment with an explicit capacity.
    ///
    /// `capacity` must be `>= data.len()`.
    pub fn create_with_capacity(
        name: &str,
        data: &[u8],
        capacity: usize,
    ) -> anyhow::Result<Self> {
        if data.len() > capacity {
            anyhow::bail!(
                "data length ({}) exceeds capacity ({})",
                data.len(),
                capacity
            );
        }

        let cname = normalize_name(name)?;
        let handle = NamedShmHandle::create(cname, capacity)?;
        let mut mapped = handle.map()?;

        let buf = mapped.as_slice_mut();
        buf[..data.len()].copy_from_slice(data);
        // Zero the remainder so readers can detect end-of-data
        for byte in &mut buf[data.len()..] {
            *byte = 0;
        }

        Ok(Self {
            mapped,
            len: data.len(),
        })
    }

    /// Overwrite the segment contents with new data.
    ///
    /// Fails if `data` is larger than the segment capacity.
    pub fn update(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let cap = self.mapped.get_size();
        if data.len() > cap {
            anyhow::bail!(
                "data length ({}) exceeds segment capacity ({})",
                data.len(),
                cap
            );
        }

        let buf = self.mapped.as_slice_mut();
        buf[..data.len()].copy_from_slice(data);
        for byte in &mut buf[data.len()..] {
            *byte = 0;
        }
        self.len = data.len();
        Ok(())
    }

    /// The number of live data bytes currently written.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the segment is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The total capacity of the segment.
    pub fn capacity(&self) -> usize {
        self.mapped.get_size()
    }
}

// -- reader -----------------------------------------------------------------

/// A read-only view of a named shared memory segment.
///
/// Dropping the reader unmaps the segment from this process, but the
/// segment itself remains alive as long as the writer holds it open.
pub struct ShmReader {
    mapped: MappedMem<NamedShmHandle>,
}

impl ShmReader {
    /// Open an existing named SHM segment for reading.
    ///
    /// Returns `Ok(None)` if no segment with that name exists.
    pub fn open(name: &str) -> anyhow::Result<Option<Self>> {
        let cname = normalize_name(name)?;
        let handle = match NamedShmHandle::open(&cname) {
            Ok(h) => h,
            Err(e) if is_not_found_error(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mapped = handle.map()?;
        Ok(Some(Self { mapped }))
    }

    /// The raw bytes of the entire mapped region.
    ///
    /// The region may contain trailing zero bytes beyond the actual
    /// payload. Use [`data_bytes`](Self::data_bytes) if you want
    /// only the non-zero prefix.
    pub fn as_bytes(&self) -> &[u8] {
        self.mapped.as_slice()
    }

    /// The non-zero prefix of the mapped region.
    ///
    /// Assumes the writer zero-filled the remainder after the payload.
    pub fn data_bytes(&self) -> &[u8] {
        let slice = self.mapped.as_slice();
        let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
        &slice[..end]
    }

    /// Total mapped size.
    pub fn mapped_size(&self) -> usize {
        self.mapped.get_size()
    }
}

// -- convenience: PID-keyed helpers -----------------------------------------

/// Create a SHM segment keyed by a PID with a well-known prefix.
///
/// The segment name will be `/dd-<prefix>-<pid>`.
pub fn create_pid_keyed(prefix: &str, pid: u32, data: &[u8]) -> anyhow::Result<ShmWriter> {
    let name = format!("/dd-{prefix}-{pid}");
    ShmWriter::create(&name, data)
}

/// Open a SHM segment keyed by a PID.
pub fn open_pid_keyed(prefix: &str, pid: u32) -> anyhow::Result<Option<ShmReader>> {
    let name = format!("/dd-{prefix}-{pid}");
    ShmReader::open(&name)
}

/// Return the current process ID.
pub fn current_pid() -> u32 {
    std::process::id()
}

/// Return the parent process ID.
#[cfg(unix)]
pub fn parent_pid() -> u32 {
    unsafe { libc::getppid() as u32 }
}

/// Return the parent process ID.
#[cfg(windows)]
pub fn parent_pid() -> u32 {
    use winapi::um::processthreadsapi::GetCurrentProcessId;
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };
    unsafe {
        let our_pid = GetCurrentProcessId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == winapi::um::handleapi::INVALID_HANDLE_VALUE {
            return 0;
        }
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snap, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == our_pid {
                    winapi::um::handleapi::CloseHandle(snap);
                    return entry.th32ParentProcessID;
                }
                if Process32Next(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        winapi::um::handleapi::CloseHandle(snap);
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn writer_reader_roundtrip() {
        let data = b"hello shared memory";
        let _writer =
            ShmWriter::create("dd-test-roundtrip-1", data).expect("create");

        let reader =
            ShmReader::open("dd-test-roundtrip-1").expect("open").expect("should exist");
        assert_eq!(reader.data_bytes(), data);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn writer_update() {
        let _writer = ShmWriter::create("dd-test-update-1", b"first")
            .expect("create");

        // Re-bind as mutable
        let mut writer = _writer;
        writer.update(b"second-value").expect("update");

        let reader =
            ShmReader::open("dd-test-update-1").expect("open").expect("should exist");
        assert_eq!(reader.data_bytes(), b"second-value");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn open_nonexistent_returns_none() {
        let result = ShmReader::open("dd-test-nonexistent-9999999")
            .expect("should not error");
        assert!(result.is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn pid_keyed_roundtrip() {
        let fake_pid = 9_800_001u32;
        let data = b"{\"session_id\":\"abc-123\"}";

        let _writer = create_pid_keyed("session", fake_pid, data)
            .expect("create pid-keyed");

        let reader = open_pid_keyed("session", fake_pid)
            .expect("open pid-keyed")
            .expect("should exist");
        assert_eq!(reader.data_bytes(), data.as_slice());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn custom_capacity() {
        let data = b"small";
        let writer = ShmWriter::create_with_capacity("dd-test-cap-1", data, 128)
            .expect("create");
        assert!(writer.capacity() >= 128);
        assert_eq!(writer.len(), 5);
    }

    #[test]
    fn data_exceeding_capacity_fails() {
        let big = vec![0xFFu8; 200];
        let result = ShmWriter::create_with_capacity("dd-test-toobig", &big, 100);
        assert!(result.is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn empty_data_roundtrip() {
        let _writer =
            ShmWriter::create("dd-test-empty-1", b"").expect("create");

        let reader =
            ShmReader::open("dd-test-empty-1").expect("open").expect("should exist");
        assert!(reader.data_bytes().is_empty());
    }
}
