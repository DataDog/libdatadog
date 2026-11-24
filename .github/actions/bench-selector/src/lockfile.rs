// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Compare root lockfiles structurally at the merge base and HEAD, ignoring
//! formatting. Changed external package names are scoped by executed coverage,
//! except build-time packages (build dependencies, proc-macros) that runtime
//! coverage can never attribute and that therefore widen the selection;
//! workspace members use source diffs instead. Missing/invalid lockfiles, git
//! failures, or changes outside package entries are unattributable (full run).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use toml::{Table, Value};

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockfileChanges {
    /// Changed external packages, excluding workspace members.
    pub packages: BTreeSet<String>,
    /// Changes cannot be scoped to packages; select everything.
    pub unattributable: bool,
}

impl LockfileChanges {
    pub fn is_changed(&self) -> bool {
        !self.packages.is_empty() || self.unattributable
    }
}

fn unattributable() -> LockfileChanges {
    LockfileChanges {
        packages: BTreeSet::new(),
        unattributable: true,
    }
}

pub fn lockfile_changes(base_ref: &str, workspace_root: &Path) -> Result<LockfileChanges> {
    let members = workspace_members(workspace_root);

    let merge_base = match crate::git::merge_base(base_ref, workspace_root) {
        Ok(rev) => rev,
        Err(err) => {
            log::warn!("no merge base with origin/{base_ref} ({err}); full run");
            return Ok(unattributable());
        }
    };
    let base = match crate::git::show_blob(&merge_base, "Cargo.lock", workspace_root) {
        Ok(content) => content,
        Err(err) => {
            log::warn!("cannot read Cargo.lock at {merge_base} ({err}); full run");
            return Ok(unattributable());
        }
    };
    let head = match crate::git::show_blob("HEAD", "Cargo.lock", workspace_root) {
        Ok(content) => content,
        Err(err) => {
            log::warn!("cannot read Cargo.lock at HEAD ({err}); full run");
            return Ok(unattributable());
        }
    };
    let (Some(base), Some(head)) = (base, head) else {
        log::warn!("Cargo.lock missing on one side of {merge_base}..HEAD; full run");
        return Ok(unattributable());
    };
    log::info!("lockfile: comparing Cargo.lock against merge base {merge_base}");
    let mut changes = compare_lockfiles(&base, &head, &members);
    // Executed-function coverage can never attribute build-time packages
    // (build dependencies such as `cc`, proc-macros): they change compiled or
    // generated code without appearing in coverage `deps`, so the package
    // intersection would select nothing. Widen instead of scoping them away;
    // the manifest-side proc-macro check does not run for lockfile-only
    // updates, so this must cover them here.
    if !changes.unattributable
        && !changes.packages.is_empty()
        && !runtime_scopes_changed_packages(workspace_root, &changes.packages)
    {
        log::info!("lockfile: changed package(s) invisible to runtime coverage -> full run");
        changes.unattributable = true;
    }
    if changes.unattributable {
        log::info!("lockfile: unattributable change -> full run");
    } else if changes.is_changed() {
        log::info!(
            "lockfile: changed packages: {}",
            changes
                .packages
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(changes)
}

fn compare_lockfiles(base: &str, head: &str, members: &BTreeSet<String>) -> LockfileChanges {
    let Ok(base) = base.parse::<Table>() else {
        return unattributable();
    };
    let Ok(head) = head.parse::<Table>() else {
        return unattributable();
    };
    let (Some(base_pkgs), Some(head_pkgs)) = (package_entries(&base), package_entries(&head))
    else {
        return unattributable();
    };

    let mut changes = LockfileChanges::default();
    for name in base_pkgs.keys().chain(head_pkgs.keys()) {
        if base_pkgs.get(name) != head_pkgs.get(name) && !members.contains(name) {
            changes.packages.insert(name.clone());
        }
    }

    if stripped(&base) != stripped(&head) {
        changes.unattributable = true;
    }
    changes
}

/// Preserve duplicate-name entries as ordered lists; reject malformed package data.
fn package_entries(doc: &Table) -> Option<BTreeMap<String, Vec<&Value>>> {
    let mut out: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    match doc.get("package") {
        None => return Some(out),
        Some(Value::Array(entries)) => {
            for entry in entries {
                let Some(Value::String(name)) = entry.get("name") else {
                    return None;
                };
                out.entry(name.clone()).or_default().push(entry);
            }
        }
        Some(_) => return None,
    }
    Some(out)
}

fn stripped(doc: &Table) -> Table {
    doc.iter()
        .filter(|(key, _)| key.as_str() != "package")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Metadata failures exclude no packages, conservatively over-selecting.
pub(crate) fn workspace_members(workspace_root: &Path) -> BTreeSet<String> {
    let Ok(output) = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
    else {
        return BTreeSet::new();
    };
    if !output.status.success() {
        return BTreeSet::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(&stdout) {
        Ok(metadata) => metadata
            .get("packages")
            .and_then(|p| p.as_array())
            .map(|pkgs| {
                pkgs.iter()
                    .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => BTreeSet::new(),
    }
}

/// Whether runtime coverage can attribute every changed package. Build
/// dependencies and proc-macros change compiled or generated code while never
/// appearing in executed-function coverage, and packages absent from the
/// resolved graph cannot be classified at all: none of them may be scoped
/// away. Metadata failures conservatively report an unscopable change.
fn runtime_scopes_changed_packages(workspace_root: &Path, packages: &BTreeSet<String>) -> bool {
    let Ok(output) = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["metadata", "--format-version", "1"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(&stdout) {
        Ok(metadata) => scopes_changed_packages(&metadata, packages),
        Err(_) => false,
    }
}

/// A changed package is scopable only when it is runtime-reachable — compiled
/// into the bench binary through normal or dev dependency edges, expanded
/// from workspace members without ever crossing a proc-macro — and is not
/// itself a proc-macro. Name-based, like the lockfile diff and coverage `deps`.
fn scopes_changed_packages(metadata: &serde_json::Value, packages: &BTreeSet<String>) -> bool {
    let Some(all) = metadata.get("packages").and_then(|p| p.as_array()) else {
        return false;
    };
    let name_by_id = |id: &serde_json::Value| -> Option<&str> {
        all.iter()
            .find(|pkg| pkg.get("id") == Some(id))
            .and_then(|pkg| pkg.get("name"))
            .and_then(|name| name.as_str())
    };
    // Roots of the runtime graph: the workspace members.
    let mut reachable: BTreeSet<&str> = metadata
        .get("workspace_members")
        .and_then(|ids| ids.as_array())
        .map(|ids| ids.iter().filter_map(name_by_id).collect())
        .unwrap_or_default();
    let mut proc_macros: BTreeSet<&str> = BTreeSet::new();
    let mut runtime_deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for pkg in all {
        let Some(name) = pkg.get("name").and_then(|name| name.as_str()) else {
            continue;
        };
        if is_proc_macro(pkg) {
            proc_macros.insert(name);
        }
        for dep in pkg
            .get("dependencies")
            .and_then(|deps| deps.as_array())
            .into_iter()
            .flatten()
        {
            // Normal (`kind` null) and dev edges compile into bench binaries;
            // build edges never do.
            if dep.get("kind").and_then(|kind| kind.as_str()) == Some("build") {
                continue;
            }
            if let Some(dep_name) = dep.get("name").and_then(|name| name.as_str()) {
                runtime_deps.entry(name).or_default().push(dep_name);
            }
        }
    }
    // Proc-macro nodes are never expanded: their dependencies run at compile
    // time, not inside the bench binary.
    let mut queue: Vec<&str> = reachable.iter().copied().collect();
    while let Some(name) = queue.pop() {
        if proc_macros.contains(name) {
            continue;
        }
        for dep in runtime_deps.get(name).into_iter().flatten() {
            if reachable.insert(dep) {
                queue.push(*dep);
            }
        }
    }
    packages
        .iter()
        .all(|name| reachable.contains(name.as_str()) && !proc_macros.contains(name.as_str()))
}

/// Missing target data conservatively reports a proc-macro: a proc-macro's
/// effects are invisible to runtime coverage either way.
fn is_proc_macro(pkg: &serde_json::Value) -> bool {
    let Some(targets) = pkg.get("targets").and_then(|targets| targets.as_array()) else {
        return true;
    };
    targets.iter().any(|target| {
        target
            .get("kind")
            .and_then(|kinds| kinds.as_array())
            .map(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("proc-macro")))
            .unwrap_or(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use set as members;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn lockfile(packages: &str) -> String {
        format!("version = 4\n\n{packages}")
    }

    fn pkg(name: &str, version: &str) -> String {
        format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n\n")
    }

    #[test]
    fn formatting_only_differences_compare_equal() {
        let base = lockfile(
            "[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aaa\"\n\n",
        );
        let head = lockfile(
            "# resolved by cargo\n[[package]]\nversion = \"1.0.210\"\nname = \"serde\"\nchecksum = \"aaa\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n",
        );
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes, LockfileChanges::default());
        assert!(!changes.is_changed());
    }

    #[test]
    fn added_and_removed_packages_are_attributed() {
        let base = lockfile(&pkg("unicode-ident", "1.0.12"));
        let head = lockfile(&format!(
            "{}{}",
            pkg("paste", "1.0.15"),
            pkg("unicode-ident", "1.0.12")
        ));
        for (base, head) in [(&base, &head), (&head, &base)] {
            let changes = compare_lockfiles(base, head, &set(&[]));
            assert_eq!(changes.packages, set(&["paste"]));
            assert!(!changes.unattributable);
        }
    }

    #[test]
    fn changed_package_fields_are_attributed() {
        for (base, head) in [
            ("version = '0.13.3'", "version = '0.14.3'"),
            ("dependencies = ['itoa']", "dependencies = ['itoa', 'ryu']"),
            (
                "source = 'git+https://github.com/x/proptest#1111'",
                "source = 'git+https://github.com/x/proptest#2222'",
            ),
            ("checksum = 'aaa'", "checksum = 'bbb'"),
        ] {
            let doc = |field| lockfile(&format!("[[package]]\nname = 'dep'\n{field}\n"));
            let changes = compare_lockfiles(&doc(base), &doc(head), &set(&[]));
            assert_eq!(changes.packages, set(&["dep"]), "{base} -> {head}");
            assert!(!changes.unattributable);
            assert!(changes.is_changed());
        }
    }

    #[test]
    fn workspace_members_are_excluded() {
        let base = lockfile(&format!(
            "{}{}",
            pkg("libdd-trace-utils", "15.0.0"),
            pkg("zstd", "0.13.3")
        ));
        let head = lockfile(&format!(
            "{}{}",
            pkg("libdd-trace-utils", "15.1.0"),
            pkg("zstd", "0.13.3")
        ));
        let changes = compare_lockfiles(&base, &head, &members(&["libdd-trace-utils"]));
        assert_eq!(changes.packages, set(&[]));
        assert!(!changes.unattributable);
        assert!(!changes.is_changed());
    }

    #[test]
    fn duplicate_same_name_entries_compare_as_a_list() {
        let base = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n",
        );
        let head = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.5.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n",
        );
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["proptest"]));
        assert!(!changes.unattributable);

        // Reordering duplicate-name entries is conservatively treated as a change.
        let reordered = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n",
        );
        let changes = compare_lockfiles(&base, &reordered, &members(&[]));
        assert_eq!(changes.packages, set(&["proptest"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn top_level_version_change_is_unattributable() {
        let packages = format!("{}{}", pkg("zstd", "0.13.3"), pkg("serde", "1.0.210"));
        let base = format!("version = 3\n\n{packages}");
        let head = format!("version = 4\n\n{packages}");
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&[]));
        assert!(changes.unattributable);
        assert!(changes.is_changed());
    }

    #[test]
    fn patch_unused_change_is_unattributable() {
        let packages = pkg("zstd", "0.13.3");
        let base = lockfile(&packages);
        let head = format!("{packages}[[patch.unused]]\nname = \"proptest\"\n");
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&[]));
        assert!(changes.unattributable);
    }

    #[test]
    fn malformed_documents_on_either_side_are_unattributable() {
        let doc = lockfile(&pkg("zstd", "0.13.3"));
        for invalid in [
            "not toml [",
            "[[package]]\nversion = '0.14.3'",
            "package = 'not-an-array'",
        ] {
            for (base, head) in [(invalid, doc.as_str()), (doc.as_str(), invalid)] {
                let changes = compare_lockfiles(base, head, &set(&[]));
                assert_eq!(changes, unattributable());
                assert!(changes.is_changed());
            }
        }
    }

    #[test]
    fn empty_document_on_a_side_marks_every_package_and_is_unattributable() {
        let doc = lockfile(&format!(
            "{}{}",
            pkg("zstd", "0.13.3"),
            pkg("serde", "1.0.210")
        ));
        let changes = compare_lockfiles(&doc, "", &members(&[]));
        assert_eq!(changes.packages, set(&["serde", "zstd"]));
        assert!(changes.unattributable);
    }

    #[test]
    fn build_time_packages_escape_runtime_scoping() {
        let metadata = serde_json::json!({
            "workspace_members": ["path+file:///ws#libdd-x@0.1.0"],
            "packages": [
                {"id": "path+file:///ws#libdd-x@0.1.0", "name": "libdd-x", "dependencies": [
                    {"name": "zstd", "kind": null},
                    {"name": "criterion", "kind": "dev"},
                    {"name": "serde"},
                    {"name": "cc", "kind": "build"}
                ], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#zstd@0.13.3", "name": "zstd", "dependencies": [
                    {"name": "zstd-sys", "kind": null}
                ], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#zstd-sys@0.2.0", "name": "zstd-sys", "dependencies": [
                    {"name": "cc", "kind": "build"},
                    {"name": "libc"}
                ], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#libc@0.2.0", "name": "libc", "dependencies": [], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#criterion@0.5.0", "name": "criterion", "dependencies": [
                    {"name": "criterion-dep", "kind": null}
                ], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#criterion-dep@1.0.0", "name": "criterion-dep", "dependencies": [], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#cc@1.0.0", "name": "cc", "dependencies": [
                    {"name": "shlex", "kind": null}
                ], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#shlex@1.0.0", "name": "shlex", "dependencies": [], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#serde@1.0.0", "name": "serde", "dependencies": [
                    {"name": "serde_derive", "kind": null}
                ], "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///ws#serde_derive@1.0.0", "name": "serde_derive", "dependencies": [
                    {"name": "syn", "kind": null}
                ], "targets": [{"kind": ["proc-macro"]}]},
                {"id": "path+file:///ws#syn@2.0.0", "name": "syn", "dependencies": [], "targets": [{"kind": ["lib"]}]}
            ]
        });
        let scoped = |names: &[&str]| scopes_changed_packages(&metadata, &set(names));
        // Runtime-reachable packages (normal and dev edges, and their
        // dependencies) stay scoped.
        assert!(scoped(&["zstd"]));
        assert!(scoped(&["zstd-sys"]));
        assert!(scoped(&["libc"]));
        assert!(scoped(&["criterion"]));
        assert!(scoped(&["criterion-dep"]));
        assert!(scoped(&["zstd", "libc", "criterion"]));
        // Build-only packages and their dependencies widen: they can change
        // compiled code without ever appearing in executed-function coverage.
        assert!(!scoped(&["cc"]));
        assert!(!scoped(&["shlex"]));
        // Proc-macros and their dependencies widen for the same reason.
        assert!(!scoped(&["serde_derive"]));
        assert!(!scoped(&["syn"]));
        // One unscopable package widens the whole set.
        assert!(!scoped(&["zstd", "cc"]));
        // Packages absent from the resolved graph cannot be classified.
        assert!(!scoped(&["absent"]));
        // No workspace member resolves: nothing can be scoped.
        assert!(!scopes_changed_packages(
            &serde_json::json!({"packages": []}),
            &set(&["zstd"])
        ));
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A workspace member whose dependency edges exercise the runtime scoping.
    /// `excluded` keeps the path dependency out of the workspace: as a member
    /// it would be filtered out of the lockfile diff instead of being scoped.
    fn write_member(repo: &Path, dependencies: &str, excluded: &str) {
        std::fs::write(
            repo.join("Cargo.toml"),
            format!("[workspace]\nmembers = [\"libdd-x\"]\nexclude = [\"{excluded}\"]\n"),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("libdd-x/src")).unwrap();
        std::fs::write(repo.join("libdd-x/src/lib.rs"), "").unwrap();
        std::fs::write(
            repo.join("libdd-x/Cargo.toml"),
            format!("[package]\nname = \"libdd-x\"\nversion = \"0.1.0\"\n\n{dependencies}"),
        )
        .unwrap();
    }

    fn write_path_package(repo: &Path, name: &str) {
        std::fs::create_dir_all(repo.join(name).join("src")).unwrap();
        std::fs::write(repo.join(name).join("src/lib.rs"), "").unwrap();
        std::fs::write(
            repo.join(name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.0.1\"\n"),
        )
        .unwrap();
    }

    fn commit_base_with_lockfile(repo: &Path, lock: &str) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.name", "Review fixture"]);
        git(repo, &["config", "user.email", "review@example.invalid"]);
        std::fs::write(repo.join("Cargo.lock"), lock).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", "base"]);
        git(repo, &["branch", "base"]);
        git(repo, &["remote", "add", "origin", repo.to_str().unwrap()]);
    }

    #[test]
    fn lockfile_changes_reads_the_target_checkout_at_the_merge_base() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        // A runtime path dependency keeps the changed package scopable by
        // executed-function coverage.
        write_member(
            repo,
            "[dependencies]\nzstd = { path = \"../zstd\" }\n",
            "zstd",
        );
        write_path_package(repo, "zstd");
        commit_base_with_lockfile(repo, &lockfile(&pkg("zstd", "0.13.3")));
        std::fs::write(repo.join("Cargo.lock"), lockfile(&pkg("zstd", "0.14.3"))).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "bump zstd"]);
        crate::fetch_base("base", repo).unwrap();
        let changes = lockfile_changes("base", repo).unwrap();
        assert_eq!(changes.packages, set(&["zstd"]));
        assert!(!changes.unattributable);
        assert!(changes.is_changed());
    }

    #[test]
    fn lockfile_changes_to_build_time_packages_widen_the_run() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        // A build dependency such as `cc` never compiles into bench binaries,
        // so runtime coverage cannot scope it: the change widens the run.
        write_member(
            repo,
            "[build-dependencies]\ncc = { path = \"../cc\" }\n",
            "cc",
        );
        write_path_package(repo, "cc");
        commit_base_with_lockfile(repo, &lockfile(&pkg("cc", "1.0.106")));
        std::fs::write(repo.join("Cargo.lock"), lockfile(&pkg("cc", "1.0.107"))).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "bump cc"]);
        crate::fetch_base("base", repo).unwrap();
        let changes = lockfile_changes("base", repo).unwrap();
        assert_eq!(changes.packages, set(&["cc"]));
        assert!(changes.unattributable);
        assert!(changes.is_changed());
    }
}
