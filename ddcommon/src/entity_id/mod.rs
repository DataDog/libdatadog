// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Extract the entity id and container id
//!
//! The container id can be extracted from `/proc/self/group`
//!
//! The entity id is either:
//! - `cid:<container id>` if available
//! - `in:<cgroup node inode>` if container id is not available (e.g. when using cgroupV2)
//!
//! # References
//! - [DataDog/dd-trace-go](https://github.com/DataDog/dd-trace-go/blob/v1/internal/container.go)
//! - [Qard/container-info](https://github.com/Qard/container-info/blob/master/index.js)
//! # Supported environments
//! ## Docker
//! /proc/self/cgroup should contain lines like:
//! ```text
//! 13:name=systemd:/docker/3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f869860
//! ```
//! ## Kubernetes
//! /proc/self/cgroup should contain lines like:
//! ```text
//! 11:perf_event:/kubepods/besteffort/pod3d274242-8ee0-11e9-a8a6-1e68d864ef1a/3e74d3fd9db4c9dd921ae05c2502fb984d0cde1b36e581b13f79c639da4518a1
//! ```
//!
//! Possibly with extra characters before id:
//! ```text
//! 1:name=systemd:/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod2d3da189_6407_48e3_9ab6_78188d75e609.slice/docker-7b8952daecf4c0e44bbcefe1b5c5ebc7b4839d4eefeccefe694709d3809b6199.scope
//! ```
//!
//! Or a UUID:
//! ```text
//! 1:name=systemd:/kubepods/besteffort/pode9b90526-f47d-11e8-b2a5-080027b9f4fb/15aa6e53-b09a-40c7-8558-c6c31e36c88a
//! ```
//! ## ECS
//! /proc/self/cgroup should contain lines like:
//! ```text
//! 9:perf_event:/ecs/haissam-ecs-classic/5a0d5ceddf6c44c1928d367a815d890f/38fac3e99302b3622be089dd41e7ccf38aff368a86cc339972075136ee2710ce
//! ```
//! ## Fargate 1.3-:
//! /proc/self/cgroup should contain lines like:
//! ```test
//! 11:hugetlb:/ecs/55091c13-b8cf-4801-b527-f4601742204d/432624d2150b349fe35ba397284dea788c2bf66b885d14dfc1569b01890ca7da
//! ```
//! ## Fargate 1.4+:
//! Here we match a task id with a suffix
//! ```test
//! 1:name=systemd:/ecs/8cd79a803caf4d2aa945152e934a5c00/8cd79a803caf4d2aa945152e934a5c00-1053176469
//! ```

#[cfg(not(unix))]
pub use fallback::{get_container_id, get_entity_id};

#[cfg(unix)]
pub use unix::{get_container_id, get_entity_id};

/// Fallback module used for non-unix systems
#[cfg(not(unix))]
mod fallback {
    pub fn get_container_id() -> Option<&'static str> {
        None
    }

    pub fn get_entity_id() -> Option<&'static str> {
        None
    }
}

/// Unix specific module allowing the use of unix specific functions
#[cfg(unix)]
mod unix {
    use lazy_static::lazy_static;
    use std::error;
    use std::fmt;
    use std::path::Path;

    mod cgroup_inode;
    mod container_id;

    const DEFAULT_CGROUP_PATH: &str = "/proc/self/cgroup";
    const DEFAULT_CGROUP_MOUNT_PATH: &str = "/sys/fs/cgroup";

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
            .map(|container_id| format!("cid-{container_id}"))
            .or(
                cgroup_inode::get_cgroup_inode(base_controller, cgroup_path, cgroup_mount_path)
                    .map(|inode| format!("in-{inode}")),
            )
    }

    /// Returns the `container_id` if available in the cgroup file, otherwise returns `None`
    pub fn get_container_id() -> Option<&'static str> {
        // cache container id in a static to avoid recomputing it at each call

        lazy_static! {
            static ref CONTAINER_ID: Option<String> =
                container_id::extract_container_id(Path::new(DEFAULT_CGROUP_PATH)).ok();
        }
        CONTAINER_ID.as_deref()
    }

    /// Returns the `entity_id` if available, either `cid-<container_id>` or `in-<cgroup_inode>`
    pub fn get_entity_id() -> Option<&'static str> {
        lazy_static! {
            static ref ENTITY_ID: Option<String> = compute_entity_id(
                CGROUP_V1_BASE_CONTROLLER,
                Path::new(DEFAULT_CGROUP_PATH),
                Path::new(DEFAULT_CGROUP_MOUNT_PATH),
            );
        }
        ENTITY_ID.as_deref()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use regex::Regex;

        lazy_static! {
            static ref IN_REGEX: Regex = Regex::new(r"in-\d+").unwrap();
            static ref CID_REGEX: Regex =
                Regex::new(&format!(r"cid-{}", container_id::CONTAINER_REGEX.as_str())).unwrap();
        }

        /// The following test can only be run in isolation because of caching behaviour introduced
        /// by lazy_static
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

        #[test]
        fn test_entity_id_for_v2() {
            test_entity_id("cgroup.v2", Some(&IN_REGEX))
        }

        #[test]
        fn test_entity_id_for_v1() {
            test_entity_id("cgroup.linux", Some(&IN_REGEX))
        }

        #[test]
        fn test_entity_id_for_container_id() {
            test_entity_id("cgroup.docker", Some(&CID_REGEX))
        }

        #[test]
        fn test_entity_id_for_no_id() {
            test_entity_id("cgroup.no_memory", None)
        }
    }
}
