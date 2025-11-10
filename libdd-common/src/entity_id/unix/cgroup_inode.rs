// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module provides functions to fetch cgroup node path and fetching it's inode
use super::CgroupFileParsingError;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::{fs, io};

// Those two variables are unused in tests
#[cfg(not(test))]
// From https://github.com/torvalds/linux/blob/5859a2b1991101d6b978f3feb5325dad39421f29/include/linux/proc_ns.h#L41-L49
// Currently, host namespace inode number are hardcoded, which can be used to detect
// if we're running in host namespace or not (does not work when running in Docker in Docker)
const HOST_CGROUP_NAMESPACE_INODE: u64 = 0xEFFFFFFB;

#[cfg(not(test))]
const DEFAULT_CGROUP_NS_PATH: &str = "/proc/self/ns/cgroup";

/// Returns the inode of file at `path`
fn get_inode(path: &Path) -> io::Result<u64> {
    let meta = fs::metadata(path)?;
    Ok(meta.ino())
}

/// Returns the cgroup mount path associated with `cgroup_v1_base_controller` or the default one for
/// cgroupV2
fn get_cgroup_node_path(
    cgroup_v1_base_controller: &str,
    cgroup_path: &Path,
    cgroup_mount_path: &Path,
) -> Result<PathBuf, CgroupFileParsingError> {
    let file = File::open(cgroup_path).map_err(|_| CgroupFileParsingError::CannotOpenFile)?;
    let reader = BufReader::new(file);

    let mut node_path: Option<PathBuf> = None;

    for line in reader.lines() {
        let line_content = &line.map_err(|_| CgroupFileParsingError::InvalidFormat)?;
        let cgroup_entry: Vec<&str> = line_content.split(':').collect();
        if cgroup_entry.len() != 3 {
            continue;
        }
        let controllers: Vec<&str> = cgroup_entry[1].split(',').collect();
        // Only keep empty controller if it is the first line as cgroupV2 uses only one line
        if controllers.contains(&cgroup_v1_base_controller) || controllers.contains(&"") {
            let matched_operator = if controllers.contains(&cgroup_v1_base_controller) {
                cgroup_v1_base_controller
            } else {
                ""
            };

            let mut path = cgroup_mount_path.join(matched_operator);
            path.push(cgroup_entry[2].strip_prefix('/').unwrap_or(cgroup_entry[2])); // Remove first / as the path is relative
            node_path = Some(path);

            // if we matched the V1 base controller we can return otherwise we continue until we
            // find it or default to the empty controller name
            if matched_operator == cgroup_v1_base_controller {
                break;
            }
        }
    }
    node_path.ok_or(CgroupFileParsingError::CgroupNotFound)
}

#[cfg(not(test))]
/// Checks if the agent is running in the host cgroup namespace.
/// This check is disabled when testing
fn is_host_cgroup_namespace() -> Result<(), ()> {
    let cgroup_namespace_inode = get_inode(Path::new(DEFAULT_CGROUP_NS_PATH)).map_err(|_| ())?;
    if cgroup_namespace_inode == HOST_CGROUP_NAMESPACE_INODE {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
/// Mock version used in tests
fn is_host_cgroup_namespace() -> Result<(), ()> {
    Ok(())
}

/// Returns the `cgroup_inode` if available, otherwise `None`
pub fn get_cgroup_inode(
    cgroup_v1_base_controller: &str,
    cgroup_path: &Path,
    cgroup_mount_path: &Path,
) -> Option<String> {
    // If we're running in the host cgroup namespace, do not get the inode.
    // This would indicate that we're not in a container and the inode we'd
    // return is not related to a container.
    is_host_cgroup_namespace().ok()?;
    let cgroup_mount_path =
        get_cgroup_node_path(cgroup_v1_base_controller, cgroup_path, cgroup_mount_path).ok()?;
    Some(get_inode(&cgroup_mount_path).ok()?.to_string())
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
