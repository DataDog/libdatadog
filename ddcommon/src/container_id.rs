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
pub use fallback::*;

#[cfg(unix)]
pub use unix::*;

/// Fallback module used for non-unix systems
#[cfg(not(unix))]
mod fallback {
    /// # Safety
    /// Marked as unsafe to match the signature of the unix version
    pub unsafe fn set_cgroup_file(_file: String) {}

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
    use regex::Regex;
    use std::error;
    use std::fmt;
    use std::fs;
    use std::fs::File;
    use std::io;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    const DEFAULT_CGROUP_PATH: &str = "/proc/self/cgroup";
    const DEFAULT_CGROUP_MOUNT_PATH: &str = "/sys/fs/cgroup";

    /// the base controller used to identify the cgroup v1 mount point in the cgroupMounts map.
    const CGROUP_V1_BASE_CONTROLLER: &str = "memory";

    // Those two variables are unused in tests
    #[cfg(not(test))]
    // From https://github.com/torvalds/linux/blob/5859a2b1991101d6b978f3feb5325dad39421f29/include/linux/proc_ns.h#L41-L49
    // Currently, host namespace inode number are hardcoded, which can be used to detect
    // if we're running in host namespace or not (does not work when running in DinD)
    const HOST_CGROUP_NAMESPACE_INODE: u64 = 0xEFFFFFFB;

    #[cfg(not(test))]
    const DEFAULT_CGROUP_NS_PATH: &str = "/proc/self/ns/cgroup";

    /// stores overridable cgroup path - used in end-to-end testing to "stub" cgroup values
    static mut TESTING_CGROUP_PATH: Option<String> = None;

    /// stores overridable cgroup mount path - used in end-to-end to mock cgroup node and be able to
    /// compute inode
    static mut TESTING_CGROUP_MOUNT_PATH: Option<String> = None;

    const UUID_SOURCE: &str =
        r"[0-9a-f]{8}[-_][0-9a-f]{4}[-_][0-9a-f]{4}[-_][0-9a-f]{4}[-_][0-9a-f]{12}";
    const CONTAINER_SOURCE: &str = r"[0-9a-f]{64}";
    const TASK_SOURCE: &str = r"[0-9a-f]{32}-\d+";

    lazy_static! {
        static ref LINE_REGEX: Regex = Regex::new(r"^\d+:[^:]*:(.+)$").unwrap();
        static ref CONTAINER_REGEX: Regex = Regex::new(&format!(
            r"({UUID_SOURCE}|{CONTAINER_SOURCE}|{TASK_SOURCE})(?:.scope)? *$"
        ))
        .unwrap();
    }

    #[derive(Debug, Clone, PartialEq)]
    enum CgroupFileParsingError {
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

    fn parse_line(line: &str) -> Option<&str> {
        // unwrap is OK since if regex matches then the groups must exist
        LINE_REGEX
            .captures(line)
            .and_then(|captures| CONTAINER_REGEX.captures(captures.get(1).unwrap().as_str()))
            .map(|captures| captures.get(1).unwrap().as_str())
    }

    fn extract_container_id(filepath: &Path) -> Result<String, CgroupFileParsingError> {
        let file = File::open(filepath).map_err(|_| CgroupFileParsingError::CannotOpenFile)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            if let Some(container_id) =
                parse_line(&line.map_err(|_| CgroupFileParsingError::InvalidFormat)?)
            {
                return Ok(String::from(container_id));
            }
        }

        Err(CgroupFileParsingError::ContainerIdNotFound)
    }

    /// Returns the inode of file at `path`
    fn get_inode(path: &Path) -> io::Result<u64> {
        let meta = fs::metadata(path)?;
        Ok(meta.ino())
    }

    /// Returns the cgroup mount path associated with `base_controller` or the default one for
    /// cgroupV2
    fn get_cgroup_node_path(
        base_controller: &str,
        cgroup_path: &Path,
    ) -> Result<PathBuf, CgroupFileParsingError> {
        let file = File::open(cgroup_path).map_err(|_| CgroupFileParsingError::CannotOpenFile)?;
        let reader = BufReader::new(file);

        let mut node_path: Option<PathBuf> = None;

        for (index, line) in reader.lines().enumerate() {
            let line_content = &line.map_err(|_| CgroupFileParsingError::InvalidFormat)?;
            let cgroup_entry: Vec<&str> = line_content.split(':').collect();
            if cgroup_entry.len() != 3 {
                return Err(CgroupFileParsingError::InvalidFormat);
            }
            let controllers: Vec<&str> = cgroup_entry[1].split(',').collect();
            // Only keep empty controller if it is the first line as cgroupV2 uses only one line
            if controllers.contains(&base_controller) || (controllers.contains(&"") && index == 0) {
                let matched_operator = if controllers.contains(&base_controller) {
                    base_controller
                } else {
                    ""
                };

                let mut path = get_cgroup_mount_path();
                path.push(matched_operator);
                path.push(cgroup_entry[2].strip_prefix('/').unwrap_or(cgroup_entry[2])); // Remove first / as the path is relative
                node_path = Some(path);

                // if we are using cgroupV1 we can stop looking for the controller
                if index != 0 {
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
        let cgroup_namespace_inode =
            get_inode(Path::new(DEFAULT_CGROUP_NS_PATH)).map_err(|_| ())?;
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
    fn get_cgroup_inode() -> Option<&'static str> {
        lazy_static! {
            static ref CGROUP_INODE: Option<String> = {
                // If we're running in the host cgroup namespace, do not get the inode.
                // This would indicate that we're not in a container and the inode we'd
                // return is not related to a container.
                is_host_cgroup_namespace().ok()?;
                let cgroup_mount_path =
                    get_cgroup_node_path(CGROUP_V1_BASE_CONTROLLER, get_cgroup_path().as_path())
                        .ok()?;
                Some(get_inode(&cgroup_mount_path).ok()?.to_string())
            };
        }
        CGROUP_INODE.as_deref()
    }

    /// # Safety
    /// Must not be called in multi-threaded contexts
    pub unsafe fn set_cgroup_file(file: String) {
        TESTING_CGROUP_PATH = Some(file)
    }

    fn get_cgroup_path() -> PathBuf {
        // Safety: we assume set_cgroup_file is not called when it shouldn't
        if let Some(path) = unsafe { TESTING_CGROUP_PATH.as_ref() } {
            Path::new(path.as_str()).into()
        } else {
            Path::new(DEFAULT_CGROUP_PATH).into()
        }
    }

    /// # Safety
    /// Must not be called in multi-threaded contexts
    pub unsafe fn set_cgroup_mount_path(file: String) {
        TESTING_CGROUP_MOUNT_PATH = Some(file)
    }

    fn get_cgroup_mount_path() -> PathBuf {
        // Safety: we assume set_cgroup_file is not called when it shouldn't
        if let Some(path) = unsafe { TESTING_CGROUP_MOUNT_PATH.as_ref() } {
            Path::new(path.as_str()).into()
        } else {
            Path::new(DEFAULT_CGROUP_MOUNT_PATH).into()
        }
    }

    /// Returns the `container_id` if available in the cgroup file, otherwise returns `None`
    pub fn get_container_id() -> Option<&'static str> {
        // cache container id in a static to avoid recomputing it at each call

        lazy_static! {
            static ref CONTAINER_ID: Option<String> =
                extract_container_id(get_cgroup_path().as_path()).ok();
        }
        CONTAINER_ID.as_deref()
    }

    /// Returns the `entity id` either `cid-<container_id>` if available or `in-<cgroup_inode>`
    pub fn get_entity_id() -> Option<&'static str> {
        lazy_static! {
            static ref ENTITY_ID: Option<String> = get_container_id()
                .map(|container_id| format!("cid-{container_id}"))
                .or(get_cgroup_inode().map(|inode| format!("in-{inode}")));
        }
        ENTITY_ID.as_deref()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use maplit::hashmap;

        #[test]
        fn test_container_id_line_parsing() {
            let test_lines = hashmap! {
                "" => None,
                "other_line" => None,
                "10:hugetlb:/kubepods/burstable/podfd52ef25-a87d-11e9-9423-0800271a638e/8c046cb0b72cd4c99f51b5591cd5b095967f58ee003710a45280c28ee1a9c7fa"
                    => Some("8c046cb0b72cd4c99f51b5591cd5b095967f58ee003710a45280c28ee1a9c7fa"),
                "11:devices:/kubepods.slice/kubepods-pod97f1ae73_7ad9_11ec_b4a7_9a35488b4fab.slice/3291bfddf3f3f8d87cb0cd1245fe9c45b2e1e5a9b6fe3de1bddf041aedaecbab"
                    => Some("3291bfddf3f3f8d87cb0cd1245fe9c45b2e1e5a9b6fe3de1bddf041aedaecbab"),
                "11:hugetlb:/ecs/55091c13-b8cf-4801-b527-f4601742204d/432624d2150b349fe35ba397284dea788c2bf66b885d14dfc1569b01890ca7da"
                    => Some("432624d2150b349fe35ba397284dea788c2bf66b885d14dfc1569b01890ca7da"),
                "1:name=systemd:/docker/34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376"
                    => Some("34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376"),
                "1:name=systemd:/uuid/34dc0b5e-626f-2c5c-4c51-70e34b10e765"
                    => Some("34dc0b5e-626f-2c5c-4c51-70e34b10e765"),
                "1:name=systemd:/ecs/34dc0b5e626f2c5c4c5170e34b10e765-1234567890"
                    => Some("34dc0b5e626f2c5c4c5170e34b10e765-1234567890"),
                "1:name=systemd:/docker/34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376.scope"
                    => Some("34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376"),
                // k8s with additional characters before ID
                "1:name=systemd:/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod2d3da189_6407_48e3_9ab6_78188d75e609.slice/docker-7b8952daecf4c0e44bbcefe1b5c5ebc7b4839d4eefeccefe694709d3809b6199.scope"
                    => Some("7b8952daecf4c0e44bbcefe1b5c5ebc7b4839d4eefeccefe694709d3809b6199"),
                // extra spaces
                "13:name=systemd:/docker/3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f869860    "
                    => Some("3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f869860"),
                // one char too short
                "13:name=systemd:/docker/3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f86986"
                    => None,
                // invalid hex
                "13:name=systemd:/docker/3726184226f5d3147g25fdeab5b60097e378e8a720503a5e19ecfdf29f869860"
                    => None,
            };
            for (line, &expected_result) in test_lines.iter() {
                assert_eq!(
                    parse_line(line),
                    expected_result,
                    "testing line parsing for container id with line: {line}"
                );
            }
        }

        #[test]
        fn test_container_id_file_parsing() {
            let test_root_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"));

            let test_files = hashmap! {
                // parse a Docker container ID"
                "cgroup.docker" => Some("9d5b23edb1ba181e8910389a99906598d69ac9a0ead109ee55730cc416d95f7f"),
                // parse a Kubernetes container ID
                "cgroup.kubernetes" => Some("3e74d3fd9db4c9dd921ae05c2502fb984d0cde1b36e581b13f79c639da4518a1"),
                // parse an ECS container ID
                "cgroup.ecs" => Some("38fac3e99302b3622be089dd41e7ccf38aff368a86cc339972075136ee2710ce"),
                // parse a Fargate container ID
                "cgroup.fargate" => Some("432624d2150b349fe35ba397284dea788c2bf66b885d14dfc1569b01890ca7da"),
                // parse a Fargate 1.4+ container ID
                "cgroup.fargate.1.4" => Some("8cd79a803caf4d2aa945152e934a5c00-1053176469"),

                // Whitespace around the matching ID is permitted so long as it is matched within a valid cgroup line.
                // parse a container ID with leading and trailing whitespace
                "cgroup.whitespace" => Some("3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f869860"),

                // a non-container Linux cgroup file makes an empty string
                "cgroup.linux" => None,

                // missing cgroup file should return None
                "/path/to/cgroup.missing" => None,

                /* To be consistent with other tracers, unrecognized services that match the
                * generic container ID regex patterns are considered valid.
                */
                //parse unrecognized container ID
                "cgroup.unrecognized" => Some("9d5b23edb1ba181e8910389a99906598d69ac9a0ead109ee55730cc416d95f7f"),

                // error edge cases when parsing container ID
                "cgroup.edge_cases" => None,

                // an empty cgroup file makes an empty string
                "" => None,

                // valid container ID with invalid line pattern makes an empty string
                "cgroup.invalid_line_container_id" => None,

                // valid task ID with invalid line pattern makes an empty string
                "cgroup.invalid_line_task_id" => None,

                // To be consistent with other tracers we only match lower case hex
                // uppercase container IDs return an empty string
                "cgroup.upper" => None,
            };

            for (&filename, &expected_result) in test_files.iter() {
                assert_eq!(
                    extract_container_id(&test_root_dir.join(filename)).ok(),
                    expected_result.map(String::from),
                    "testing file parsing for container id with file: {filename}"
                );
            }
        }

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
                "cgroup.invalid_line_container_id" => Err(CgroupFileParsingError::InvalidFormat),
            };

            for (&filename, expected_result) in test_files.iter() {
                assert_eq!(
                    get_cgroup_node_path(CGROUP_V1_BASE_CONTROLLER, &test_root_dir.join(filename)),
                    expected_result.clone().map(PathBuf::from),
                    "testing file parsing for cgroup node path with file: {filename}"
                );
            }
        }

        lazy_static! {
            static ref IN_REGEX: Regex = Regex::new(r"in-\d+").unwrap();
            static ref CID_REGEX: Regex =
                Regex::new(&format!(r"cid-{}", CONTAINER_REGEX.as_str())).unwrap();
        }

        /// The following test can only be run in isolation because of caching behaviour introduced
        /// by lazy_static
        fn test_entity_id(filename: &str, expected_result: Option<&Regex>) {
            let test_root_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"));
            unsafe {
                set_cgroup_mount_path(
                    test_root_dir
                        .join("cgroup")
                        .as_path()
                        .to_str()
                        .expect("Invalid test directory")
                        .to_owned(),
                );
            }
            unsafe {
                set_cgroup_file(
                    test_root_dir
                        .join(filename)
                        .as_path()
                        .to_str()
                        .expect("Invalid test directory")
                        .to_owned(),
                );
            }

            if let Some(regex) = expected_result {
                assert!(
                    regex.is_match(get_entity_id().unwrap()),
                    "testing get_entity_id with file {}: {} is not matching the expected regex",
                    filename,
                    get_entity_id().unwrap_or("None")
                );
            } else {
                assert_eq!(
                    None,
                    get_entity_id(),
                    "testing get_entity_id with file {filename}"
                );
            }
        }

        #[test]
        #[ignore]
        fn test_entity_id_for_v2() {
            test_entity_id("cgroup.v2", Some(&IN_REGEX))
        }

        #[test]
        #[ignore]
        fn test_entity_id_for_v1() {
            test_entity_id("cgroup.linux", Some(&IN_REGEX))
        }

        #[test]
        #[ignore]
        fn test_entity_id_for_container_id() {
            test_entity_id("cgroup.docker", Some(&CID_REGEX))
        }

        #[test]
        #[ignore]
        fn test_entity_id_for_no_id() {
            test_entity_id("cgroup.no_memory", None)
        }
    }
}
