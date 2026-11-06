// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Structural comparison of the changed `Cargo.toml` manifests between the
//! base branch and the PR head.
//!
//! Both sides are fetched with git (`git show <ref>:<path>`) and parsed with
//! a real TOML parser; the decision compares the parsed data, never the text
//! format — comments, key order, and formatting differences compare equal,
//! and section attribution cannot be fooled by hunk context the way diff
//! parsing can.
//!
//! A manifest whose two sides differ ONLY in runtime dependency declarations
//! (normal/dev dependencies, `[workspace.dependencies]`, target-scoped
//! variants — version, source, features, `package =` renames) is scoped
//! exactly like a lockfile bump: the changed package names are intersected
//! with the packages each benchmark cluster executes (see
//! `coverage::CoverageEntry::deps`), so only the benchmarks actually
//! measuring the changed dependency run.
//!
//! Anything else — the crate's own `[features]`, `[profile]`, `[package]`
//! metadata, `build-dependencies` (compile-time effect), `[patch]`, or a
//! dependency that is a proc-macro — keeps the conservative full run: line
//! coverage cannot attribute those.
//!
//! Path dependencies (workspace members) are excluded from the package set:
//! their *source* changes arrive as diffs the regular line intersection
//! already handles. A *declaration* change of a member (e.g. enabling a
//! feature on `libdd-x = { path = ... }`) is different: the member's
//! compiled code changes with no source line moving, so nothing in the
//! dependency package set can scope it — such a manifest is marked
//! not-deps-only and keeps the conservative full run.

use crate::diff::ChangedLines;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use toml::Table;
use toml::Value;

/// Dependency-scoping analysis of the changed manifests.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestChanges {
    /// Per changed manifest path: true when the base and head documents
    /// differ only in runtime dependency declarations that coverage can
    /// scope. False keeps the conservative full run for that manifest.
    pub deps_only: BTreeMap<String, bool>,
    /// External packages whose wiring changed anywhere (dependency added,
    /// removed, or re-specified in a deps-only manifest). Workspace members
    /// (path dependencies) are excluded: a *declaration* change of a member
    /// instead makes the manifest not-deps-only (see [`ParsedManifest`]).
    pub packages: BTreeSet<String>,
}

impl ManifestChanges {
    /// Is a manifest path analyzed as dependencies-only?
    /// Paths never analyzed here (or not present) keep their default
    /// conservative treatment in `select`.
    pub fn is_deps_only(&self, path: &str) -> bool {
        self.deps_only.get(path).copied().unwrap_or(false)
    }
}

/// Analyze the changed manifests of a diff. `changed` supplies the paths;
/// each manifest-shaped one (a `Cargo.toml` outside the CI helper workspace)
/// is fetched on both sides and compared.
///
/// Any failure (git, metadata, parsing) is conservative: the affected
/// manifests are marked not-deps-only, i.e. they fall back to the full run.
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
    // Mark everything conservative up front; each successfully compared
    // deps-only manifest flips its entry to true.
    for path in &manifests {
        out.deps_only.insert(path.clone(), false);
    }
    let members = crate::lockfile::workspace_members(workspace_root);

    // Compare against the MERGE-BASE with the base branch, matching the
    // three-dot semantics of every other diff in the pipeline: `origin/main`
    // itself has moved on since the PR branched, and its unrelated changes
    // (e.g. version bumps) would wrongly disqualify the manifest.
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
        // Missing on a side (manifest added or deleted) compares against an
        // empty document: the [package] table then differs, which correctly
        // disqualifies — a whole manifest never appears or disappears via a
        // dependency-only change.
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

    // Dependencies whose code runs at compile time (proc-macros) have no
    // runtime coverage to scope with: treat their manifests as conservative.
    if !out.packages.is_empty() && workspace_has_proc_macro(workspace_root, &out.packages) {
        for path in &manifests {
            out.deps_only.insert(path.clone(), false);
        }
    }
    out
}

/// A path is a manifest candidate when it is a Cargo.toml of the workspace —
/// not the CI helper workspace's, which cannot affect the benchmarked crates.
fn is_manifest(path: &str) -> bool {
    path == "Cargo.toml" || (path.ends_with("/Cargo.toml") && !path.starts_with(".github/actions/"))
}

/// The result of comparing one manifest's two sides.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedManifest {
    deps_only: bool,
    packages: BTreeSet<String>,
    /// Workspace members whose runtime-dependency *declaration* changed.
    /// Their compiled code changes with no source line moving (e.g. a
    /// feature enabled on `libdd-x = { path = ... }`), so the dependency
    /// packages cannot scope the manifest: it must keep the full run.
    changed_members: BTreeSet<String>,
}

/// Compare a manifest's base and head documents.
fn compare_manifests(base: &str, head: &str, members: &BTreeSet<String>) -> ParsedManifest {
    let conservative = || ParsedManifest {
        deps_only: false,
        packages: BTreeSet::new(),
        changed_members: BTreeSet::new(),
    };
    // An unparseable document on either side is never scoping-safe.
    let Ok(base) = base.parse::<Table>() else {
        return conservative();
    };
    let Ok(head) = head.parse::<Table>() else {
        return conservative();
    };

    // Packages whose runtime wiring changed: entries of the runtime
    // dependency tables that appear, disappear, or change on either side,
    // resolved through `package = "..."` renames and excluding workspace
    // members (path dependencies).
    let base_deps = runtime_dependencies(&base);
    let head_deps = runtime_dependencies(&head);
    let mut packages = BTreeSet::new();
    let mut changed_members = BTreeSet::new();
    for name in base_deps.keys().chain(head_deps.keys()) {
        if base_deps.get(name) != head_deps.get(name) {
            if members.contains(name) {
                // The member's declaration changed: its compiled code
                // changes with no source diff to scope it, so this manifest
                // must not be treated as deps-only (see ParsedManifest).
                changed_members.insert(name.clone());
            } else {
                packages.insert(name.clone());
            }
        }
    }

    // Everything outside the runtime dependency tables — the crate's own
    // features, profiles, metadata, build-dependencies (compile time),
    // patches, membership — must be identical for the manifest to be
    // scoped by coverage, and no workspace member's dependency
    // declaration may have changed either.
    let deps_only = stripped(&base) == stripped(&head) && changed_members.is_empty();
    ParsedManifest {
        deps_only,
        packages,
        changed_members,
    }
}

/// Collect the runtime dependency declarations of a parsed manifest, keyed by
/// resolved package name. Covers `[dependencies]`, `[dev-dependencies]`,
/// `[workspace.dependencies]`, and their `target.'cfg(...)'.` variants. A
/// name maps to every declaration of it (a crate can depend on the same
/// package under several target scopes); two sides differ only when the
/// collected declarations differ.
fn runtime_dependencies(doc: &Table) -> BTreeMap<String, Vec<&Value>> {
    // Not a closure: closures cannot accept higher-ranked lifetime
    // parameters, and these borrows must flow from `doc` through to the
    // return value.
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
    if let Some(Value::Table(workspace)) = doc.get("workspace") {
        if let Some(Value::Table(table)) = workspace.get("dependencies") {
            collect(table, &mut out);
        }
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

/// The dependency's package name: `package = "..."` renames inside the
/// declaration win over the table key.
fn package_name(key: &str, value: &Value) -> String {
    if let Some(renamed) = value.get("package").and_then(Value::as_str) {
        renamed.to_string()
    } else {
        key.to_string()
    }
}

/// A copy of the document with the runtime dependency tables removed and
/// empty leftover sub-tables pruned, so the remaining data can be compared
/// structurally. Everything that remains — build-dependencies, the crate's
/// own features, profiles, package metadata, patches — must be identical
/// between the two sides.
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

/// Recursively remove empty sub-tables left behind by the stripping above
/// (a target scope that contained only dependencies), so a fully removed
/// dependency section compares equal to a never-existing one. Empty arrays
/// are kept: `benches = []` is meaningful data.
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

/// Does any of the packages resolve to a proc-macro in this workspace?
/// Their code runs at compile time, so runtime coverage cannot scope them.
/// Best-effort: a metadata failure conservatively reports true.
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
        // Same data, different text: reordered keys, added comments, inline
        // table layout. Parsed data is identical, so nothing changes.
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
        // And the other direction records the removal.
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
        // A whole manifest appearing means [package] appeared too.
        let parsed = compare_manifests("", doc, &set(&[]));
        assert!(!parsed.deps_only);
        let parsed = compare_manifests(doc, "", &set(&[]));
        assert!(!parsed.deps_only);
    }

    #[test]
    fn changed_member_dependency_declaration_is_not_deps_only() {
        // Enabling a feature on a member path dependency changes the
        // member's compiled code with no source line moving: the dependency
        // package set cannot scope it (it excludes members), so the
        // manifest must keep the conservative full run.
        let base = "[dependencies]\nlibdd-trace-utils = { path = \"../trace-utils\" }\n";
        let head = "[dependencies]\nlibdd-trace-utils = { path = \"../trace-utils\", features = [\"x\"] }\n";
        let parsed = compare_manifests(base, head, &set(&["libdd-trace-utils"]));
        assert!(!parsed.deps_only);
        assert_eq!(parsed.packages, set(&[]));
        assert_eq!(parsed.changed_members, set(&["libdd-trace-utils"]));
    }

    #[test]
    fn unchanged_member_entries_stay_excluded_from_packages() {
        // The member's declaration is unchanged (only an external dependency
        // was added), so the manifest is still deps-only and the member
        // stays out of the package signal.
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
