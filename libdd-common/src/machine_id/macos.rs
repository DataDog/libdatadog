// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! macOS host machine id via `gethostuuid(3)`, returning `IOPlatformUUID`.

/// Returns `IOPlatformUUID` via `gethostuuid(3)`, which avoids a fork+exec of `ioreg`.
pub fn get_machine_id_impl() -> String {
    let mut uuid = [0u8; 16];
    // Zero timeout: the host UUID is static, so there's nothing to wait for.
    let wait = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &wait) };
    if rc != 0 {
        return String::new();
    }
    // Assemble the 16 raw bytes into the canonical 8-4-4-4-12 hyphenated UUID.
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
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
        let id = get_machine_id_impl();
        assert!(!id.is_empty());
        assert_eq!(id.len(), 36);
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[13..14], "-");
        assert_eq!(&id[18..19], "-");
        assert_eq!(&id[23..24], "-");
    }
}
