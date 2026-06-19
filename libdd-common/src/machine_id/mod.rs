// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Host machine identifier, equivalent to `pkg/util/uuid.GetUUID()` in the
//! Go agent.
//!
//! The value is read once at first access, cached for the lifetime of the
//! process, and never replaced with a random UUID on failure.  An empty
//! string is the correct fallback — the backend can detect a missing value
//! but cannot detect an incorrect one.
//!
//! # Per-platform source
//!
//! | Platform | Source |
//! |----------|--------|
//! | Linux | `/etc/machine-id` (preferred), fallback `/var/lib/dbus/machine-id` |
//! | macOS | `gethostuuid(3)` — same value as `IOPlatformUUID` |
//! | Windows | `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` |
//! | Other   | `""` (matches Go agent failure behaviour) |

use std::sync::LazyLock;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
mod windows;

/// Cached host machine ID, populated on first access.
static MACHINE_ID: LazyLock<String> = LazyLock::new(|| {
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
});

/// Returns the host machine ID, cached for the process lifetime.
///
/// Returns `""` on failure or on unsupported platforms (matches Go agent
/// behaviour — an empty string is preferable to a synthetic random UUID).
pub fn get_machine_id() -> &'static str {
    MACHINE_ID.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two successive calls must return the identical pointer/value (the
    /// LazyLock must be stable).
    #[test]
    fn cached_value_is_stable() {
        let a = get_machine_id();
        let b = get_machine_id();
        assert_eq!(a, b);
    }

    /// The cached value must not contain leading/trailing whitespace or
    /// newlines (each platform implementation is responsible for trimming,
    /// but we assert it here as a cross-platform contract).
    #[test]
    fn value_is_trimmed() {
        let id = get_machine_id();
        assert_eq!(id, id.trim());
    }
}
