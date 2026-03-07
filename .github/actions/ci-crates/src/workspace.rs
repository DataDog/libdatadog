// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use cargo_metadata::camino::{Utf8Path, Utf8PathBuf};
use cargo_metadata::{MetadataCommand, Package};
use std::collections::{HashMap, HashSet, VecDeque};

/// Parsed workspace metadata with reverse-dependency index.
pub struct WorkspaceMetadata {
    packages: Vec<Package>,
    /// Maps each crate name to the list of workspace crates that depend on it.
    reverse_deps: HashMap<String, Vec<String>>,
    workspace_root: Utf8PathBuf,
}

impl WorkspaceMetadata {
    pub fn members(&self) -> &[Package] {
        &self.packages
    }

    pub fn workspace_root(&self) -> &Utf8Path {
        &self.workspace_root
    }

    pub fn affected_from(&self, seeds: &[String]) -> HashSet<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        for name in seeds {
            if visited.insert(name.clone()) {
                queue.push_back(name.clone());
            }
        }

        while let Some(name) = queue.pop_front() {
            if let Some(dependents) = self.reverse_deps.get(&name) {
                for dep in dependents {
                    if visited.insert(dep.clone()) {
                        queue.push_back(dep.clone());
                    }
                }
            }
        }

        visited
    }
}

pub fn load() -> Result<WorkspaceMetadata> {
    let metadata = MetadataCommand::new().exec()?;

    let workspace_root = metadata.workspace_root.clone();
    let packages: Vec<Package> = metadata.workspace_packages().into_iter().cloned().collect();

    let member_names: HashSet<String> = packages.iter().map(|p| p.name.as_str().to_string()).collect();

    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in &packages {
        for dep in &pkg.dependencies {
            if member_names.contains(&dep.name) {
                reverse_deps
                    .entry(dep.name.clone())
                    .or_default()
                    .push(pkg.name.as_str().to_string());
            }
        }
    }

    Ok(WorkspaceMetadata {
        packages,
        reverse_deps,
        workspace_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `WorkspaceMetadata` with an empty package list and a custom
    /// reverse-dep map expressed as `&[("crate", &["dependent", ...])]`.
    fn make_meta(deps: &[(&str, &[&str])]) -> WorkspaceMetadata {
        let reverse_deps = deps
            .iter()
            .map(|(k, vs)| {
                (
                    k.to_string(),
                    vs.iter().map(|s| s.to_string()).collect(),
                )
            })
            .collect();
        WorkspaceMetadata {
            packages: vec![],
            reverse_deps,
            workspace_root: Utf8PathBuf::new(),
        }
    }

    fn names(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    fn set(s: &[&str]) -> HashSet<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    // --- affected_from ---

    #[test]
    fn affected_from_empty_seeds_returns_empty() {
        let meta = make_meta(&[]);
        assert!(meta.affected_from(&[]).is_empty());
    }

    #[test]
    fn affected_from_seed_with_no_dependents() {
        let meta = make_meta(&[]);
        assert_eq!(meta.affected_from(&names(&["a"])), set(&["a"]));
    }

    #[test]
    fn affected_from_direct_dependents() {
        // b and c both depend on a
        let meta = make_meta(&[("a", &["b", "c"])]);
        assert_eq!(meta.affected_from(&names(&["a"])), set(&["a", "b", "c"]));
    }

    #[test]
    fn affected_from_transitive_chain() {
        // a → b → c
        let meta = make_meta(&[("a", &["b"]), ("b", &["c"])]);
        assert_eq!(meta.affected_from(&names(&["a"])), set(&["a", "b", "c"]));
    }

    #[test]
    fn affected_from_diamond() {
        // b and c depend on a; d depends on both b and c
        let meta = make_meta(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &["d"])]);
        assert_eq!(
            meta.affected_from(&names(&["a"])),
            set(&["a", "b", "c", "d"])
        );
    }

    #[test]
    fn affected_from_multiple_seeds() {
        // independent chains: a → b, c → d
        let meta = make_meta(&[("a", &["b"]), ("c", &["d"])]);
        assert_eq!(
            meta.affected_from(&names(&["a", "c"])),
            set(&["a", "b", "c", "d"])
        );
    }

    #[test]
    fn affected_from_duplicate_seeds_no_duplicates_in_result() {
        let meta = make_meta(&[("a", &["b"])]);
        assert_eq!(
            meta.affected_from(&names(&["a", "a"])),
            set(&["a", "b"])
        );
    }

    #[test]
    fn affected_from_unknown_seed_returns_seed_only() {
        let meta = make_meta(&[]);
        assert_eq!(
            meta.affected_from(&names(&["not-in-workspace"])),
            set(&["not-in-workspace"])
        );
    }

    // --- load() integration ---

    #[test]
    fn load_returns_non_empty_workspace() {
        let meta = load().expect("load() should succeed against the real workspace");
        assert!(
            !meta.members().is_empty(),
            "expected at least one workspace member"
        );
    }
}
