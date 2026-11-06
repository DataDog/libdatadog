// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Structural comparison of the root `Cargo.lock` between the merge base
//! and the PR head, into the set of packages whose resolved version (or
//! source/checksum/dependencies) changed.
//!
//! Both sides are fetched with git (`git show <rev>:Cargo.lock`) and parsed
//! with a real TOML parser; the decision compares the parsed `[[package]]`
//! entries, never the textual unified diff — comments, key order, and entry
//! formatting compare equal, and attribution cannot be fooled by hunk
//! context the way diff parsing can.
//!
//! A lockfile bump changes the compiled code of exactly the named packages —
//! no workspace source line moves, so line coverage alone cannot see it.
//! The package names from this comparison are intersected with the packages
//! each benchmark cluster actually *executes* (recorded during the coverage
//! pass, where every dependency is instrumented) to select only the
//! benchmarks that measure the changed dependencies, instead of re-running
//! the whole suite.
//!
//! Path dependencies (workspace members) are excluded: their code changes come
//! with source diffs that the regular line intersection already handles.
//!
//! Fail-conservative: a lockfile missing or unparseable on either side, a
//! change outside the package entries (lockfile format `version`,
//! `[[patch.unused]]`, workspace membership), or any git failure is
//! `unattributable` — the caller then conservatively selects everything.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use toml::{Table, Value};

/// Packages changed in the lockfile.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockfileChanges {
    /// External (registry/git) package names with a changed version, source,
    /// checksum, or dependency list. Workspace members are excluded.
    pub packages: BTreeSet<String>,
    /// True when the lockfile changed in a way the comparison could not
    /// attribute to packages (lockfile missing or unparseable on either
    /// side, a change outside the `[[package]]` entries). The caller must
    /// then be conservative and select everything.
    pub unattributable: bool,
}

impl LockfileChanges {
    /// True when the lockfile changed anything.
    pub fn is_changed(&self) -> bool {
        !self.packages.is_empty() || self.unattributable
    }
}

/// The conservative verdict: something changed that packages cannot scope.
fn unattributable() -> LockfileChanges {
    LockfileChanges {
        packages: BTreeSet::new(),
        unattributable: true,
    }
}

/// Compare the root `Cargo.lock` at the merge base with `origin/<base_ref>`
/// and at HEAD, structurally. Git failures, a lockfile missing on either
/// side, or anything unparseable is swallowed with a warning and reported
/// as unattributable — the conservative full run — instead of an error.
pub fn lockfile_changes(base_ref: &str, workspace_root: &Path) -> Result<LockfileChanges> {
    let members = workspace_members(workspace_root);

    // Compare against the MERGE-BASE with the base branch, matching the
    // three-dot semantics of every other diff in the pipeline: `origin/main`
    // itself has moved on since the PR branched, and its unrelated lockfile
    // bumps would wrongly widen the selection.
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
    // A lockfile appearing or disappearing cannot be attributed to the
    // packages of either side alone.
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

/// Compare the base and head lockfile documents. Pure: the git plumbing
/// lives in [`lockfile_changes`], so tests can compare documents directly.
fn compare_lockfiles(base: &str, head: &str, members: &BTreeSet<String>) -> LockfileChanges {
    // An unparseable document on either side is never scoping-safe.
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

    // Everything outside the package entries — the lockfile format version,
    // patches, workspace membership — differing while all package entries
    // are identical cannot be attributed to any package.
    if stripped(&base) != stripped(&head) {
        changes.unattributable = true;
    }
    changes
}

/// Collect the `[[package]]` entries of a parsed lockfile, keyed by package
/// name. A lockfile can hold two entries with the same name (different
/// version or source), so a name maps to its full ordered entry list: any
/// difference in that list marks the name changed — a field-level diff is
/// unnecessary when the whole entry compares.
///
/// `None` — and with it the conservative verdict — when the `package` data
/// is not the expected array of named tables: cargo never writes such a
/// lockfile, so its changes cannot be attributed.
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

/// A copy of the document with the `[[package]]` entries removed, so the
/// remaining top-level data — lockfile format version, patches, workspace
/// membership — can be compared structurally. Everything that remains must
/// be identical between the two sides; a difference there cannot be
/// attributed to any package.
fn stripped(doc: &Table) -> Table {
    doc.iter()
        .filter(|(key, _)| key.as_str() != "package")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Workspace member names, for excluding path dependencies from the results.
/// Cargo metadata is the source of truth; failure falls back to no exclusions
/// (over-selecting is safe, under-selecting is not).
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

    fn members(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

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
    fn unchanged_lockfile_is_no_change() {
        let doc = lockfile(&format!(
            "{}{}",
            pkg("zstd", "0.13.3"),
            pkg("serde", "1.0.210")
        ));
        let changes = compare_lockfiles(&doc, &doc, &members(&[]));
        assert_eq!(changes, LockfileChanges::default());
        assert!(!changes.is_changed());
    }

    #[test]
    fn formatting_only_differences_compare_equal() {
        // Same resolved data, different text: added comments and reordered
        // entry keys are invisible to the structural comparison.
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
    fn version_bump_is_attributed() {
        let base = lockfile(&pkg("zstd", "0.13.3"));
        let head = lockfile(&pkg("zstd", "0.14.3"));
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["zstd"]));
        assert!(!changes.unattributable);
        assert!(changes.is_changed());
    }

    #[test]
    fn added_package_is_attributed() {
        let base = lockfile(&pkg("unicode-ident", "1.0.12"));
        let head = lockfile(&format!(
            "{}{}",
            pkg("paste", "1.0.15"),
            pkg("unicode-ident", "1.0.12")
        ));
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["paste"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn removed_package_is_attributed() {
        let base = lockfile(&format!(
            "{}{}",
            pkg("paste", "1.0.15"),
            pkg("unicode-ident", "1.0.12")
        ));
        let head = lockfile(&pkg("unicode-ident", "1.0.12"));
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["paste"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn dependency_list_change_is_attributed() {
        let base = lockfile(
            "[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\ndependencies = [\n \"itoa\",\n]\n\n",
        );
        let head = lockfile(
            "[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\ndependencies = [\n \"itoa\",\n \"ryu\",\n]\n\n",
        );
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["serde"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn source_change_is_attributed() {
        let base = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n",
        );
        let head = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"git+https://github.com/x/proptest?branch=b#2222\"\n\n",
        );
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["proptest"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn checksum_change_is_attributed() {
        let base =
            lockfile("[[package]]\nname = \"zstd\"\nversion = \"0.13.3\"\nchecksum = \"aaa\"\n\n");
        let head =
            lockfile("[[package]]\nname = \"zstd\"\nversion = \"0.13.3\"\nchecksum = \"bbb\"\n\n");
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["zstd"]));
        assert!(!changes.unattributable);
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
        // The same name resolved twice (git override + registry): bumping
        // either entry marks the name changed.
        let base = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n",
        );
        let head = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.5.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n",
        );
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes.packages, set(&["proptest"]));
        assert!(!changes.unattributable);

        // Identical duplicates, reordered: the ordered entry list differs,
        // and the name is marked changed (conservative).
        let reordered = lockfile(
            "[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"proptest\"\nversion = \"1.4.0\"\nsource = \"git+https://github.com/x/proptest?branch=a#1111\"\n\n",
        );
        let changes = compare_lockfiles(&base, &reordered, &members(&[]));
        assert_eq!(changes.packages, set(&["proptest"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn top_level_version_change_is_unattributable() {
        // Lockfile format version bump with identical package entries.
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
    fn malformed_toml_on_either_side_is_unattributable() {
        let doc = lockfile(&pkg("zstd", "0.13.3"));
        let changes = compare_lockfiles("not toml [", &doc, &members(&[]));
        assert_eq!(changes, unattributable());
        assert!(changes.is_changed());
        let changes = compare_lockfiles(&doc, "not toml [", &members(&[]));
        assert_eq!(changes, unattributable());
    }

    #[test]
    fn empty_document_on_a_side_marks_every_package_and_is_unattributable() {
        // A missing lockfile at the fetch level reports unattributable
        // directly; at the document level, the empty side removes every
        // package (correct attribution) and drops the format version
        // (unattributable, conservative).
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
    fn nameless_package_entry_is_unattributable() {
        let base = lockfile(&pkg("zstd", "0.13.3"));
        let head = lockfile("[[package]]\nversion = \"0.14.3\"\n\n");
        let changes = compare_lockfiles(&base, &head, &members(&[]));
        assert_eq!(changes, unattributable());
    }

    #[test]
    fn non_array_package_data_is_unattributable() {
        let base = lockfile(&pkg("zstd", "0.13.3"));
        let head = "version = 4\n\npackage = \"not-an-array\"\n";
        let changes = compare_lockfiles(&base, head, &members(&[]));
        assert_eq!(changes, unattributable());
    }

    // Integration-style: the fetch plumbing reads the lockfile of the
    // target checkout at the merge base, like the manifest path.
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
