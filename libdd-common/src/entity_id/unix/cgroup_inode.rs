// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module provides functions to fetch cgroup node path and fetching it's inode
use super::super::discovery;
#[cfg(test)]
use super::CgroupFileParsingError;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::{fs, io};

/// Returns the inode of file at `path`
fn get_inode(path: &Path) -> io::Result<u64> {
    let meta = fs::metadata(path)?;
    Ok(meta.ino())
}

/// Returns the cgroup mount path associated with `cgroup_v1_base_controller` or the default one for
/// cgroupV2
#[cfg(test)]
fn get_cgroup_node_path(
    cgroup_v1_base_controller: &str,
    cgroup_path: &Path,
    cgroup_mount_path: &Path,
) -> Result<PathBuf, CgroupFileParsingError> {
    let file = fs::File::open(cgroup_path).map_err(|_| CgroupFileParsingError::CannotOpenFile)?;
    let cgroup = io::read_to_string(file).map_err(|_| CgroupFileParsingError::InvalidFormat)?;
    discovery::cgroup_node_path(
        &cgroup,
        cgroup_v1_base_controller,
        cgroup_mount_path
            .to_str()
            .ok_or(CgroupFileParsingError::InvalidFormat)?,
    )
    .map(PathBuf::from)
    .ok_or(CgroupFileParsingError::CgroupNotFound)
}

/// Returns the `cgroup_inode` if available, otherwise `None`
pub fn get_cgroup_inode(
    cgroup_v1_base_controller: &str,
    cgroup_path: &Path,
    cgroup_mount_path: &Path,
) -> Option<String> {
    let cgroup = fs::read_to_string(cgroup_path).ok()?;
    discovery::cgroup_inode(
        &cgroup,
        cgroup_v1_base_controller,
        cgroup_mount_path.to_str()?,
        |path| {
            // Preserve the existing std tests' namespace mock. The alloc-only tests exercise
            // the real namespace guard with caller-supplied inode values.
            #[cfg(test)]
            if path == "/proc/self/ns/cgroup" {
                return Some(0);
            }
            get_inode(Path::new(path)).ok()
        },
    )
    .map(|inode| inode.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::CGROUP_V1_BASE_CONTROLLER;
    use super::*;
    use maplit::hashmap;

    #[test]
    fn test_cgroup_node_path_parsing() {
        let test_root_dir: &Path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"));

        let test_files = hashmap! {
            // parsing standard cgroupV2 file
            "cgroup.v2" => Ok("/sys/fs/cgroup"),
            // parsing cgroupV2 file with custom path
            "cgroup.v2_custom_path" => Ok("/sys/fs/cgroup/custom/path"),
            // a cgroupv1 container cgroup file returns the memory controller path
            "cgroup.docker" => Ok("/sys/fs/cgroup/memory/docker/9d5b23edb1ba181e8910389a99906598d69ac9a0ead109ee55730cc416d95f7f"),
            // a non-container Linux cgroup file returns the memory controller path
            "cgroup.linux" => Ok("/sys/fs/cgroup/memory/user.slice/user-0.slice/session-14.scope"),
            // a cgroupV1 file with an entry using 0 as a hierarchy id should not be detected as V2
            "cgroup.v1_with_id_0" => Ok("/sys/fs/cgroup/memory/user.slice/user-0.slice/session-14.scope"),
            // a cgroupV1 file using multiple controllers in the same entry returns the correct path
            "cgroup.multiple_controllers" => Ok("/sys/fs/cgroup/memory/user.slice/user-0.slice/session-14.scope"),
            // a cgroupV1 file missing the memory controller should return an error
            "cgroup.no_memory" => Err(CgroupFileParsingError::CgroupNotFound),
            // missing cgroup file should return a CannotOpenFile Error
            "path/to/cgroup.missing" => Err(CgroupFileParsingError::CannotOpenFile),
            // valid container ID with invalid line pattern makes an empty string
            "cgroup.invalid_line_container_id" => Err(CgroupFileParsingError::CgroupNotFound),
        };

        for (&filename, expected_result) in test_files.iter() {
            assert_eq!(
                get_cgroup_node_path(
                    CGROUP_V1_BASE_CONTROLLER,
                    &test_root_dir.join(filename),
                    Path::new("/sys/fs/cgroup")
                ),
                expected_result.clone().map(PathBuf::from),
                "testing file parsing for cgroup node path with file: {filename}"
            );
        }
    }
}
