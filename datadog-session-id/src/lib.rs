// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Cross-platform shared memory carrier for Datadog session IDs.
//!
//! This crate provides a thin abstraction over named shared memory
//! (POSIX `shm_open` on Unix, `CreateFileMapping` on Windows) to
//! propagate stable session identifiers from a parent process to its
//! children across `fork`/`exec` boundaries.
//!
//! ## Wire format
//!
//! The shared memory region contains a JSON payload:
//!
//! ```json
//! {
//!   "version": 1,
//!   "session_id": "<uuid-string>",
//!   "parent_session_id": "<uuid-string-or-null>"
//! }
//! ```
//!
//! ## Discovery
//!
//! The SHM segment is created under a well-known name derived from the
//! **creating process's PID**:
//!
//! - Unix: `/dd-session-<pid>`  (via `shm_open` or `/tmp/libdatadog` fallback)
//! - Windows: `Local\dd-session-<pid>` (via `CreateFileMapping`)
//!
//! A child process discovers the parent's segment by opening
//! `/dd-session-<ppid>`.

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use serde::{Deserialize, Serialize};
use std::ffi::CString;

const SHM_NAME_PREFIX: &str = "/dd-session-";

/// Current wire format version.
const WIRE_VERSION: u32 = 1;

/// Maximum size for the session payload. 4 KiB is more than enough for two
/// UUIDs plus a version field serialized as JSON.
const MAX_PAYLOAD_SIZE: usize = 4096;

/// The payload written to and read from shared memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPayload {
    pub version: u32,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

/// An opaque handle to the shared memory segment that holds session data.
/// The segment stays alive (and readable by children) for as long as this
/// handle is not dropped.
pub struct SessionCarrier {
    _mapped: MappedMem<NamedShmHandle>,
}

/// Create a new session carrier for the **current process**.
///
/// Writes `session_id` (and optionally `parent_session_id`) into a named
/// shared memory segment discoverable by child processes via
/// [`read_parent_session`].
///
/// The caller **must** keep the returned [`SessionCarrier`] alive for as
/// long as child processes may need to read the session data.
pub fn create_session_carrier(
    session_id: &str,
    parent_session_id: Option<&str>,
) -> anyhow::Result<SessionCarrier> {
    create_session_carrier_for_pid(session_id, parent_session_id, current_pid())
}

/// Create a session carrier associated with a specific PID.
///
/// This is the same as [`create_session_carrier`] but allows the caller
/// to control which PID the segment is keyed under. Useful for testing
/// or for advanced scenarios where the publishing PID differs from the
/// current process.
pub fn create_session_carrier_for_pid(
    session_id: &str,
    parent_session_id: Option<&str>,
    pid: u32,
) -> anyhow::Result<SessionCarrier> {
    let payload = SessionPayload {
        version: WIRE_VERSION,
        session_id: session_id.to_owned(),
        parent_session_id: parent_session_id.map(|s| s.to_owned()),
    };

    let data = serde_json::to_vec(&payload)?;
    if data.len() > MAX_PAYLOAD_SIZE {
        anyhow::bail!(
            "Session payload too large: {} bytes (max {})",
            data.len(),
            MAX_PAYLOAD_SIZE
        );
    }

    let shm_name = shm_name_for_pid(pid)?;
    let handle = NamedShmHandle::create(shm_name, MAX_PAYLOAD_SIZE)?;
    let mut mapped = handle.map()?;

    let buf = mapped.as_slice_mut();
    buf[..data.len()].copy_from_slice(&data);
    // Zero the rest so readers can find the end of JSON
    for byte in &mut buf[data.len()..] {
        *byte = 0;
    }

    Ok(SessionCarrier { _mapped: mapped })
}

/// Read session data published by the **parent process**.
///
/// Opens the shared memory segment created by the parent (using the
/// parent's PID for discovery) and deserializes the session payload.
///
/// Returns `Ok(None)` if the parent did not publish session data (segment
/// does not exist).
pub fn read_parent_session() -> anyhow::Result<Option<SessionPayload>> {
    let ppid = parent_pid();
    read_session_for_pid(ppid)
}

/// Read session data published by a specific process.
pub fn read_session_for_pid(pid: u32) -> anyhow::Result<Option<SessionPayload>> {
    let shm_name = shm_name_for_pid(pid)?;
    let handle = match NamedShmHandle::open(&shm_name) {
        Ok(h) => h,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // On some platforms, permission errors or ENOENT variants differ
        Err(e) => {
            // Treat "does not exist" style errors as None
            let raw = e.raw_os_error().unwrap_or(0);
            if is_not_found_error(raw) {
                return Ok(None);
            }
            return Err(e.into());
        }
    };

    let mapped = handle.map()?;
    let slice = mapped.as_slice();

    // Find the end of the JSON data (first null byte or end of region)
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    if end == 0 {
        return Ok(None);
    }

    let payload: SessionPayload = serde_json::from_slice(&slice[..end])?;
    Ok(Some(payload))
}

fn shm_name_for_pid(pid: u32) -> anyhow::Result<CString> {
    let name = format!("{SHM_NAME_PREFIX}{pid}");
    Ok(CString::new(name)?)
}

fn current_pid() -> u32 {
    std::process::id()
}

#[cfg(unix)]
fn parent_pid() -> u32 {
    unsafe { libc::getppid() as u32 }
}

#[cfg(windows)]
fn parent_pid() -> u32 {
    // On Windows we need the Windows API to get the parent PID.
    // Using the `winapi` crate already in the dependency tree via datadog-ipc.
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

/// Platform-specific check for "not found" OS error codes.
#[cfg(unix)]
fn is_not_found_error(raw: i32) -> bool {
    raw == libc::ENOENT || raw == libc::ENOSYS || raw == libc::ENOTSUP
}

#[cfg(windows)]
fn is_not_found_error(raw: i32) -> bool {
    // ERROR_FILE_NOT_FOUND = 2
    raw == 2
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use distinct fake PIDs so tests don't collide when run in parallel.
    // These PIDs are high enough to be very unlikely to exist.
    const TEST_PID_1: u32 = 9_900_001;
    const TEST_PID_2: u32 = 9_900_002;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn roundtrip_session_carrier() {
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let parent_id = "660e8400-e29b-41d4-a716-446655440001";

        let _carrier = create_session_carrier_for_pid(
            session_id,
            Some(parent_id),
            TEST_PID_1,
        )
        .expect("create carrier");

        let payload = read_session_for_pid(TEST_PID_1).expect("read session");

        let payload = payload.expect("payload should exist");
        assert_eq!(payload.version, WIRE_VERSION);
        assert_eq!(payload.session_id, session_id);
        assert_eq!(
            payload.parent_session_id.as_deref(),
            Some(parent_id)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn roundtrip_no_parent() {
        let session_id = "770e8400-e29b-41d4-a716-446655440002";

        let _carrier = create_session_carrier_for_pid(
            session_id,
            None,
            TEST_PID_2,
        )
        .expect("create carrier");

        let payload = read_session_for_pid(TEST_PID_2).expect("read session");

        let payload = payload.expect("payload should exist");
        assert_eq!(payload.session_id, session_id);
        assert!(payload.parent_session_id.is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn read_nonexistent_pid_returns_none() {
        // PID 1 is init/systemd — very unlikely to have our SHM segment
        let result = read_session_for_pid(999_999_999).expect("should not error");
        assert!(result.is_none());
    }

    #[test]
    fn payload_serialization() {
        let payload = SessionPayload {
            version: 1,
            session_id: "abc-123".to_string(),
            parent_session_id: Some("def-456".to_string()),
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        let back: SessionPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(payload, back);
    }
}
