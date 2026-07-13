// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module provides functions to parse the container id from the cgroup file
use super::CgroupFileParsingError;
use crate::entity_id::parse::parse_container_id;
use std::fs;
use std::path::Path;

/// Extract container id contained in the cgroup file located at `cgroup_path`
pub fn extract_container_id(cgroup_path: &Path) -> Result<String, CgroupFileParsingError> {
    let content =
        fs::read_to_string(cgroup_path).map_err(|_| CgroupFileParsingError::CannotOpenFile)?;
    parse_container_id(&content)
        .map(String::from)
        .ok_or(CgroupFileParsingError::ContainerIdNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::hashmap;

    #[cfg_attr(miri, ignore)]
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
            "cgroup.pcf" => Some("6f265890-5165-7fab-6b52-18d1"),

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
}
