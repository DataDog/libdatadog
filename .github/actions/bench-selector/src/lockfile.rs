// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Parsing of `Cargo.lock` diffs into the set of packages whose resolved
//! version (or source/checksum) changed.
//!
//! A lockfile bump changes the compiled code of exactly the named packages —
//! no workspace source line moves, so line coverage alone cannot see it.
//! The package names from this diff are intersected with the packages each
//! benchmark cluster actually *executes* (recorded during the coverage pass,
//! where every dependency is instrumented) to select only the benchmarks that
//! measure the changed dependencies, instead of re-running the whole suite.
//!
//! Path dependencies (workspace members) are excluded: their code changes come
//! with source diffs that the regular line intersection already handles.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Packages changed in the lockfile diff.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockfileChanges {
    /// External (registry/git) package names with a changed version, source,
    /// checksum, or dependency list. Workspace members are excluded.
    pub packages: BTreeSet<String>,
    /// True when the lockfile changed in a way the parser could not attribute
    /// to a package (e.g. a hunk whose context does not show the package
    /// name). The caller must then be conservative and select everything.
    pub unattributable: bool,
}

impl LockfileChanges {
    /// True when the lockfile diff changed anything.
    pub fn is_changed(&self) -> bool {
        !self.packages.is_empty() || self.unattributable
    }
}

/// Run `git diff <base>...HEAD -- Cargo.lock` in `workspace_root` and parse it.
pub fn lockfile_changes(base_ref: &str, workspace_root: &Path) -> Result<LockfileChanges> {
    let spec = format!("origin/{base_ref}...HEAD");
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["diff", "-U3", "--no-color", &spec, "--", "Cargo.lock"])
        .output()
        .context("failed to run git diff for Cargo.lock")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff for Cargo.lock failed: {stderr}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let members = workspace_members(workspace_root);
    Ok(parse_lockfile_diff(&stdout, &members))
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

/// Parse a `Cargo.lock` unified diff.
///
/// A `[[package]]` block is identified by its `name = "..."` key; every
/// changed (added/removed) line inside a block — version, source, checksum,
/// dependencies, or the name itself for added/removed packages — is attributed
/// to that package.
pub fn parse_lockfile_diff(diff: &str, workspace_members: &BTreeSet<String>) -> LockfileChanges {
    let mut changes = LockfileChanges::default();
    let mut current_package: Option<String> = None;

    for line in diff.lines() {
        if line.starts_with("diff --git")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("@@")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
        {
            continue;
        }
        let changed = matches!(line.chars().next(), Some('+' | '-'));
        let body = if changed { &line[1..] } else { line }.trim_start();

        // `name = "..."` identifies the enclosing package block; a changed
        // name line marks a package added, removed, or renamed (both sides of
        // a rename get attributed, as two separate lines).
        if let Some(name) = parse_kv(body, "name") {
            if changed && !workspace_members.contains(name) {
                changes.packages.insert(name.to_string());
            }
            current_package = Some(name.to_string());
            continue;
        }
        if changed {
            match &current_package {
                Some(name) if !workspace_members.contains(name) => {
                    changes.packages.insert(name.clone());
                }
                // A changed line with no enclosing package name in view: the
                // hunk context cut the block header off. Be conservative.
                Some(_) => {}
                None => changes.unattributable = true,
            }
        }
    }
    changes
}

/// Parse a `key = "value"` line, returning the value when the key matches.
fn parse_kv<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = body.strip_prefix(key)?.strip_prefix(" = \"")?;
    rest.strip_suffix('"')
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

    #[test]
    fn empty_diff_is_no_change() {
        let changes = parse_lockfile_diff("", &members(&[]));
        assert_eq!(changes, LockfileChanges::default());
        assert!(!changes.is_changed());
    }

    #[test]
    fn version_bump_is_attributed() {
        let diff = "\
diff --git a/Cargo.lock b/Cargo.lock
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1200,7 +1200,7 @@
 [[package]]
 name = \"zstd\"
-version = \"0.13.3\"
+version = \"0.14.3\"
 source = \"registry+https://github.com/rust-lang/crates.io-index\"
 checksum = \"...\"
";
        let changes = parse_lockfile_diff(diff, &members(&[]));
        assert_eq!(changes.packages, set(&["zstd"]));
        assert!(!changes.unattributable);
    }

    #[test]
    fn workspace_members_are_excluded() {
        let diff = "\
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1,4 +1,4 @@
 [[package]]
 name = \"libdd-trace-utils\"
-version = \"15.0.0\"
+version = \"15.1.0\"
";
        let changes = parse_lockfile_diff(diff, &members(&["libdd-trace-utils"]));
        assert_eq!(changes.packages, set(&[]));
        assert!(!changes.is_changed());
    }

    #[test]
    fn added_and_removed_packages_are_attributed() {
        let diff = "\
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1,5 +1,9 @@
+[[package]]
+name = \"paste\"
+version = \"1.0.15\"
+source = \"registry+https://github.com/rust-lang/crates.io-index\"
+
 [[package]]
 name = \"unicode-ident\"
-version = \"1.0.12\"
+version = \"1.0.13\"
";
        let changes = parse_lockfile_diff(diff, &members(&[]));
        assert_eq!(changes.packages, set(&["paste", "unicode-ident"]));
    }

    #[test]
    fn unattributable_hunk_is_conservative() {
        // The hunk starts mid-block, below the name line.
        let diff = "\
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -5,3 +5,3 @@
-version = \"0.1.0\"
+version = \"0.2.0\"
";
        let changes = parse_lockfile_diff(diff, &members(&[]));
        assert_eq!(changes.packages, set(&[]));
        assert!(changes.unattributable);
        assert!(changes.is_changed());
    }

    #[test]
    fn dependency_list_changes_are_attributed() {
        let diff = "\
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -9,4 +9,5 @@
 name = \"serde\"
 version = \"1.0.210\"
 dependencies = [
+ \"serde_derive\",
 ]
";
        let changes = parse_lockfile_diff(diff, &members(&[]));
        assert_eq!(changes.packages, set(&["serde"]));
    }

    #[test]
    fn checksum_and_source_changes_are_attributed() {
        let diff = "\
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -3,3 +3,3 @@
 name = \"proptest\"
-source = \"git+https://github.com/x/proptest?branch=a#1111\"
+source = \"git+https://github.com/x/proptest?branch=b#2222\"
";
        let changes = parse_lockfile_diff(diff, &members(&[]));
        assert_eq!(changes.packages, set(&["proptest"]));
    }

    #[test]
    fn member_named_block_after_member_change_is_still_excluded() {
        // The [[package]] header line is context; the name line changes.
        let diff = "\
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -2,3 +2,3 @@
 [[package]]
-name = \"libdd-common\"
+name = \"libdd-common\"
 version = \"9.0.0\"
";
        let changes = parse_lockfile_diff(diff, &members(&["libdd-common"]));
        assert_eq!(changes.packages, set(&[]));
        assert!(!changes.unattributable);
    }
}
