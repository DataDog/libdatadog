// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module provides functions to parse the container id from the cgroup file
use super::CgroupFileParsingError;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

fn is_lowercase_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

/// Try to match `[0-9a-f]{64}` at the end of `s`.
fn try_match_hex64(s: &str) -> Option<&str> {
    if s.len() < 64 {
        return None;
    }

    let candidate = &s[s.len() - 64..];
    candidate.bytes().all(is_lowercase_hex).then_some(candidate)
}

/// Try to match a UUID `[0-9a-f]{8}[-_][0-9a-f]{4}[-_]..[-_][0-9a-f]{12}` (36 chars) at the end.
fn try_match_uuid(s: &str) -> Option<&str> {
    if s.len() < 36 {
        return None;
    }

    let candidate = &s[s.len() - 36..];
    const TEMPLATE: &[u8; 36] = b"hhhhhhhh-hhhh-hhhh-hhhh-hhhhhhhhhhhh";
    candidate
        .as_bytes()
        .iter()
        .zip(TEMPLATE)
        .all(|(&c, &t)| match t {
            b'h' => is_lowercase_hex(c),
            b'-' => matches!(c, b'-' | b'_'),
            _ => false,
        })
        .then_some(candidate)
}

/// Try to match `[0-9a-f]{32}-\d+` at the end of `s`.
fn try_match_task_id(s: &str) -> Option<&str> {
    let (prefix, digits) = s.rsplit_once('-')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || prefix.len() < 32 {
        return None;
    }

    let hex_start = prefix.len() - 32;
    prefix[hex_start..]
        .bytes()
        .all(is_lowercase_hex)
        .then_some(&s[hex_start..])
}

/// Extract a container ID from a cgroup path, matching the pattern
/// `(UUID|HEX64|TASK_ID)(?:.scope)? *$`
pub(super) fn extract_container_id_from_path(path: &str) -> Option<&str> {
    let path = {
        let trimmed = path.trim_end();
        trimmed.strip_suffix(".scope").unwrap_or(trimmed)
    };

    try_match_hex64(path)
        .or_else(|| try_match_uuid(path))
        .or_else(|| try_match_task_id(path))
}

/// Parse a cgroup line (`^\d+:[^:]*:(.+)$`) and extract a container ID from the path component.
fn parse_line(line: &str) -> Option<&str> {
    let mut parts = line.splitn(3, ':');
    let hierarchy_id = parts.next()?;
    let _controllers = parts.next()?;
    let path = parts.next()?;

    if hierarchy_id.is_empty()
        || !hierarchy_id.bytes().all(|b| b.is_ascii_digit())
        || path.is_empty()
    {
        return None;
    }
    extract_container_id_from_path(path)
}

/// Extract container id contained in the cgroup file located at `cgroup_path`
pub fn extract_container_id(cgroup_path: &Path) -> Result<String, CgroupFileParsingError> {
    let file = File::open(cgroup_path).map_err(|_| CgroupFileParsingError::CannotOpenFile)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::hashmap;

    #[cfg_attr(miri, ignore)]
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
