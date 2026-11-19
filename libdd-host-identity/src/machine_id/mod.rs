// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Host machine identifier, mirroring `pkg/util/uuid.GetUUID()` in the Go agent.
//!
//! | Platform | Source |
//! |----------|--------|
//! | Linux    | `/sys/class/dmi/id/product_uuid` then `/etc/machine-id` → `/proc/sys/kernel/random/boot_id` |
//! | macOS    | `gethostuuid(3)` |
//! | Windows  | `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` |
//! | Other    | `""` |
//!
//! All values are normalised to lowercase `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
//! Returns `""` on failure rather than a random UUID — the backend can detect
//! a missing value but not a wrong one.

use std::sync::LazyLock;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
mod windows;

/// Normalise a raw OS machine-id to a lowercase hyphenated UUID string.
/// Strips hyphens, filters to hex digits, lowercases, then re-inserts hyphens.
/// Returns `""` if the result is not exactly 32 hex digits.
pub(crate) fn normalize_uuid(raw: &str) -> String {
    let hex: String = raw
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect();

    if hex.len() != 32 {
        return String::new();
    }

    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32],
    )
}

static MACHINE_ID: LazyLock<String> = LazyLock::new(|| {
    let raw = {
        #[cfg(target_os = "linux")]
        {
            linux::get_machine_id_impl()
        }
        #[cfg(target_os = "macos")]
        {
            macos::get_machine_id_impl()
        }
        #[cfg(windows)]
        {
            windows::get_machine_id_impl()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            String::new()
        }
    };
    normalize_uuid(&raw)
});

/// Returns the host machine ID as a lowercase hyphenated UUID, cached for the process lifetime.
/// Returns `""` on failure or unsupported platforms.
pub fn get_machine_id() -> &'static str {
    MACHINE_ID.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_value_is_stable() {
        assert_eq!(get_machine_id(), get_machine_id());
    }

    #[test]
    fn value_has_uuid_shape_if_nonempty() {
        let id = get_machine_id();
        if id.is_empty() {
            return;
        }
        assert_eq!(id.len(), 36);
        for (i, c) in id.chars().enumerate() {
            if [8, 13, 18, 23].contains(&i) {
                assert_eq!(c, '-');
            } else {
                assert!(c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
            }
        }
    }

    #[test]
    fn normalize_bare_hex_inserts_hyphens() {
        assert_eq!(
            normalize_uuid("b08fa8a2b01a4d2bbd95fec7e30c5aec"),
            "b08fa8a2-b01a-4d2b-bd95-fec7e30c5aec"
        );
    }

    #[test]
    fn normalize_uppercase_uuid_lowercased() {
        assert_eq!(
            normalize_uuid("B08FA8A2-B01A-4D2B-BD95-FEC7E30C5AEC"),
            "b08fa8a2-b01a-4d2b-bd95-fec7e30c5aec"
        );
    }

    #[test]
    fn normalize_lowercase_uuid_unchanged() {
        assert_eq!(
            normalize_uuid("b08fa8a2-b01a-4d2b-bd95-fec7e30c5aec"),
            "b08fa8a2-b01a-4d2b-bd95-fec7e30c5aec"
        );
    }

    #[test]
    fn normalize_invalid_returns_empty() {
        assert_eq!(normalize_uuid(""), "");
        assert_eq!(normalize_uuid("b08fa8a2"), "");
        assert_eq!(normalize_uuid("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"), "");
    }
}
