// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Linux host machine id, mirroring gopsutil's fallback order:
//! `/sys/class/dmi/id/product_uuid` (root-only, usually empty otherwise) =>
//! `/etc/machine-id` => `/proc/sys/kernel/random/boot_id`.
//!
//! Note that boot_id changes is re-generated boot time. This is regretable
//! but aligned with the agent

use std::path::Path;

fn read_trimmed(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolves the id from the three candidate paths in priority order
pub fn get_machine_id_impl_paths(dmi_path: &Path, etc_path: &Path, boot_path: &Path) -> String {
    if let Some(id) = read_trimmed(dmi_path) {
        return id;
    }
    // agent compatibility:
    // gopsutil only accepts /etc/machine-id when it's exactly 32 chars (bare hex)
    if let Some(id) = read_trimmed(etc_path) {
        if id.len() == 32 {
            return id;
        }
    }
    read_trimmed(boot_path).unwrap_or_default()
}

pub fn get_machine_id_impl() -> String {
    get_machine_id_impl_paths(
        Path::new("/sys/class/dmi/id/product_uuid"),
        Path::new("/etc/machine-id"),
        Path::new("/proc/sys/kernel/random/boot_id"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &[u8]) {
        std::fs::write(path, content).unwrap();
    }

    fn tmp_paths(
        dir: &tempfile::TempDir,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        (
            dir.path().join("product_uuid"),
            dir.path().join("machine_id"),
            dir.path().join("boot_id"),
        )
    }

    #[test]
    fn level1_dmi_wins_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let (dmi, etc, boot) = tmp_paths(&dir);
        write(&dmi, b"B08FA8A2-B01A-4D2B-BD95-FEC7E30C5AEC\n");
        write(&etc, b"aabbccddaabbccddaabbccddaabbccdd\n");
        write(&boot, b"cccccccccccccccccccccccccccccccc\n");
        assert_eq!(
            get_machine_id_impl_paths(&dmi, &etc, &boot),
            "B08FA8A2-B01A-4D2B-BD95-FEC7E30C5AEC"
        );
    }

    #[test]
    fn level2_etc_used_when_dmi_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (dmi, etc, boot) = tmp_paths(&dir);
        write(&etc, b"aabbccddaabbccddaabbccddaabbccdd\n");
        write(&boot, b"cccccccccccccccccccccccccccccccc\n");
        assert_eq!(
            get_machine_id_impl_paths(&dmi, &etc, &boot),
            "aabbccddaabbccddaabbccddaabbccdd"
        );
    }

    #[test]
    fn level2_skipped_when_etc_not_32_chars() {
        let dir = tempfile::tempdir().unwrap();
        let (dmi, etc, boot) = tmp_paths(&dir);
        write(&etc, b"aabbccdd-aabb-ccdd-aabb-ccddaabbccdd\n");
        write(&boot, b"dddddddddddddddddddddddddddddddd\n");
        assert_eq!(
            get_machine_id_impl_paths(&dmi, &etc, &boot),
            "dddddddddddddddddddddddddddddddd"
        );
    }

    #[test]
    fn level3_boot_id_as_last_resort() {
        let dir = tempfile::tempdir().unwrap();
        let (dmi, etc, boot) = tmp_paths(&dir);
        write(&boot, b"cccccccccccccccccccccccccccccccc\n");
        assert_eq!(
            get_machine_id_impl_paths(&dmi, &etc, &boot),
            "cccccccccccccccccccccccccccccccc"
        );
    }

    #[test]
    fn all_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (dmi, etc, boot) = tmp_paths(&dir);
        assert_eq!(get_machine_id_impl_paths(&dmi, &etc, &boot), "");
    }

    #[test]
    fn trims_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let (dmi, etc, boot) = tmp_paths(&dir);
        write(&etc, b"  aabbccddaabbccddaabbccddaabbccdd  \n");
        assert_eq!(
            get_machine_id_impl_paths(&dmi, &etc, &boot),
            "aabbccddaabbccddaabbccddaabbccdd"
        );
    }
}
