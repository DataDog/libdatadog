// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Selection of bench clusters to run, by intersecting each cluster's
//! executed-function line ranges with the PR's changed lines.
//!
//! Fail-conservative rules:
//!
//! * A cluster with no coverage entry, or a `failed` coverage entry, always runs (never skip on
//!   missing evidence).
//! * A binary whose bench sources (`<crate>/benches/`) changed runs all of its clusters — the
//!   bench's own code, and thus its methodology, changed.
//! * Changes to dependency, build, toolchain or benchmark infrastructure inputs, native source, and
//!   Rust paths without candidate ranges run all clusters: Rust line coverage cannot rule out an
//!   effect from these inputs.
//! * A cluster is selected when any of its covered files has an executed range overlapping a
//!   changed line range.
//!
//! The output filter for the measuring run matches exactly the selected
//! benchmark IDs (`^(id1|id2|…)$`), so what runs is exact even though the
//! clustering heuristic may be coarser than criterion's true groups.

use crate::coverage::Coverage;
use crate::diff::ChangedLines;
use crate::inventory::Inventory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct Selection {
    pub binaries: Vec<SelectedBinary>,
    pub summary: Summary,
    /// True when a changed input cannot be scoped below the whole suite —
    /// currently a non-dependency-only ROOT manifest change (profiles,
    /// membership, features of the workspace itself). The benchmarks job
    /// turns this into the full suite; cluster-level reasons cannot express
    /// it because the crate-detection maps root manifests to no crate, so no
    /// inventory would exist to mark.
    #[serde(default)]
    pub full_suite: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectedBinary {
    pub crate_name: String,
    pub target: String,
    pub features: Vec<String>,
    /// Selected benchmark IDs (exact).
    pub bench_ids: Vec<String>,
    /// Regex filter matching exactly `bench_ids`, for the measuring run.
    pub filter: String,
    pub clusters: Vec<SelectedCluster>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectedCluster {
    pub name: String,
    pub bench_ids: Vec<String>,
    pub reason: Reason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// An executed function of the cluster overlaps a changed line.
    CoveredChange,
    /// The binary's bench sources changed.
    BenchSourceChanged,
    /// The cluster has no coverage record (e.g. added by this PR).
    NoCoverage,
    /// The coverage pass failed for this cluster.
    CoverageFailed,
    /// An input not attributable by Rust line coverage changed.
    UncoveredInputChanged,
    /// A dependency the cluster executes was changed in the lockfile diff.
    DependencyChanged,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Summary {
    pub binaries_total: usize,
    pub binaries_selected: usize,
    pub clusters_total: usize,
    pub clusters_selected: usize,
}

/// Does this cluster execute a dependency that the lockfile diff changed?
/// An unattributable lockfile diff conservatively matches every cluster: the
/// parser could not tell which packages moved, so none can be excluded.
fn dependency_exercised(
    entry: &crate::coverage::CoverageEntry,
    changed_packages: &std::collections::BTreeSet<String>,
    unattributable: bool,
) -> bool {
    if changed_packages.is_empty() && !unattributable {
        return false;
    }
    if unattributable {
        return true;
    }
    entry.deps.iter().any(|dep| changed_packages.contains(dep))
}

/// Do two inclusive ranges overlap?
fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// Compute the selection for every binary of `inventory`.
pub fn select(
    inventory: &Inventory,
    coverage: &Coverage,
    changed: &ChangedLines,
    lockfile: &crate::lockfile::LockfileChanges,
    manifests: &crate::manifest::ManifestChanges,
    self_test: bool,
) -> Selection {
    let coverage_by_key: BTreeMap<(&str, &str, &str), &crate::coverage::CoverageEntry> = coverage
        .entries
        .iter()
        .map(|e| {
            (
                (e.crate_name.as_str(), e.target.as_str(), e.cluster.as_str()),
                e,
            )
        })
        .collect();

    // Lockfile bumps and dependency-only manifest changes feed the same
    // signal: the packages whose code (or wiring) changed, intersected with
    // the packages each cluster executes.
    let mut changed_packages = lockfile.packages.clone();
    changed_packages.extend(manifests.packages.iter().cloned());
    let unattributable = lockfile.unattributable;

    // A changed manifest that is NOT dependencies-only is unscopable by
    // coverage (its own features, profiles, metadata) and keeps the full-run
    // treatment. A dependencies-only manifest is scoped like a lockfile
    // bump instead. Unanalyzed manifests keep requires_full_run's default.
    let full_run = changed.iter().any(|(path, ranges)| {
        if manifests.deps_only.contains_key(path) {
            return !manifests.is_deps_only(path);
        }
        requires_full_run(path, ranges, self_test)
    });
    // A non-dependency-only ROOT manifest (profiles, workspace features,
    // membership) can affect every crate in the workspace, and the crate
    // detection maps root manifests to no crate — so no inventory scope
    // could ever express it. Signal the benchmarks job to run everything.
    let full_suite =
        !manifests.is_deps_only("Cargo.toml") && manifests.deps_only.contains_key("Cargo.toml");
    let mut binaries = Vec::new();
    let mut clusters_total = 0usize;
    let mut clusters_selected = 0usize;

    for bin in &inventory.binaries {
        // Bench sources changed: run the whole binary.
        let bench_dir = bin.bench_source_dir();
        let bench_source_changed = changed
            .keys()
            .any(|file| file == &bench_dir || file.starts_with(&format!("{bench_dir}/")));

        let mut selected_clusters = Vec::new();
        for cluster in bin.clusters() {
            clusters_total += 1;
            let reason = if bench_source_changed {
                Some(Reason::BenchSourceChanged)
            } else if full_run {
                Some(Reason::UncoveredInputChanged)
            } else {
                match coverage_by_key.get(&(
                    bin.crate_name.as_str(),
                    bin.target.as_str(),
                    cluster.name.as_str(),
                )) {
                    None => Some(Reason::NoCoverage),
                    Some(entry) if entry.status != "ok" || entry.files.is_empty() => {
                        Some(Reason::CoverageFailed)
                    }
                    Some(entry) => {
                        if cluster_touches_changes(&entry.files, changed) {
                            Some(Reason::CoveredChange)
                        } else if dependency_exercised(entry, &changed_packages, unattributable) {
                            Some(Reason::DependencyChanged)
                        } else {
                            None
                        }
                    }
                }
            };
            if let Some(reason) = reason {
                clusters_selected += 1;
                selected_clusters.push(SelectedCluster {
                    name: cluster.name.clone(),
                    bench_ids: cluster.bench_ids.clone(),
                    reason,
                });
            }
        }

        if selected_clusters.is_empty() {
            continue;
        }
        let mut bench_ids: Vec<String> = selected_clusters
            .iter()
            .flat_map(|c| c.bench_ids.iter().cloned())
            .collect();
        bench_ids.sort();
        bench_ids.dedup();
        binaries.push(SelectedBinary {
            crate_name: bin.crate_name.clone(),
            target: bin.target.clone(),
            features: bin.features.clone(),
            filter: crate::cluster::exact_filter(&bench_ids),
            bench_ids,
            clusters: selected_clusters,
        });
    }

    Selection {
        summary: Summary {
            binaries_total: inventory.binaries.len(),
            binaries_selected: binaries.len(),
            clusters_total,
            clusters_selected,
        },
        full_suite,
        binaries,
    }
}

/// Inputs outside Rust runtime coverage need a conservative full run. This
/// includes the existing benchmark script's infrastructure/toolchain fallback.
///
/// `self_test` skips the benchmark-tooling paths (not the C/proto/toolchain
/// ones): it exists only for validating the selection itself on PRs whose diff
/// contains the tooling changes, which would otherwise always force a full
/// suite and make the selection impossible to measure. It is enabled via the
/// `BENCH_SELECTOR_SELF_TEST` environment variable, which only a deliberate
/// CI configuration change can set (and editing the CI configuration is
/// itself a tooling change that re-enables the full-suite guard on real PRs).
fn requires_full_run(path: &str, ranges: &[(u32, u32)], self_test: bool) -> bool {
    // The CI helper workspace (.github/actions) is a separate workspace the
    // benchmarked crates never build against; its manifests and build
    // scripts cannot affect the benchmarks, so they never force a full run.
    // bench-selector itself is exempt from that exemption outside self-test:
    // a change to the selection tooling must validate against the full
    // suite (mirrors the guard in select_bench_packages.sh). Under self-test
    // the whole directory is the tooling under validation and is ignored.
    let helper_workspace = path.starts_with(".github/actions/") || path == ".github/actions";
    let tooling = path.starts_with(".github/actions/bench-selector/");
    if helper_workspace && (self_test || !tooling) {
        return false;
    }
    let path_obj = std::path::Path::new(path);
    let name = path_obj.file_name().and_then(|name| name.to_str());
    if matches!(
        name,
        Some(
            "Cargo.toml"
                | "build.rs"
                | "rust-toolchain"
                | "rust-toolchain.toml"
                | "ci-toolchain.toml"
        )
    ) || path.starts_with(".cargo/")
        || path.contains("/.cargo/")
        || (path.starts_with("benchmark/") && !self_test)
        || (path.starts_with(".github/actions/bench-selector/") && !self_test)
        || (matches!(
            path,
            ".gitlab/benchmarks.yml" | ".gitlab/impacted-crates.yml"
        ) && !self_test)
    {
        return true;
    }
    match path_obj.extension().and_then(|ext| ext.to_str()) {
        // Root Cargo.lock is not here: a lockfile bump changes exactly the
        // packages named in its diff, which the caller scopes by intersecting
        // the changed packages with the packages each cluster executes
        // (Reason::DependencyChanged). It still forces a full run when the
        // diff cannot be attributed to packages (see lockfile.rs), when a
        // member manifest or the root Cargo.toml changes (profiles, feature
        // toggles — nothing in the lockfile names them), or when no selection
        // is available at all (the CI fallback path).
        Some(
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "s" | "S" | "asm" | "m"
            | "mm" | "inc" | "proto",
        ) => true,
        // Deleted/renamed/mode-only Rust paths have no trustworthy candidate
        // range; a candidate coverage map need not contain the old path at all.
        Some("rs") => ranges.is_empty(),
        _ => false,
    }
}

/// Does any executed-function range of this cluster overlap a changed range?
fn cluster_touches_changes(
    covered: &BTreeMap<String, Vec<(u32, u32)>>,
    changed: &ChangedLines,
) -> bool {
    covered.iter().any(|(file, ranges)| {
        changed
            .get(file)
            .map(|changed_ranges| {
                ranges
                    .iter()
                    .any(|covered| changed_ranges.iter().any(|c| overlaps(*covered, *c)))
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::clusters_of;
    use crate::coverage::{Coverage, CoverageEntry};
    use crate::diff::LineRange;
    use crate::inventory::{BenchBinary, Inventory};
    use crate::lockfile::LockfileChanges;
    use crate::manifest::ManifestChanges;
    use std::collections::BTreeSet;

    fn binary(ids: &[&str]) -> BenchBinary {
        let bench_ids = ids.iter().map(|s| s.to_string()).collect();
        BenchBinary {
            crate_name: "libdd-x".into(),
            target: "main".into(),
            crate_dir: "libdd-x".into(),
            features: vec![],
            bench_ids,
        }
    }

    fn entry(cluster: &str, files: Vec<(&str, Vec<LineRange>)>) -> CoverageEntry {
        entry_with_deps(cluster, files, Vec::new())
    }

    fn entry_with_deps(
        cluster: &str,
        files: Vec<(&str, Vec<LineRange>)>,
        deps: Vec<&str>,
    ) -> CoverageEntry {
        CoverageEntry {
            crate_name: "libdd-x".into(),
            target: "main".into(),
            cluster: cluster.into(),
            status: "ok".into(),
            files: files.into_iter().map(|(f, r)| (f.to_string(), r)).collect(),
            deps: deps.into_iter().map(|d| d.to_string()).collect(),
        }
    }

    fn coverage(entries: Vec<CoverageEntry>) -> Coverage {
        Coverage { entries }
    }

    fn changed(files: Vec<(&str, Vec<LineRange>)>) -> ChangedLines {
        files.into_iter().map(|(f, r)| (f.to_string(), r)).collect()
    }

    #[test]
    fn selects_cluster_covering_changed_line() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/hot.rs", vec![(10, 20)])]),
            entry("b", vec![("libdd-x/src/cold.rs", vec![(1, 5)])]),
        ]);
        let ch = changed(vec![("libdd-x/src/hot.rs", vec![(15, 15)])]);
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.binaries.len(), 1);
        assert_eq!(selection.binaries[0].clusters.len(), 1);
        assert_eq!(selection.binaries[0].clusters[0].name, "a");
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::CoveredChange
        );
        assert_eq!(selection.binaries[0].bench_ids, ["a/x"]);
        assert_eq!(selection.binaries[0].filter, "^(a/x)$");
        assert_eq!(selection.summary.clusters_selected, 1);
    }

    #[test]
    fn skips_cluster_whose_coverage_does_not_overlap() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/hot.rs", vec![(10, 20)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        let ch = changed(vec![("libdd-x/src/hot.rs", vec![(50, 60)])]);
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        assert!(selection.binaries.is_empty());
        assert_eq!(selection.summary.clusters_selected, 0);
    }

    #[test]
    fn always_selects_failed_and_missing_coverage() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y", "c/z"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/hot.rs", vec![(1, 5)])]),
            CoverageEntry {
                crate_name: "libdd-x".into(),
                target: "main".into(),
                cluster: "b".into(),
                status: "failed".into(),
                files: BTreeMap::new(),
                deps: BTreeSet::new(),
            },
        ]);
        let ch = changed(vec![("libdd-x/src/unrelated.rs", vec![(1, 1)])]);
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        let selected: Vec<&str> = selection.binaries[0]
            .clusters
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(selected, ["b", "c"]);
        let reasons: Vec<Reason> = selection.binaries[0]
            .clusters
            .iter()
            .map(|c| c.reason)
            .collect();
        assert_eq!(reasons, [Reason::CoverageFailed, Reason::NoCoverage]);
    }

    #[test]
    fn bench_source_change_selects_whole_binary() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/hot.rs", vec![(1, 5)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        let ch = changed(vec![("libdd-x/benches/main.rs", vec![(1, 1)])]);
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.binaries.len(), 1);
        assert_eq!(selection.binaries[0].clusters.len(), 2);
        assert!(
            selection.binaries[0]
                .clusters
                .iter()
                .all(|c| c.reason == Reason::BenchSourceChanged)
        );
        assert_eq!(selection.binaries[0].filter, "^(a/x|b/y)$");
    }

    #[test]
    fn uncovered_inputs_select_every_cluster() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/lib.rs", vec![(1, 5)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        for path in [
            "Cargo.toml",
            "libdd-x/Cargo.toml",
            "rust-toolchain.toml",
            "ci-toolchain.toml",
            ".cargo/config.toml",
            "libdd-x/build.rs",
            "benchmark/run_benchmarks_ci.sh",
            ".gitlab/benchmarks.yml",
            ".gitlab/impacted-crates.yml",
            "libdd-x/src/hot.c",
            "libdd-x/include/hot.h",
            "libdd-x/src/hot.cpp",
            "libdd-x/src/hot.S",
            "libdd-x/schema.proto",
        ] {
            let selection = select(
                &inv,
                &cov,
                &changed(vec![(path, vec![(100, 100)])]),
                &LockfileChanges::default(),
                &ManifestChanges::default(),
                false,
            );
            assert_eq!(selection.summary.clusters_selected, 2, "{path}");
            assert!(
                selection.binaries[0]
                    .clusters
                    .iter()
                    .all(|c| c.reason == Reason::UncoveredInputChanged)
            );
        }
    }

    #[test]
    fn self_test_skips_tooling_paths_but_not_others() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/hot.rs", vec![(1, 5)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        // Tooling paths: ignored under self-test, so the unrelated .rs change
        // selects nothing.
        for path in [
            "benchmark/run_benchmarks_ci.sh",
            ".gitlab/benchmarks.yml",
            ".gitlab/impacted-crates.yml",
            ".github/actions/bench-selector/src/select.rs",
            // The CI helper workspace's manifests cannot affect the
            // benchmarked crates; ignored under self-test rather than
            // triggering the generic Cargo.toml/Cargo.lock filename rule.
            ".github/actions/Cargo.toml",
            ".github/actions/Cargo.lock",
        ] {
            let selection = select(
                &inv,
                &cov,
                &changed(vec![(path, vec![(100, 100)])]),
                &LockfileChanges::default(),
                &ManifestChanges::default(),
                true,
            );
            assert_eq!(selection.summary.clusters_selected, 0, "{path}");
        }
        // Non-tooling uncovered inputs still force a full run under self-test.
        for path in ["libdd-x/src/hot.c", "libdd-x/build.rs"] {
            let selection = select(
                &inv,
                &cov,
                &changed(vec![(path, vec![(100, 100)])]),
                &LockfileChanges::default(),
                &ManifestChanges::default(),
                true,
            );
            assert_eq!(selection.summary.clusters_selected, 2, "{path}");
        }
    }

    #[test]
    fn metadata_only_bench_changes_select_the_whole_binary() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/lib.rs", vec![(1, 5)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        let ch = crate::diff::parse_unified_diff("diff --git a/libdd-x/benches/old.rs b/libdd-x/benches/new.rs\nsimilarity index 100%\nrename from libdd-x/benches/old.rs\nrename to libdd-x/benches/new.rs\n").unwrap();
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 2);
        assert!(
            selection.binaries[0]
                .clusters
                .iter()
                .all(|c| c.reason == Reason::BenchSourceChanged)
        );
    }

    #[test]
    fn deleted_rust_source_and_empty_coverage_are_conservative() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x"])],
        };
        let cov = coverage(vec![entry("a", vec![("libdd-x/src/lib.rs", vec![(1, 5)])])]);
        let selection = select(
            &inv,
            &cov,
            &changed(vec![("libdd-x/src/deleted.rs", vec![])]),
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 1);
        let cov = coverage(vec![entry("a", vec![])]);
        let selection = select(
            &inv,
            &cov,
            &ChangedLines::new(),
            &LockfileChanges::default(),
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::CoverageFailed
        );
    }

    #[test]
    fn unrelated_documentation_does_not_force_a_full_run() {
        assert!(!requires_full_run("README.md", &[(1, 2)], false));
        assert!(!requires_full_run("docs/README.md", &[], false));
    }

    #[test]
    fn filter_is_exact_alternation() {
        assert_eq!(crate::cluster::exact_filter(&["a/x".into()]), "^(a/x)$");
        assert_eq!(
            crate::cluster::exact_filter(&["b".into(), "a/x".into()]),
            "^(a/x|b)$"
        );
    }

    #[test]
    fn lockfile_bump_selects_clusters_exercising_the_changed_dep() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        // Both clusters are healthy, neither overlaps the (empty) source diff;
        // cluster a executes the bumped dependency, cluster b does not.
        let cov = coverage(vec![
            entry_with_deps(
                "a",
                vec![("libdd-x/src/hot.rs", vec![(1, 5)])],
                vec!["zstd"],
            ),
            entry_with_deps(
                "b",
                vec![("libdd-x/src/other.rs", vec![(1, 5)])],
                vec!["serde"],
            ),
        ]);
        let lockfile = crate::lockfile::parse_lockfile_diff(
            "\
[[package]]
 name = \"zstd\"
-version = \"0.13\"
+version = \"0.14\"
",
            &std::collections::BTreeSet::new(),
        );
        let selection = select(
            &inv,
            &cov,
            &ChangedLines::new(),
            &lockfile,
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 1);
        let cluster = &selection.binaries[0].clusters[0];
        assert_eq!(cluster.name, "a");
        assert_eq!(cluster.reason, Reason::DependencyChanged);
    }

    #[test]
    fn lockfile_bump_of_unexercised_dep_selects_nothing() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry_with_deps(
                "a",
                vec![("libdd-x/src/hot.rs", vec![(1, 5)])],
                vec!["serde"],
            ),
            entry_with_deps(
                "b",
                vec![("libdd-x/src/other.rs", vec![(1, 5)])],
                vec!["libc"],
            ),
        ]);
        let lockfile = crate::lockfile::parse_lockfile_diff(
            "\
[[package]]
 name = \"zstd\"
-version = \"0.13\"
+version = \"0.14\"
",
            &std::collections::BTreeSet::new(),
        );
        let selection = select(
            &inv,
            &cov,
            &ChangedLines::new(),
            &lockfile,
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 0);
        assert!(selection.binaries.is_empty());
    }

    #[test]
    fn unattributable_lockfile_diff_selects_everything() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry_with_deps(
                "a",
                vec![("libdd-x/src/hot.rs", vec![(1, 5)])],
                vec!["serde"],
            ),
            entry_with_deps(
                "b",
                vec![("libdd-x/src/other.rs", vec![(1, 5)])],
                vec!["libc"],
            ),
        ]);
        let lockfile = crate::lockfile::LockfileChanges {
            packages: std::collections::BTreeSet::new(),
            unattributable: true,
        };
        let selection = select(
            &inv,
            &cov,
            &ChangedLines::new(),
            &lockfile,
            &ManifestChanges::default(),
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 2);
        assert!(
            selection.binaries[0]
                .clusters
                .iter()
                .all(|c| c.reason == Reason::DependencyChanged)
        );
    }

    #[test]
    fn root_lockfile_alone_no_longer_forces_full_run_but_member_manifests_do() {
        assert!(!requires_full_run("Cargo.lock", &[(1, 2)], false));
        assert!(requires_full_run("libdd-x/Cargo.toml", &[(1, 2)], false));
        assert!(requires_full_run("Cargo.toml", &[(1, 2)], false));
        // The CI helper workspace never affects the benchmarked crates, even
        // outside self-test; bench-selector's own sources still do.
        assert!(!requires_full_run(
            ".github/actions/Cargo.toml",
            &[(1, 2)],
            false
        ));
        assert!(!requires_full_run(
            ".github/actions/Cargo.lock",
            &[(1, 2)],
            false
        ));
        assert!(requires_full_run(
            ".github/actions/bench-selector/src/select.rs",
            &[(1, 2)],
            false
        ));
        assert!(!requires_full_run(
            ".github/actions/bench-selector/src/select.rs",
            &[(1, 2)],
            true
        ));
    }

    #[test]
    fn member_manifest_deps_only_scopes_like_a_lockfile_bump() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        // Cluster a executes the re-wired dependency; cluster b does not.
        let cov = coverage(vec![
            entry_with_deps(
                "a",
                vec![("libdd-x/src/hot.rs", vec![(1, 5)])],
                vec!["allocator-api2"],
            ),
            entry_with_deps(
                "b",
                vec![("libdd-x/src/other.rs", vec![(1, 5)])],
                vec!["serde"],
            ),
        ]);
        let ch = changed(vec![("libdd-x/Cargo.toml", vec![(30, 30)])]);
        let manifests = crate::manifest::ManifestChanges {
            deps_only: [("libdd-x/Cargo.toml".to_string(), true)]
                .into_iter()
                .collect(),
            packages: ["allocator-api2".to_string()].into_iter().collect(),
        };
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &manifests,
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 1);
        assert_eq!(selection.binaries[0].clusters[0].name, "a");
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::DependencyChanged
        );
        assert!(!selection.full_suite);
    }

    #[test]
    fn member_manifest_changing_a_member_dep_never_selects_nothing() {
        // A member manifest whose workspace-member path dependency got a new
        // feature: compare_manifests marks it not-deps-only with an EMPTY
        // package set (members are excluded from the dependency signal). An
        // empty selection here would be a SKIP in CI; selection must widen
        // instead — the compiled member code changed without any source
        // line moving.
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/hot.rs", vec![(1, 5)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        let ch = changed(vec![("libdd-sampling/Cargo.toml", vec![(30, 30)])]);
        let manifests = crate::manifest::ManifestChanges {
            deps_only: [("libdd-sampling/Cargo.toml".to_string(), false)]
                .into_iter()
                .collect(),
            packages: std::collections::BTreeSet::new(),
        };
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &manifests,
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 2);
        assert!(
            selection.binaries[0]
                .clusters
                .iter()
                .all(|c| c.reason == Reason::UncoveredInputChanged)
        );
        // A member manifest stays scoped to the inventory, not the suite.
        assert!(!selection.full_suite);
    }

    #[test]
    fn member_manifest_with_other_changes_keeps_the_full_run() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x"])],
        };
        let cov = coverage(vec![entry("a", vec![("libdd-x/src/hot.rs", vec![(1, 5)])])]);
        let ch = changed(vec![("libdd-x/Cargo.toml", vec![(40, 40)])]);
        let manifests = crate::manifest::ManifestChanges {
            deps_only: [("libdd-x/Cargo.toml".to_string(), false)]
                .into_iter()
                .collect(),
            packages: std::collections::BTreeSet::new(),
        };
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &manifests,
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 1);
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::UncoveredInputChanged
        );
        // A member manifest stays scoped to the affected crates, not the
        // whole suite.
        assert!(!selection.full_suite);
    }

    #[test]
    fn root_manifest_deps_only_does_not_force_anything() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry_with_deps(
                "a",
                vec![("libdd-x/src/hot.rs", vec![(1, 5)])],
                vec!["serde"],
            ),
            entry_with_deps(
                "b",
                vec![("libdd-x/src/other.rs", vec![(1, 5)])],
                vec!["libc"],
            ),
        ]);
        // Root [workspace.dependencies] zstd bump, dependencies-only. Nothing
        // here exercises zstd, so nothing runs.
        let ch = changed(vec![("Cargo.toml", vec![(60, 60)])]);
        let manifests = crate::manifest::ManifestChanges {
            deps_only: [("Cargo.toml".to_string(), true)].into_iter().collect(),
            packages: ["zstd".to_string()].into_iter().collect(),
        };
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &manifests,
            false,
        );
        assert_eq!(selection.summary.clusters_selected, 0);
        assert!(!selection.full_suite);
    }

    #[test]
    fn root_manifest_with_other_changes_requires_the_full_suite() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x"])],
        };
        let cov = coverage(vec![entry("a", vec![("libdd-x/src/hot.rs", vec![(1, 5)])])]);
        // A profile or membership change in the root manifest.
        let ch = changed(vec![("Cargo.toml", vec![(80, 80)])]);
        let manifests = crate::manifest::ManifestChanges {
            deps_only: [("Cargo.toml".to_string(), false)].into_iter().collect(),
            packages: std::collections::BTreeSet::new(),
        };
        let selection = select(
            &inv,
            &cov,
            &ch,
            &LockfileChanges::default(),
            &manifests,
            false,
        );
        assert!(selection.full_suite);
        assert_eq!(selection.summary.clusters_selected, 1);
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::UncoveredInputChanged
        );
    }

    #[test]
    fn manifest_and_lockfile_signals_merge() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry_with_deps(
                "a",
                vec![("libdd-x/src/hot.rs", vec![(1, 5)])],
                vec!["serde"],
            ),
            entry_with_deps(
                "b",
                vec![("libdd-x/src/other.rs", vec![(1, 5)])],
                vec!["bytes"],
            ),
        ]);
        // Lockfile bumped zstd (unexercised); a member manifest re-wired
        // bytes (exercised by cluster b).
        let lockfile = crate::lockfile::parse_lockfile_diff(
            "[[package]]\nname = \"zstd\"\n-version = \"0.13\"\n+version = \"0.14\"\n",
            &std::collections::BTreeSet::new(),
        );
        let ch = changed(vec![("libdd-x/Cargo.toml", vec![(30, 30)])]);
        let manifests = crate::manifest::ManifestChanges {
            deps_only: [("libdd-x/Cargo.toml".to_string(), true)]
                .into_iter()
                .collect(),
            packages: ["bytes".to_string()].into_iter().collect(),
        };
        let selection = select(&inv, &cov, &ch, &lockfile, &manifests, false);
        assert_eq!(selection.summary.clusters_selected, 1);
        assert_eq!(selection.binaries[0].clusters[0].name, "b");
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::DependencyChanged
        );
    }

    #[test]
    fn clusters_helper_groups_ids() {
        let clusters = clusters_of(&["a/x".to_string(), "a/y".to_string(), "b".to_string()]);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].name, "a");
        assert_eq!(clusters[1].bench_ids, ["b"]);
    }
}
