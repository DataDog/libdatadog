// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

const ETC_MACHINE_ID: &str = "/etc/machine-id";
const DBUS_MACHINE_ID: &str = "/var/lib/dbus/machine-id";

/// Read and trim the contents of `path`, returning `None` on any I/O error or
/// if the resulting string is empty.
fn read_id(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Return the machine ID from the given paths.
///
/// Tries `etc_path` first (mirrors `/etc/machine-id`), falls back to
/// `dbus_path` (mirrors `/var/lib/dbus/machine-id`). Returns an empty
/// `String` when both are unavailable, matching the Go agent's behaviour.
///
/// Accepts explicit paths so tests can inject temporary files without needing
/// a feature flag.
pub fn get_machine_id_impl_paths(etc_path: &Path, dbus_path: &Path) -> String {
    read_id(etc_path)
        .or_else(|| read_id(dbus_path))
        .unwrap_or_default()
}

/// Return the machine ID using the standard system paths.
pub fn get_machine_id_impl() -> String {
    get_machine_id_impl_paths(Path::new(ETC_MACHINE_ID), Path::new(DBUS_MACHINE_ID))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn prefers_etc_machine_id() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc_machine_id");
        let dbus = dir.path().join("dbus_machine_id");
        std::fs::write(&etc, "aabbccdd\n").unwrap();
        std::fs::write(&dbus, "11223344\n").unwrap();
        assert_eq!(get_machine_id_impl_paths(&etc, &dbus), "aabbccdd");
    }

    #[test]
    fn falls_back_to_dbus_when_etc_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dbus = dir.path().join("dbus_machine_id");
        std::fs::write(&dbus, "11223344\n").unwrap();
        assert_eq!(
            get_machine_id_impl_paths(Path::new("/nonexistent_etc_mid"), &dbus),
            "11223344"
        );
    }

    #[test]
    fn both_missing_returns_empty() {
        assert_eq!(
            get_machine_id_impl_paths(
                Path::new("/nonexistent_etc_mid"),
                Path::new("/nonexistent_dbus_mid"),
            ),
            ""
        );
    }

    #[test]
    fn trims_whitespace_and_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc_machine_id");
        std::fs::write(&etc, "  deadbeef  \n").unwrap();
        assert_eq!(
            get_machine_id_impl_paths(&etc, Path::new("/nonexistent_dbus_mid")),
            "deadbeef"
        );
    }

    #[test]
    fn empty_file_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc_machine_id");
        let dbus = dir.path().join("dbus_machine_id");
        // etc exists but is whitespace-only; should fall back to dbus
        let mut f = std::fs::File::create(&etc).unwrap();
        f.write_all(b"   \n").unwrap();
        std::fs::write(&dbus, "fallback_id").unwrap();
        assert_eq!(get_machine_id_impl_paths(&etc, &dbus), "fallback_id");
    }
}
