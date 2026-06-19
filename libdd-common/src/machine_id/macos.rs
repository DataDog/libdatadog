// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Return the platform UUID via the `gethostuuid(3)` BSD syscall.
///
/// This returns the same 128-bit value that `IOPlatformUUID` exposes via
/// IOKit (`ioreg -rd1 -c IOPlatformExpertDevice`), which is what gopsutil
/// (and therefore the Go agent) returns on macOS.  Using the syscall avoids a
/// fork+exec of `ioreg`.
///
/// The UUID is formatted as uppercase hex with hyphens:
/// `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`
///
/// Returns an empty `String` on syscall failure, matching the Go agent's
/// silent-empty behaviour.
pub fn get_machine_id_impl() -> String {
    let mut uuid: [u8; 16] = [0u8; 16];
    // Passing a zero timespec requests an indefinite wait; in practice the
    // call returns immediately (the UUID is available after very early boot).
    let wait = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &wait) };
    if rc != 0 {
        return String::new();
    }
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        uuid[0], uuid[1], uuid[2], uuid[3],
        uuid[4], uuid[5],
        uuid[6], uuid[7],
        uuid[8], uuid[9],
        uuid[10], uuid[11], uuid[12], uuid[13], uuid[14], uuid[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_nonempty_uuid() {
        // On any real macOS host (including CI) the host UUID is always set.
        let id = get_machine_id_impl();
        assert!(!id.is_empty(), "expected a non-empty UUID on macOS");
        // Basic shape check: 36 chars, hyphens at positions 8, 13, 18, 23.
        assert_eq!(id.len(), 36);
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[13..14], "-");
        assert_eq!(&id[18..19], "-");
        assert_eq!(&id[23..24], "-");
    }
}
