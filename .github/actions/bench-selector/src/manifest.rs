// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Parsing of `Cargo.toml` diffs into "this manifest changed only
//! dependencies" and the packages whose wiring changed.
//!
//! A manifest change that only touches dependency declarations (version,
//! source, features, or `package =` rename of a normal/dev dependency, or a
//! `[workspace.dependencies]` entry) is scoped exactly like a lockfile bump:
//! the changed package names are intersected with the packages each benchmark
//! cluster executes (see `coverage::CoverageEntry::deps`), so only the
//! benchmarks actually measuring the changed dependency run.
//!
//! Anything else in the manifest — the crate's own `[features]`, `[profile]`,
//! `[package]` metadata, `build-dependencies`, or a dependency that is a
//! proc-macro (compile-time effect, invisible to runtime line coverage) —
//! keeps the conservative full run: line coverage cannot attribute those.
//!
//! Path dependencies (workspace members) are excluded from the package set:
//! their code changes arrive as source diffs the regular line intersection
//! already handles, and their wiring is visible in the lockfile.

use crate::diff::ChangedLines;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Dependency-scoping analysis of the changed manifests.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestChanges {
    /// Per changed manifest path: true when every changed line in it is a
    /// dependency declaration that runtime coverage can scope. False keeps
    /// the conservative full run for that manifest.
    pub deps_only: BTreeMap<String, bool>,
    /// External packages whose wiring changed anywhere (dep added, removed,
    /// re-required, or re-versioned in a deps-only manifest). Workspace
    /// members (path dependencies) are excluded.
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
/// is diffed against `origin/<base>...HEAD` and parsed.
///
/// Any failure (git, metadata, parsing) is conservative: the affected
/// manifests are marked not-deps-only, i.e. they fall back to the full run.
pub fn manifest_changes(
    base_ref: &str,
    workspace_root: &Path,
    changed: &ChangedLines,
) -> ManifestChanges {
    let manifests: Vec<&String> = changed.keys().filter(|path| is_manifest(path)).collect();
    let mut out = ManifestChanges::default();
    if manifests.is_empty() {
        return out;
    }
    // Mark everything conservative up front; each successfully parsed
    // deps-only manifest flips its entry to true.
    for path in &manifests {
        out.deps_only.insert((*path).clone(), false);
    }

    let spec = format!("origin/{base_ref}...HEAD");
    let mut cmd = Command::new("git");
    cmd.current_dir(workspace_root);
    // Whole-file context (-U200): a mid-table hunk would otherwise lack its
    // section header and could not be attributed safely.
    cmd.arg("diff")
        .arg("-U200")
        .arg("--no-color")
        .arg(&spec)
        .arg("--");
    cmd.args(manifests.iter().map(|p| p.as_str()));
    let Ok(output) = cmd.output() else {
        return out;
    };
    if !output.status.success() {
        return out;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Split the multi-file diff into per-file sections and parse each.
    let mut file: Option<String> = None;
    let mut lines: Vec<String> = Vec::new();
    let flush =
        |file: &mut Option<String>, lines: &mut Vec<String>, out: &mut ManifestChanges| {
            if let Some(path) = file.take() {
                let parsed = parse_manifest_diff(lines.join("\n").as_str());
                out.deps_only.insert(path.clone(), parsed.deps_only);
                out.packages.extend(parsed.packages);
            }
            lines.clear();
        };
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            flush(&mut file, &mut lines, &mut out);
            let path = rest.split(' ').next().unwrap_or(rest);
            // Strip the b/ side prefix of "path b/path".
            file = Some(path.strip_prefix("b/").unwrap_or(path).to_string());
            continue;
        }
        lines.push(line.to_string());
    }
    flush(&mut file, &mut lines, &mut out);

    // Dependencies whose code runs at compile time (proc-macros) have no
    // runtime coverage to scope with: treat their manifests as conservative.
    if !out.packages.is_empty() && workspace_has_proc_macro(workspace_root, &out.packages) {
        for path in manifests {
            out.deps_only.insert((*path).clone(), false);
        }
    }
    out
}

/// A path is a manifest candidate when it is a Cargo.toml of the workspace —
/// not the CI helper workspace's, which cannot affect the benchmarked crates.
fn is_manifest(path: &str) -> bool {
    path == "Cargo.toml" || (path.ends_with("/Cargo.toml") && !path.starts_with(".github/actions/"))
}

/// The result of parsing one manifest diff.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedManifest {
    deps_only: bool,
    packages: BTreeSet<String>,
}

/// Parse a single `Cargo.toml` unified diff.
fn parse_manifest_diff(diff: &str) -> ParsedManifest {
    let mut result = ParsedManifest {
        deps_only: true,
        packages: BTreeSet::new(),
    };
    let mut section: Option<String> = None;
    // The dependency declaration the current line belongs to (for multi-line
    // inline tables), tracked across context and changed lines alike.
    let mut current_dep: Option<String> = None;

    for line in diff.lines() {
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("@@")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("similarity ")
            || line.starts_with("rename ")
        {
            continue;
        }
        let changed = matches!(line.chars().next(), Some('+' | '-'));
        let body = if changed { &line[1..] } else { line };
        let body = body.trim_start();

        // Comments never change compilation.
        if body.starts_with('#') {
            continue;
        }

        // Section headers: `[[...]]` or `[...]`, possibly added/removed.
        if body.starts_with('[') {
            let inner = body.trim_start_matches('[').trim_end_matches(']');
            section = Some(inner.to_string());
            current_dep = None;
            if changed && !is_dependency_section(inner) {
                // Adding or removing a non-dependency section (features,
                // profile, benches, ...) is not scoping-safe.
                result.deps_only = false;
            }
            continue;
        }

        let dep_section = section.as_deref().is_some_and(is_dependency_section);
        // `name = ...` starts a declaration.
        if let Some((name, _)) = parse_declaration(body) {
            if dep_section {
                current_dep = Some(name.to_string());
                if changed {
                    result.packages.insert(name.to_string());
                }
            } else {
                // A changed declaration outside dependency tables is never
                // scoping-safe — [package] metadata, [features], build
                // dependencies (not a scoping section), anything else.
                if changed {
                    result.deps_only = false;
                }
                current_dep = None;
            }
            continue;
        }

        if changed {
            if !dep_section {
                // A changed line outside dependency tables: features,
                // profile values, package metadata, anything else.
                result.deps_only = false;
            } else if let Some(dep) = &current_dep {
                // Continuation of a dependency declaration (multi-line inline
                // table: features lists, etc.) — still a dependency change.
                result.packages.insert(dep.clone());
            } else {
                // Changed line in a dependency section with no declaration in
                // view: the hunk context cut it off.
                result.deps_only = false;
            }
        }
    }
    result
}

/// Dependency tables whose changes runtime coverage can scope. Build
/// dependencies are deliberately NOT dependency sections (their effect is
/// at compile time), so changes there disqualify conservatively.
fn is_dependency_section(section: &str) -> bool {
    let last = section.split('.').next_back().unwrap_or(section);
    matches!(last, "dependencies" | "dev-dependencies")
        || section == "workspace.dependencies"
        || section.starts_with("workspace.dependencies.")
        // Target-scoped tables: [target.'cfg(unix)'.dependencies] etc.
        || section.contains(".dependencies")
        || section.contains(".dev-dependencies")
}

/// Parse a `name = ...` TOML key line, returning the key and the rest.
/// Handles `package = "..."` renames inside inline tables on the same line.
fn parse_declaration(body: &str) -> Option<(&str, &str)> {
    let (name, rest) = body.split_once(" = ")?;
    if name.contains('.') || name.contains('[') || name.is_empty() || !is_key_like(name) {
        return None;
    }
    Some((name, rest))
}

fn is_key_like(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

    #[test]
    fn empty_diff_is_vacuously_deps_only() {
        let parsed = parse_manifest_diff("");
        assert!(parsed.deps_only);
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn version_bump_is_deps_only() {
        let diff = "\
diff --git a/libdd-x/Cargo.toml b/libdd-x/Cargo.toml
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -20,7 +20,7 @@ license = \"Apache-2.0\"
 [dependencies]
-serde = \"1.0.200\"
+serde = \"1.0.210\"
 anyhow.workspace = true
";
        let parsed = parse_manifest_diff(diff);
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, ["serde".to_string()].into_iter().collect());
    }

    #[test]
    fn added_dependency_is_deps_only() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -25,3 +25,4 @@
 [dev-dependencies]
 criterion = \"0.5\"
+allocator-api2 = \"0.2\"
";
        let parsed = parse_manifest_diff(diff);
        assert!(parsed.deps_only);
        assert_eq!(
            parsed.packages,
            ["allocator-api2".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn multiline_feature_change_attributes_to_the_dep() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -1,8 +1,8 @@
 [package]
 name = \"libdd-x\"

 [dependencies]
 serde = { version = \"1\", features = [
-  \"derive\",
+  \"derive\", \"rc\",
 ] }
";
        let parsed = parse_manifest_diff(diff);
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, ["serde".to_string()].into_iter().collect());
    }

    #[test]
    fn crate_features_change_is_not_deps_only() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -40,3 +40,6 @@
 [features]
 default = [\"v04\"]
+v05 = []
+
+[profile.release]
";
        let parsed = parse_manifest_diff(diff);
        assert!(!parsed.deps_only);
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn package_metadata_change_is_not_deps_only() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -3,3 +3,3 @@
 [package]
-version = \"1.0.0\"
+version = \"1.0.1\"
";
        let parsed = parse_manifest_diff(diff);
        assert!(!parsed.deps_only);
    }

    #[test]
    fn build_dependencies_are_not_scoping_safe() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -30,3 +30,4 @@
 [build-dependencies]
 cc = \"1\"
+anyhow = \"1\"
";
        let parsed = parse_manifest_diff(diff);
        assert!(!parsed.deps_only);
    }

    #[test]
    fn workspace_dependencies_section_is_dep_section() {
        let diff = "\
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -60,7 +60,7 @@
 [workspace.dependencies]
-zstd = \"0.13\"
+zstd = \"0.14\"
";
        let parsed = parse_manifest_diff(diff);
        assert!(parsed.deps_only);
        assert_eq!(parsed.packages, ["zstd".to_string()].into_iter().collect());
    }

    #[test]
    fn target_scoped_dependencies_are_dep_sections() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -30,3 +30,4 @@
 [target.'cfg(unix)'.dependencies]
 libc = \"0.2\"
+memmap2 = \"0.9\"
";
        let parsed = parse_manifest_diff(diff);
        assert!(parsed.deps_only);
        assert_eq!(
            parsed.packages,
            ["memmap2".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn comments_never_disqualify() {
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -20,3 +20,4 @@
 [dependencies]
+# TODO: bump serde
 serde = \"1\"
";
        let parsed = parse_manifest_diff(diff);
        assert!(parsed.deps_only);
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn dep_context_in_other_sections_does_not_record() {
        // A changed line under a non-dependency section is not a package.
        let diff = "\
--- a/libdd-x/Cargo.toml
+++ b/libdd-x/Cargo.toml
@@ -60,3 +60,4 @@
 [package.metadata.docs.rs]
+targets = [\"x86_64\"]
";
        let parsed = parse_manifest_diff(diff);
        assert!(!parsed.deps_only);
        assert!(parsed.packages.is_empty());
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
