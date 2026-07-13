// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Target-agnostic parsing and composition for entity headers.
//!
//! The Unix code path reads `/proc/self/cgroup` and `DD_EXTERNAL_ENV` itself;
//! wasm callers (which can't read `/proc` or `process.env`) inject the same raw
//! ingredients through [`crate::entity_id::init_entity_inputs`]. Both paths
//! funnel through the helpers in this module so parsing, composition and
//! validation live in one place.

#[cfg(any(unix, target_arch = "wasm32"))]
use crate::regex_engine::Regex;
#[cfg(any(unix, target_arch = "wasm32"))]
use std::sync::LazyLock;

#[cfg(any(unix, target_arch = "wasm32"))]
const UUID_SOURCE: &str =
    r"[0-9a-f]{8}[-_][0-9a-f]{4}[-_][0-9a-f]{4}[-_][0-9a-f]{4}[-_][0-9a-f]{12}";
/// PCF / Garden container UUID source: 8-4-4-4-4 hex (28 chars).
/// Distinct from `UUID_SOURCE` (8-4-4-4-12) because the last group is 4 hex.
#[cfg(any(unix, target_arch = "wasm32"))]
const PCF_UUID_SOURCE: &str = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}";
#[cfg(any(unix, target_arch = "wasm32"))]
const CONTAINER_SOURCE: &str = r"[0-9a-f]{64}";
#[cfg(any(unix, target_arch = "wasm32"))]
const TASK_SOURCE: &str = r"[0-9a-f]{32}-\d+";

#[cfg(any(unix, target_arch = "wasm32"))]
pub(crate) static LINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(r"^\d+:[^:]*:(.+)$").unwrap()
});

#[cfg(any(unix, target_arch = "wasm32"))]
pub(crate) static CONTAINER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(&format!(
        r"({UUID_SOURCE}|{PCF_UUID_SOURCE}|{CONTAINER_SOURCE}|{TASK_SOURCE})(?:\.scope(?:/[^/ \t]+)?)? *$"
    ))
    .unwrap()
});

/// Extract a container id from a single `/proc/self/cgroup` line.
#[cfg(any(unix, target_arch = "wasm32"))]
pub(crate) fn parse_container_id_line(line: &str) -> Option<&str> {
    // unwrap is OK since if regex matches then the groups must exist
    #[allow(clippy::unwrap_used)]
    LINE_REGEX
        .captures(line)
        .and_then(|captures| CONTAINER_REGEX.captures(captures.get(1).unwrap().as_str()))
        .map(|captures| captures.get(1).unwrap().as_str())
}

/// Extract a container id from the full contents of `/proc/self/cgroup`.
///
/// Returns the first matching id, or `None` if no line yields a container id.
#[cfg(any(unix, target_arch = "wasm32"))]
pub(crate) fn parse_container_id(cgroup_content: &str) -> Option<&str> {
    cgroup_content.lines().find_map(parse_container_id_line)
}

/// Compose an entity id from the ingredients extracted from `/proc/self/cgroup`
/// and its inode: `ci-<container_id>` when a container id is found, else
/// `in-<cgroup_inode>`.
#[cfg(any(unix, target_arch = "wasm32"))]
pub(crate) fn compose_entity_id(
    container_id: Option<&str>,
    cgroup_inode: Option<u64>,
) -> Option<String> {
    container_id
        .map(|id| format!("ci-{id}"))
        .or_else(|| cgroup_inode.map(|inode| format!("in-{inode}")))
}

/// Validate a `DD_EXTERNAL_ENV` value as safe to emit as an HTTP header.
///
/// Accepts printable ASCII plus tab. Rejects CR/LF (header injection /
/// request-smuggling vector), other control bytes, and non-latin1. Returns
/// `None` for empty or invalid values so callers can drop them uniformly.
pub(crate) fn sanitize_external_env(raw: &str) -> Option<&str> {
    if raw.is_empty() {
        return None;
    }
    if raw
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7E).contains(&b))
    {
        Some(raw)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(unix, target_arch = "wasm32"))]
    use maplit::hashmap;

    #[cfg(any(unix, target_arch = "wasm32"))]
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
            // Podman cgroup v2: libpod-HEXID.scope/container (cgroupns=host)
            "0::/machine.slice/libpod-93afc7bc3ce42ad052d2926ffacfba941803bfae080941d1e1375d9d46b6a281.scope/container"
                => Some("93afc7bc3ce42ad052d2926ffacfba941803bfae080941d1e1375d9d46b6a281"),
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
            // PCF Garden 8-4-4-4-4 UUID
            "1:name=systemd:/system.slice/garden.service/garden/6f265890-5165-7fab-6b52-18d1"
                => Some("6f265890-5165-7fab-6b52-18d1"),
            "10:freezer:/garden/6f265890-5165-7fab-6b52-18d1"
                => Some("6f265890-5165-7fab-6b52-18d1"),
            // Regression guard: 8-4-4-4-12 standard UUID must NOT be truncated to 28 chars
            "1:name=systemd:/uuid/5a081c13-b8cf-4801-b427-f4601742204d"
                => Some("5a081c13-b8cf-4801-b427-f4601742204d"),
            // First group only 7 chars -> no match
            "1:name=systemd:/garden/6f26589-5165-7fab-6b52-18d1"
                => None,
        };
        for (line, &expected_result) in test_lines.iter() {
            assert_eq!(
                parse_container_id_line(line),
                expected_result,
                "testing line parsing for container id with line: {line}"
            );
        }
    }

    #[cfg(any(unix, target_arch = "wasm32"))]
    #[test]
    fn test_parse_container_id_from_multiline_content() {
        // Multi-line cgroup file; first matching line wins.
        let content = "\
0::/user.slice
1:name=systemd:/docker/34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376
";
        assert_eq!(
            parse_container_id(content),
            Some("34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376")
        );

        // No container id present.
        assert_eq!(parse_container_id("0::/user.slice\n"), None);
        assert_eq!(parse_container_id(""), None);
    }

    #[cfg(any(unix, target_arch = "wasm32"))]
    #[test]
    fn test_compose_entity_id() {
        assert_eq!(
            compose_entity_id(Some("abc123"), None),
            Some("ci-abc123".to_string())
        );
        // container id wins over inode when both present
        assert_eq!(
            compose_entity_id(Some("abc123"), Some(42)),
            Some("ci-abc123".to_string())
        );
        assert_eq!(compose_entity_id(None, Some(42)), Some("in-42".to_string()));
        assert_eq!(compose_entity_id(None, None), None);
    }

    #[test]
    fn test_sanitize_external_env() {
        assert_eq!(sanitize_external_env("prod"), Some("prod"));
        assert_eq!(
            sanitize_external_env("cn-1,e-app,it-false"),
            Some("cn-1,e-app,it-false")
        );
        assert_eq!(sanitize_external_env("with\ttab"), Some("with\ttab"));

        // Rejected: control bytes and non-ASCII.
        assert_eq!(sanitize_external_env(""), None);
        assert_eq!(sanitize_external_env("with\nnewline"), None);
        assert_eq!(sanitize_external_env("with\rcarriage"), None);
        assert_eq!(sanitize_external_env("foo\r\nInjected: 1"), None);
        assert_eq!(sanitize_external_env("nul\0byte"), None);
        assert_eq!(sanitize_external_env("café"), None);
    }
}
