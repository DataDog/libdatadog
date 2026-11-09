// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Compare root lockfiles structurally at the merge base and HEAD, ignoring
//! formatting. Changed external package names are scoped by executed coverage;
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
    let changes = compare_lockfiles(&base, &head, &members);
    if changes.unattributable {
        log::info!("lockfile: unattributable change -> full run");
    } else if !changes.packages.is_empty() {
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
    fn lockfile_changes_reads_the_target_checkout_at_the_merge_base() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let git = |args: &[&str]| {
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
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Review fixture"]);
        git(&["config", "user.email", "review@example.invalid"]);
        let base_lock = lockfile(&format!(
            "{}{}",
            pkg("zstd", "0.13.3"),
            pkg("serde", "1.0.210")
        ));
        std::fs::write(repo.join("Cargo.lock"), base_lock).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["branch", "base"]);
        git(&["remote", "add", "origin", repo.to_str().unwrap()]);
        let head_lock = lockfile(&format!(
            "{}{}",
            pkg("zstd", "0.14.3"),
            pkg("serde", "1.0.210")
        ));
        std::fs::write(repo.join("Cargo.lock"), head_lock).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "bump zstd"]);
        crate::fetch_base("base", repo).unwrap();
        let changes = lockfile_changes("base", repo).unwrap();
        assert_eq!(changes.packages, set(&["zstd"]));
        assert!(!changes.unattributable);
        assert!(changes.is_changed());
    }
}
