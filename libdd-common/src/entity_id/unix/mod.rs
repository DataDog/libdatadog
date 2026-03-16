// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error;
use std::fmt;
use std::path::Path;
use std::sync::LazyLock;

mod cgroup_inode;
mod container_id;

const DEFAULT_CGROUP_PATH: &str = "/proc/self/cgroup";
const DEFAULT_CGROUP_MOUNT_PATH: &str = "/sys/fs/cgroup";

/// stores overridable cgroup path - used in end-to-end testing to "stub" cgroup values
#[cfg(feature = "cgroup_testing")]
static TESTING_CGROUP_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// the base controller used to identify the cgroup v1 mount point in the cgroupMounts map.
const CGROUP_V1_BASE_CONTROLLER: &str = "memory";

#[derive(Debug, Clone, PartialEq)]
pub enum CgroupFileParsingError {
    ContainerIdNotFound,
    CgroupNotFound,
    CannotOpenFile,
    InvalidFormat,
}

impl fmt::Display for CgroupFileParsingError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CgroupFileParsingError::ContainerIdNotFound => write!(f, "Container id not found"),
            CgroupFileParsingError::CgroupNotFound => write!(f, "Cgroup not found"),
            CgroupFileParsingError::CannotOpenFile => {
                write!(f, "Error while opening cgroup file")
            }
            CgroupFileParsingError::InvalidFormat => write!(f, "Invalid format in cgroup file"),
        }
    }
}

impl error::Error for CgroupFileParsingError {}

fn compute_entity_id(
    base_controller: &str,
    cgroup_path: &Path,
    cgroup_mount_path: &Path,
) -> Option<String> {
    container_id::extract_container_id(cgroup_path)
        .ok()
        .map(|container_id| format!("ci-{container_id}"))
        .or(
            cgroup_inode::get_cgroup_inode(base_controller, cgroup_path, cgroup_mount_path)
                .map(|inode| format!("in-{inode}")),
        )
}

/// Set cgroup mount path to mock during tests
#[cfg(feature = "cgroup_testing")]
pub fn set_cgroup_file(path: String) {
    let _ = TESTING_CGROUP_PATH.set(path);
}

fn get_cgroup_path() -> &'static str {
    #[cfg(feature = "cgroup_testing")]
    return TESTING_CGROUP_PATH
        .get()
        .map(std::ops::Deref::deref)
        .unwrap_or(DEFAULT_CGROUP_PATH);
    #[cfg(not(feature = "cgroup_testing"))]
    return DEFAULT_CGROUP_PATH;
}

fn get_cgroup_mount_path() -> &'static str {
    DEFAULT_CGROUP_MOUNT_PATH
}

/// The `container_id` if available in the cgroup file, otherwise returns `None`. Value is cached to
/// avoid recomputing it at each call.
pub static CONTAINER_ID: LazyLock<Option<&'static str>> = LazyLock::new(|| {
    // cache container id in a static to avoid recomputing it at each call
    container_id::extract_container_id(Path::new(get_cgroup_path()))
        .ok()
        .map(|s| {
            let leaked_str: &'static str = Box::leak(s.into_boxed_str());
            leaked_str
        })
});

/// The `entity_id` if available, either `cid-<container_id>` or `in-<cgroup_inode>`
pub static ENTITY_ID: LazyLock<Option<&'static str>> = LazyLock::new(|| {
    compute_entity_id(
        CGROUP_V1_BASE_CONTROLLER,
        Path::new(get_cgroup_path()),
        Path::new(get_cgroup_mount_path()),
    )
    .map(|s| {
        let leaked_str: &'static str = Box::leak(s.into_boxed_str());
        leaked_str
    })
});

#[cfg(test)]
mod tests {
    use super::*;
    use regex_lite::Regex;

    static IN_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"in-\d+").unwrap());
    static CI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"ci-{}", container_id::CONTAINER_REGEX.as_str())).unwrap()
    });

    /// The following test can only be run in isolation because of caching behaviour
    fn test_entity_id(filename: &str, expected_result: Option<&Regex>) {
        let test_root_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"));

        let entity_id = compute_entity_id(
            CGROUP_V1_BASE_CONTROLLER,
            test_root_dir.join(filename).as_path(),
            test_root_dir.join("cgroup").as_path(),
        );

        if let Some(regex) = expected_result {
            assert!(
                regex.is_match(entity_id.as_deref().unwrap()),
                "testing get_entity_id with file {}: {} is not matching the expected regex",
                filename,
                entity_id.as_deref().unwrap_or("None")
            );
        } else {
            assert_eq!(
                None, entity_id,
                "testing get_entity_id with file {filename}"
            );
        }
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_entity_id_for_v2() {
        test_entity_id("cgroup.v2", Some(&*IN_REGEX))
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_entity_id_for_v1() {
        test_entity_id("cgroup.linux", Some(&*IN_REGEX))
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_entity_id_for_container_id() {
        test_entity_id("cgroup.docker", Some(&*CI_REGEX))
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_entity_id_for_no_id() {
        test_entity_id("cgroup.no_memory", None)
    }
}
