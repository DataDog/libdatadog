// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::string::String;
#[cfg(any(target_os = "linux", test))]
use alloc::{format, vec::Vec};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(any(target_os = "linux", test))]
const CGROUP_PATH: &str = "/proc/self/cgroup";
#[cfg(any(target_os = "linux", test))]
const CGROUP_MOUNT_PATH: &str = "/sys/fs/cgroup";
#[cfg(any(target_os = "linux", all(feature = "std", unix), test))]
const CGROUP_NAMESPACE_PATH: &str = "/proc/self/ns/cgroup";
// Linux's initial cgroup namespace inode (include/linux/proc_ns.h).
#[cfg(any(target_os = "linux", all(feature = "std", unix), test))]
const HOST_CGROUP_NAMESPACE_INODE: u64 = 0xEFFFFFFB;

// Private seam for fixture tests; production discovery always uses rustix on Linux.
#[cfg(any(target_os = "linux", test))]
trait OriginRead {
    type Error;

    /// Read a cgroup file's contents. Errors make cgroup discovery unavailable.
    fn read(&self, path: &str) -> Result<Vec<u8>, Self::Error>;

    /// Return the inode of `path`, following symlinks as `stat` does.
    /// In particular, `/proc/self/ns/cgroup` must resolve to the namespace inode.
    fn inode(&self, path: &str) -> Result<u64, Self::Error>;
}

// Fixture environment; production uses the shared shadow environment.
#[cfg(any(target_os = "linux", test))]
trait EnvRead {
    /// Return an owned UTF-8 value; missing or non-UTF-8 variables return `None`.
    fn get(&self, name: &str) -> Option<String>;
}

#[cfg(target_os = "linux")]
struct ProcessEnvironment;

#[cfg(target_os = "linux")]
impl EnvRead for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        crate::environment::var(name).ok()
    }
}

/// Origin metadata suitable for the Trace Agent's origin headers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EntityInfo {
    pub container_id: Option<String>,
    pub entity_id: Option<String>,
    pub external_env: Option<String>,
}

impl EntityInfo {
    /// Borrow the available `datadog-container-id`, `datadog-entity-id`, and
    /// `datadog-external-env` headers, in that order.
    pub fn headers(&self) -> impl Iterator<Item = (&'static str, &str)> {
        headers(
            self.container_id.as_deref(),
            self.entity_id.as_deref(),
            self.external_env.as_deref(),
        )
    }
}

/// Discover Linux origin metadata using rustix and the shared shadow environment.
///
/// Container IDs take precedence over cgroup inodes for the entity ID. The inode fallback
/// selects the v1 `memory` controller or the v2 unified hierarchy, and is disabled in the
/// host cgroup namespace. `DD_EXTERNAL_ENV` is included when nonempty; `DD_ENTITY_ID` is
/// not consulted. Values are owned and are not cached.
///
/// Like the cached `std` API, discovery is best effort: unavailable or invalid cgroup data
/// produces absent IDs, while environment discovery proceeds independently.
#[cfg(target_os = "linux")]
pub fn discover() -> EntityInfo {
    discover_with_reader(&linux::Procfs, &ProcessEnvironment)
}

/// Read the current Linux UTS namespace's hostname using rustix's `uname` syscall.
///
/// Does not consult the environment, DNS, or hostname files. Returns `ILSEQ` for invalid
/// UTF-8 and `INVAL` for an empty hostname.
#[cfg(target_os = "linux")]
pub fn read_hostname() -> rustix::io::Result<String> {
    let uname = rustix::system::uname();
    let hostname = uname
        .nodename()
        .to_str()
        .map_err(|_| rustix::io::Errno::ILSEQ)?;
    if hostname.is_empty() {
        return Err(rustix::io::Errno::INVAL);
    }
    Ok(String::from(hostname))
}

/// Read the Linux process's container ID from `/proc/self/cgroup` using rustix.
///
/// Returns `None` when the file is missing or no recognized ID is present. Other I/O errors
/// are propagated, and invalid UTF-8 is reported as `ILSEQ`.
#[cfg(target_os = "linux")]
pub fn read_container_id() -> rustix::io::Result<Option<String>> {
    let bytes = match linux::read_file(CGROUP_PATH) {
        Ok(bytes) => bytes,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error),
    };
    let cgroup = core::str::from_utf8(&bytes).map_err(|_| rustix::io::Errno::ILSEQ)?;
    Ok(parse_container_id(cgroup).map(String::from))
}

#[cfg(any(target_os = "linux", test))]
fn discover_with_reader(reader: &impl OriginRead, env: &impl EnvRead) -> EntityInfo {
    let cgroup = reader
        .read(CGROUP_PATH)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let container_id = cgroup
        .as_deref()
        .and_then(parse_container_id)
        .map(String::from);
    let entity_id = match container_id.as_deref() {
        Some(id) => Some(format!("ci-{id}")),
        None => cgroup.as_deref().and_then(|cgroup| {
            cgroup_inode(cgroup, "memory", CGROUP_MOUNT_PATH, |path| {
                reader.inode(path).ok()
            })
            .map(|inode| format!("in-{inode}"))
        }),
    };

    EntityInfo {
        container_id,
        entity_id,
        external_env: env.get("DD_EXTERNAL_ENV").filter(|value| !value.is_empty()),
    }
}

#[cfg(feature = "std")]
pub(super) fn external_env() -> Option<String> {
    crate::environment::var("DD_EXTERNAL_ENV")
        .ok()
        .filter(|value| !value.is_empty())
}

pub(super) fn headers<'a>(
    container_id: Option<&'a str>,
    entity_id: Option<&'a str>,
    external_env: Option<&'a str>,
) -> impl Iterator<Item = (&'static str, &'a str)> {
    [
        container_id.map(|id| ("datadog-container-id", id)),
        entity_id.map(|id| ("datadog-entity-id", id)),
        external_env.map(|value| ("datadog-external-env", value)),
    ]
    .into_iter()
    .flatten()
}

#[cfg(any(target_os = "linux", all(feature = "std", unix), test))]
pub(super) fn cgroup_inode(
    cgroup: &str,
    base_controller: &str,
    mount: &str,
    inode: impl Fn(&str) -> Option<u64>,
) -> Option<u64> {
    if inode(CGROUP_NAMESPACE_PATH)? == HOST_CGROUP_NAMESPACE_INODE {
        return None;
    }
    inode(&cgroup_node_path(cgroup, base_controller, mount)?)
}

#[cfg(any(target_os = "linux", all(feature = "std", unix), test))]
pub(super) fn cgroup_node_path(cgroup: &str, base_controller: &str, mount: &str) -> Option<String> {
    let mut node_path = None;
    for line in cgroup.lines() {
        let mut fields = line.split(':');
        let _hierarchy = fields.next()?;
        let Some(controllers) = fields.next() else {
            continue;
        };
        let Some(path) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            continue;
        }
        let is_v1 = controllers.split(',').any(|value| value == base_controller);
        if !is_v1 && !controllers.split(',').any(str::is_empty) {
            continue;
        }
        let mut node = String::from(mount.trim_end_matches('/'));
        if is_v1 {
            node.push('/');
            node.push_str(base_controller);
        }
        let relative = path.strip_prefix('/').unwrap_or(path);
        if !relative.is_empty() {
            node.push('/');
            node.push_str(relative);
        }
        if node.is_empty() {
            node.push('/');
        }
        node_path = Some(node);
        if is_v1 {
            break;
        }
    }
    node_path
}

/// Extract the first recognized container ID from Linux cgroup file contents.
///
/// Recognizes lowercase hexadecimal IDs, standard UUIDs, PCF/Garden UUIDs, ECS task IDs,
/// and systemd `.scope` suffixes, including a single child cgroup after `.scope`.
pub fn parse_container_id(cgroup: &str) -> Option<&str> {
    cgroup.lines().find_map(|line| {
        let (hierarchy, rest) = line.split_once(':')?;
        if hierarchy.is_empty() || !hierarchy.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let (_, path) = rest.split_once(':')?;
        let path = path.trim_end_matches(' ');
        if let Some((scope, suffix)) = path.rsplit_once(".scope") {
            let valid_suffix = suffix.is_empty()
                || suffix
                    .strip_prefix('/')
                    .is_some_and(|child| !child.is_empty() && !child.contains(['/', ' ', '\t']));
            if let Some(id) = valid_suffix.then(|| container_id_suffix(scope)).flatten() {
                return Some(id);
            }
        }
        container_id_suffix(path)
    })
}

fn container_id_suffix(path: &str) -> Option<&str> {
    for length in [64, 36, 28] {
        let Some(start) = path.len().checked_sub(length) else {
            continue;
        };
        let Some(id) = path.get(start..) else {
            continue;
        };
        let valid = match length {
            64 => is_hex(id.as_bytes()),
            36 => is_uuid(id.as_bytes(), true),
            28 => is_uuid(id.as_bytes(), false),
            _ => false,
        };
        if valid {
            return Some(id);
        }
    }

    let (prefix, suffix) = path.rsplit_once('-')?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let start = prefix.len().checked_sub(32)?;
    let id = prefix.get(start..)?;
    is_hex(id.as_bytes()).then(|| &path[start..])
}

fn is_hex(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn is_uuid(bytes: &[u8], allow_underscore: bool) -> bool {
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-' || (allow_underscore && *byte == b'_')
        } else {
            is_hex(core::slice::from_ref(byte))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::cell::RefCell;

    struct Source<'a> {
        cgroup: Option<&'a [u8]>,
        namespace: Option<u64>,
        node: Option<(&'a str, u64)>,
        env: Option<&'a str>,
        inode_calls: RefCell<Vec<String>>,
    }

    impl<'a> Source<'a> {
        fn new(cgroup: &'a [u8]) -> Self {
            Self {
                cgroup: Some(cgroup),
                namespace: Some(123),
                node: None,
                env: None,
                inode_calls: RefCell::default(),
            }
        }
    }

    impl OriginRead for Source<'_> {
        type Error = ();

        fn read(&self, path: &str) -> Result<Vec<u8>, ()> {
            assert_eq!(path, CGROUP_PATH);
            self.cgroup.map(<[u8]>::to_vec).ok_or(())
        }

        fn inode(&self, path: &str) -> Result<u64, ()> {
            self.inode_calls.borrow_mut().push(path.into());
            if path == CGROUP_NAMESPACE_PATH {
                self.namespace.ok_or(())
            } else {
                self.node
                    .filter(|(expected, _)| path == *expected)
                    .map(|(_, inode)| inode)
                    .ok_or(())
            }
        }
    }

    impl EnvRead for Source<'_> {
        fn get(&self, name: &str) -> Option<String> {
            assert_eq!(name, "DD_EXTERNAL_ENV");
            self.env.map(String::from)
        }
    }

    #[test]
    fn container_id_takes_precedence_and_all_headers_are_available() {
        let source = Source {
            env: Some("it-false,cn-checkout,pu-pod-uuid"),
            namespace: Some(HOST_CGROUP_NAMESPACE_INODE),
            ..Source::new(b"0::/ecs/8cd79a803caf4d2aa945152e934a5c00-1053176469\n")
        };
        let info = discover_with_reader(&source, &source);
        assert_eq!(
            info.headers().collect::<Vec<_>>(),
            vec![
                (
                    "datadog-container-id",
                    "8cd79a803caf4d2aa945152e934a5c00-1053176469"
                ),
                (
                    "datadog-entity-id",
                    "ci-8cd79a803caf4d2aa945152e934a5c00-1053176469"
                ),
                ("datadog-external-env", "it-false,cn-checkout,pu-pod-uuid"),
            ]
        );
        assert!(source.inode_calls.borrow().is_empty());
    }

    #[test]
    fn unified_cgroup_uses_namespace_and_node_inodes() {
        let source = Source {
            node: Some(("/sys/fs/cgroup/custom/path", 42)),
            ..Source::new(b"0::/custom/path\n")
        };
        let info = discover_with_reader(&source, &source);
        assert_eq!(info.container_id, None);
        assert_eq!(info.entity_id.as_deref(), Some("in-42"));
        assert_eq!(
            *source.inode_calls.borrow(),
            [CGROUP_NAMESPACE_PATH, "/sys/fs/cgroup/custom/path"]
        );
    }

    #[test]
    fn memory_controller_takes_precedence_over_unified_hierarchy() {
        let source = Source {
            node: Some(("/sys/fs/cgroup/memory/user.slice", 1234)),
            ..Source::new(b"0::/unified\n0:cpu,memory:/user.slice\n5:cpu:/other\n")
        };
        assert_eq!(
            discover_with_reader(&source, &source).entity_id.as_deref(),
            Some("in-1234")
        );
    }

    #[test]
    fn host_namespace_and_missing_inodes_do_not_identify_a_container() {
        for namespace in [Some(HOST_CGROUP_NAMESPACE_INODE), None, Some(123)] {
            let source = Source {
                namespace,
                env: Some("external-data"),
                ..Source::new(b"0::/\n")
            };
            let info = discover_with_reader(&source, &source);
            assert_eq!(info.entity_id, None);
            assert_eq!(
                info.headers().collect::<Vec<_>>(),
                [("datadog-external-env", "external-data")]
            );
            if namespace != Some(123) {
                assert_eq!(source.inode_calls.borrow().len(), 1);
            }
        }
    }

    #[test]
    fn environment_is_independent_of_cgroup_failures() {
        for cgroup in [None, Some(b"\xff".as_slice()), Some(b"invalid".as_slice())] {
            let source = Source {
                cgroup,
                env: Some("external-data"),
                ..Source::new(b"")
            };
            assert_eq!(
                discover_with_reader(&source, &source),
                EntityInfo {
                    external_env: Some("external-data".into()),
                    ..Default::default()
                }
            );
        }
    }

    #[test]
    fn only_empty_or_missing_external_env_is_omitted() {
        for env in [None, Some(""), Some(" "), Some("external-data")] {
            let source = Source {
                env,
                ..Source::new(b"")
            };
            let info = discover_with_reader(&source, &source);
            assert_eq!(
                info.external_env.as_deref(),
                env.filter(|value| !value.is_empty())
            );
            assert_eq!(
                info.headers().count(),
                usize::from(info.external_env.is_some())
            );
        }
    }

    #[test]
    fn existing_container_fixtures_work_without_std() {
        for (contents, expected) in [
            (
                include_str!("../../tests/cgroup.docker"),
                Some("9d5b23edb1ba181e8910389a99906598d69ac9a0ead109ee55730cc416d95f7f"),
            ),
            (
                include_str!("../../tests/cgroup.kubernetes"),
                Some("3e74d3fd9db4c9dd921ae05c2502fb984d0cde1b36e581b13f79c639da4518a1"),
            ),
            (
                include_str!("../../tests/cgroup.ecs"),
                Some("38fac3e99302b3622be089dd41e7ccf38aff368a86cc339972075136ee2710ce"),
            ),
            (
                include_str!("../../tests/cgroup.fargate"),
                Some("432624d2150b349fe35ba397284dea788c2bf66b885d14dfc1569b01890ca7da"),
            ),
            (
                include_str!("../../tests/cgroup.fargate.1.4"),
                Some("8cd79a803caf4d2aa945152e934a5c00-1053176469"),
            ),
            (
                include_str!("../../tests/cgroup.pcf"),
                Some("6f265890-5165-7fab-6b52-18d1"),
            ),
            (
                include_str!("../../tests/cgroup.whitespace"),
                Some("3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f869860"),
            ),
            (
                include_str!("../../tests/cgroup.unrecognized"),
                Some("9d5b23edb1ba181e8910389a99906598d69ac9a0ead109ee55730cc416d95f7f"),
            ),
            (include_str!("../../tests/cgroup.linux"), None),
            (include_str!("../../tests/cgroup.v2"), None),
            (include_str!("../../tests/cgroup.edge_cases"), None),
            (
                include_str!("../../tests/cgroup.invalid_line_container_id"),
                None,
            ),
            (
                include_str!("../../tests/cgroup.invalid_line_task_id"),
                None,
            ),
            (include_str!("../../tests/cgroup.upper"), None),
        ] {
            assert_eq!(parse_container_id(contents), expected, "{contents}");
        }
    }

    #[test]
    fn existing_cgroup_node_fixtures_preserve_controller_selection() {
        for (contents, expected) in [
            (
                include_str!("../../tests/cgroup.v2"),
                Some("/sys/fs/cgroup"),
            ),
            (
                include_str!("../../tests/cgroup.v2_custom_path"),
                Some("/sys/fs/cgroup/custom/path"),
            ),
            (
                include_str!("../../tests/cgroup.linux"),
                Some("/sys/fs/cgroup/memory/user.slice/user-0.slice/session-14.scope"),
            ),
            (
                include_str!("../../tests/cgroup.v1_with_id_0"),
                Some("/sys/fs/cgroup/memory/user.slice/user-0.slice/session-14.scope"),
            ),
            (
                include_str!("../../tests/cgroup.multiple_controllers"),
                Some("/sys/fs/cgroup/memory/user.slice/user-0.slice/session-14.scope"),
            ),
            (include_str!("../../tests/cgroup.no_memory"), None),
            (
                include_str!("../../tests/cgroup.invalid_line_container_id"),
                None,
            ),
        ] {
            assert_eq!(
                cgroup_node_path(contents, "memory", CGROUP_MOUNT_PATH).as_deref(),
                expected,
                "{contents}"
            );
        }
    }
    #[test]
    fn container_id_formats_and_scope_paths() {
        for id in [
            "34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376",
            "34dc0b5e-626f-2c5c-4c51-70e34b10e765",
            "34dc0b5e_626f_2c5c_4c51_70e34b10e765",
            "34dc0b5e-626f-2c5c-4c51-70e3",
            "8cd79a803caf4d2aa945152e934a5c00-1053176469",
        ] {
            for (prefix, suffix) in [
                ("/docker/", ""),
                ("/kubepods.slice/docker-", ".scope"),
                ("/machine.slice/libpod-", ".scope/container"),
                ("/ecs/task/", "   "),
                ("/user.slice/user-1000.scope/docker/", ""),
            ] {
                let cgroup = format!("0::/\n11:memory:{prefix}{id}{suffix}\n");
                assert_eq!(parse_container_id(&cgroup), Some(id), "{cgroup}");
            }
        }
    }

    #[test]
    fn malformed_cgroups_and_unrecognized_ids_are_ignored() {
        let id = "34dc0b5e626f2c5c4c5170e34b10e7654ce36f0fcd532739f4445baabea03376";
        for cgroup in [
            String::new(),
            "0::/".into(),
            "invalid".into(),
            format!("x:memory:/docker/{id}"),
            format!(":memory:/docker/{id}"),
            format!("0::/docker/{}", &id[..63]),
            format!("0::/docker/{}", id.to_uppercase()),
            format!("0::/docker/{id}.scope/"),
            format!("0::/docker/{id}.scope/child/grandchild"),
            "0::/ecs/8cd79a803caf4d2aa945152e934a5c00-".into(),
            "0::/ecs/8cd79a803caf4d2aa945152e934a5c00-abc".into(),
            "0::/🐶".into(),
        ] {
            assert_eq!(parse_container_id(&cgroup), None, "{cgroup}");
        }
    }
}
