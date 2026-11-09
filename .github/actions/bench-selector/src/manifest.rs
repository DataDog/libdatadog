// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Compare manifests structurally at the merge base and HEAD, ignoring formatting.
//! Runtime dependency changes are scoped by executed package coverage. Changes
//! to other sections, proc-macros, or member dependency declarations require a
//! full run: compiled code can change without any source line moving.

use crate::diff::ChangedLines;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use toml::{Table, Value};

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestChanges {
    /// Per changed manifest: false requires a full run, true allows package scoping.
    pub deps_only: BTreeMap<String, bool>,
    /// Changed external dependencies. Member declaration changes instead force a full run.
    pub packages: BTreeSet<String>,
}

/// Analyze changed manifests; failures keep the conservative full-run verdict.
pub fn manifest_changes(
    base_ref: &str,
    workspace_root: &Path,
    changed: &ChangedLines,
) -> ManifestChanges {
    let manifests: Vec<String> = changed
        .keys()
        .filter(|path| is_manifest(path))
        .cloned()
        .collect();
    let mut out = ManifestChanges::default();
    if manifests.is_empty() {
        return out;
    }
    for path in &manifests {
        out.deps_only.insert(path.clone(), false);
    }
    let members = crate::lockfile::workspace_members(workspace_root);

    let merge_base = match crate::git::merge_base(base_ref, workspace_root) {
        Ok(rev) => rev,
        Err(err) => {
            log::warn!("no merge base with origin/{base_ref} ({err}); full run");
            return out;
        }
    };

    log::info!(
        "manifests: comparing {} changed Cargo.toml against merge base {merge_base}",
        manifests.len()
    );
    for path in &manifests {
        // Added/deleted manifests compare against empty; [package] then differs.
        let base = match crate::git::show_blob(&merge_base, path, workspace_root) {
            Ok(content) => content.unwrap_or_default(),
            Err(err) => {
                log::warn!("cannot read {path} at {merge_base} ({err}); full run");
                return out;
            }
        };
        let head = match crate::git::show_blob("HEAD", path, workspace_root) {
            Ok(content) => content.unwrap_or_default(),
            Err(err) => {
                log::warn!("cannot read {path} on HEAD ({err}); full run");
                return out;
            }
        };
        let parsed = compare_manifests(&base, &head, &members);
        let verdict = if parsed.deps_only {
            format!(
                "dependencies-only (changed: {})",
                if parsed.packages.is_empty() {
                    "none".to_string()
                } else {
                    parsed
                        .packages
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )
        } else if parsed.changed_members.is_empty() {
            "not dependencies-only -> full run".to_string()
        } else {
            format!(
                "workspace-member dependency declaration changed ({}) -> full run",
                parsed
                    .changed_members
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        log::info!("manifests: {path} {verdict}");
        out.deps_only.insert(path.clone(), parsed.deps_only);
        out.packages.extend(parsed.packages);
    }

    // Proc-macro effects are invisible to runtime coverage.
    if !out.packages.is_empty() && workspace_has_proc_macro(workspace_root, &out.packages) {
        for path in &manifests {
            out.deps_only.insert(path.clone(), false);
        }
    }
    out
}

/// The separate CI helper workspace cannot affect benchmarked crates.
fn is_manifest(path: &str) -> bool {
    path == "Cargo.toml" || (path.ends_with("/Cargo.toml") && !path.starts_with(".github/actions/"))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedManifest {
    deps_only: bool,
    packages: BTreeSet<String>,
    /// Member declaration changes cannot be scoped by external-package coverage.
    changed_members: BTreeSet<String>,
}

fn compare_manifests(base: &str, head: &str, members: &BTreeSet<String>) -> ParsedManifest {
    let Ok(base) = base.parse::<Table>() else {
        return ParsedManifest::default();
    };
    let Ok(head) = head.parse::<Table>() else {
        return ParsedManifest::default();
    };

    let base_deps = runtime_dependencies(&base);
    let head_deps = runtime_dependencies(&head);
    let mut packages = BTreeSet::new();
    let mut changed_members = BTreeSet::new();
    for name in base_deps.keys().chain(head_deps.keys()) {
        if base_deps.get(name) != head_deps.get(name) {
            if members.contains(name) {
                changed_members.insert(name.clone());
            } else {
                packages.insert(name.clone());
            }
        }
    }

    let deps_only = stripped(&base) == stripped(&head) && changed_members.is_empty();
    ParsedManifest {
        deps_only,
        packages,
        changed_members,
    }
}

/// Group normal/dev/workspace/target dependencies by resolved package name,
/// retaining all declarations when a package appears in several scopes.
fn runtime_dependencies(doc: &Table) -> BTreeMap<String, Vec<&Value>> {
    fn collect<'a>(table: &'a Table, out: &mut BTreeMap<String, Vec<&'a Value>>) {
        for (key, value) in table {
            out.entry(package_name(key, value)).or_default().push(value);
        }
    }
    let mut out: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for section in ["dependencies", "dev-dependencies"] {
        if let Some(Value::Table(table)) = doc.get(section) {
            collect(table, &mut out);
        }
    }
    if let Some(Value::Table(workspace)) = doc.get("workspace")
        && let Some(Value::Table(table)) = workspace.get("dependencies")
    {
        collect(table, &mut out);
    }
    if let Some(Value::Table(targets)) = doc.get("target") {
        for target in targets.values() {
            let Value::Table(target) = target else {
                continue;
            };
            for section in ["dependencies", "dev-dependencies"] {
                if let Some(Value::Table(table)) = target.get(section) {
                    collect(table, &mut out);
                }
            }
        }
    }
    out
}

fn package_name(key: &str, value: &Value) -> String {
    value
        .get("package")
        .and_then(Value::as_str)
        .unwrap_or(key)
        .to_owned()
}

/// Remove runtime dependency tables to compare the remaining build inputs.
fn stripped(doc: &Table) -> Table {
    fn strip_table(table: &mut Table) {
        table.remove("dependencies");
        table.remove("dev-dependencies");
    }
    let mut rest: Table = doc.clone();
    strip_table(&mut rest);
    if let Some(Value::Table(workspace)) = rest.get_mut("workspace") {
        workspace.remove("dependencies");
    }
    if let Some(Value::Table(targets)) = rest.get_mut("target") {
        for (_, target) in targets.iter_mut() {
            if let Value::Table(target) = target {
                strip_table(target);
            }
        }
    }
    prune_empty_tables(&mut rest);
    rest
}

/// Prune emptied scopes; keep meaningful empty arrays such as `benches = []`.
fn prune_empty_tables(table: &mut Table) {
    table.retain(|_, value| {
        if let Value::Table(inner) = value {
            prune_empty_tables(inner);
            !inner.is_empty()
        } else {
            true
        }
    });
}

/// Metadata failures conservatively report a compile-time dependency.
fn workspace_has_proc_macro(workspace_root: &Path, packages: &BTreeSet<String>) -> bool {
    let Ok(output) = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["metadata", "--format-version", "1"])
        .output()
    else {
        return true;
    };
    if !output.status.success() {
        return true;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&stdout) else {
        return true;
    };
    let Some(pkgs) = metadata.get("packages").and_then(|p| p.as_array()) else {
        return true;
    };
    pkgs.iter().any(|pkg| {
        let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if !packages.contains(name) {
            return false;
        }
        pkg.get("targets")
            .and_then(|t| t.as_array())
            .map(|targets| {
                targets.iter().any(|t| {
                    t.get("kind")
                        .and_then(|k| k.as_array())
                        .map(|k| k.iter().any(|v| v.as_str() == Some("proc-macro")))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(true) // Cannot tell: be conservative.
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn identical_documents_are_deps_only() {
        let doc = "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n";
        let parsed = compare_manifests(doc, doc, &set(&[]));
        assert!(parsed.deps_only);
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn formatting_and_comments_only_compare_equal() {
        let base = "\
[package]
name = \"x\"
version = \"1.0.0\"

[dependencies]
serde = \"1\"
anyhow = \"1\"
";
        let head = "\
# crate x
[dependencies]
anyhow = \"1\" # unchanged
serde = \"1\"

[package]
version = \"1.0.0\"
name = \"x\"
";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn version_bump_is_deps_only() {
        let base = "[dependencies]\nserde = \"1.0.200\"\n";
        let head = "[dependencies]\nserde = \"1.0.210\"\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["serde"]));
    }

    #[test]
    fn added_and_removed_dependencies_are_deps_only() {
        let base = "[dev-dependencies]\ncriterion = \"0.5\"\n";
        let head = "[dev-dependencies]\ncriterion = \"0.5\"\nallocator-api2 = \"0.2\"\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["allocator-api2"]));
        let parsed = compare_manifests(head, base, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["allocator-api2"]));
    }

    #[test]
    fn feature_change_on_a_dep_is_deps_only() {
        let base = "[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n";
        let head = "[dependencies]\nserde = { version = \"1\", features = [\"derive\", \"rc\"] }\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["serde"]));
    }

    #[test]
    fn workspace_dependencies_section_changes_are_deps_only() {
        let base = "[workspace.dependencies]\nzstd = \"0.13\"\n";
        let head = "[workspace.dependencies]\nzstd = \"0.14\"\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["zstd"]));
    }

    #[test]
    fn target_scoped_dependency_changes_are_deps_only() {
        let base = "[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n";
        let head = "[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\nmemmap2 = \"0.9\"\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["memmap2"]));
    }

    #[test]
    fn removing_a_whole_target_scope_is_deps_only() {
        let base = "[target.'cfg(unix)'.dependencies]\nmemmap2 = \"0.9\"\n";
        let head = "";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["memmap2"]));
    }

    #[test]
    fn renamed_packages_resolve_through_package_field() {
        let base = "[dependencies]\nserde2 = { package = \"serde\", version = \"1\" }\n";
        let head = "[dependencies]\nserde2 = { package = \"serde\", version = \"1\", features = [\"rc\"] }\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["serde"]));
    }

    #[test]
    fn crate_features_change_is_not_deps_only() {
        let base = "[features]\ndefault = [\"v04\"]\n";
        let head = "[features]\ndefault = [\"v04\"]\nv05 = []\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn package_metadata_change_is_not_deps_only() {
        let base = "[package]\nversion = \"1.0.0\"\n";
        let head = "[package]\nversion = \"1.0.1\"\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn build_dependencies_are_not_scoping_safe() {
        let base = "[build-dependencies]\ncc = \"1\"\n";
        let head = "[build-dependencies]\ncc = \"1\"\nanyhow = \"1\"\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn patch_section_change_is_not_deps_only() {
        let base = "";
        let head = "[patch.crates-io]\nproptest = { git = 'https://github.com/x/proptest' }\n";
        let parsed = compare_manifests(base, head, &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn new_or_deleted_manifests_are_not_deps_only() {
        let doc = "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n";
        let parsed = compare_manifests("", doc, &set(&[]));
        assert!(!parsed.deps_only);
        let parsed = compare_manifests(doc, "", &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn changed_member_dependency_declaration_is_not_deps_only() {
        let base = "[dependencies]\nlibdd-trace-utils = { path = \"../trace-utils\" }\n";
        let head = "[dependencies]\nlibdd-trace-utils = { path = \"../trace-utils\", features = [\"x\"] }\n";
        let parsed = compare_manifests(base, head, &set(&["libdd-trace-utils"]));
        assert!(!parsed.deps_only);
        assert_eq!(parsed.packages, set(&[]));
        assert_eq!(parsed.changed_members, set(&["libdd-trace-utils"]));
    }

    #[test]
    fn unchanged_member_entries_stay_excluded_from_packages() {
        let base = "[dependencies]\nlibdd-trace-utils = { path = \"../trace-utils\" }\n";
        let head =
            "[dependencies]\nlibdd-trace-utils = { path = \"../trace-utils\" }\nserde = \"1\"\n";
        let parsed = compare_manifests(base, head, &set(&["libdd-trace-utils"]));
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, set(&["serde"]));
    }

    #[test]
    fn unparseable_documents_are_conservative() {
        let parsed = compare_manifests("[dependencies]\nserde = ", "not toml at all", &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn manifest_paths_exclude_the_helper_workspace() {
        assert!(is_manifest("Cargo.toml"));
        assert!(is_manifest("libdd-x/Cargo.toml"));
        assert!(!is_manifest(".github/actions/Cargo.toml"));
        assert!(!is_manifest(".github/actions/bench-selector/Cargo.toml"));
        assert!(!is_manifest("libdd-x/src/lib.rs"));
    }
}
