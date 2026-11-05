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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Summary {
    pub binaries_total: usize,
    pub binaries_selected: usize,
    pub clusters_total: usize,
    pub clusters_selected: usize,
}

/// Do two inclusive ranges overlap?
fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// Compute the selection for every binary of `inventory`.
pub fn select(inventory: &Inventory, coverage: &Coverage, changed: &ChangedLines) -> Selection {
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

    let full_run = changed
        .iter()
        .any(|(path, ranges)| requires_full_run(path, ranges));
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
        binaries,
    }
}

/// Inputs outside Rust runtime coverage need a conservative full run. This
/// includes the existing benchmark script's infrastructure/toolchain fallback.
fn requires_full_run(path: &str, ranges: &[(u32, u32)]) -> bool {
    let path_obj = std::path::Path::new(path);
    let name = path_obj.file_name().and_then(|name| name.to_str());
    if matches!(
        name,
        Some(
            "Cargo.toml"
                | "Cargo.lock"
                | "build.rs"
                | "rust-toolchain"
                | "rust-toolchain.toml"
                | "ci-toolchain.toml"
        )
    ) || path.starts_with(".cargo/")
        || path.contains("/.cargo/")
        || path.starts_with("benchmark/")
        || path.starts_with(".github/actions/bench-selector/")
        || matches!(
            path,
            ".gitlab/benchmarks.yml" | ".gitlab/impacted-crates.yml"
        )
    {
        return true;
    }
    match path_obj.extension().and_then(|ext| ext.to_str()) {
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
        CoverageEntry {
            crate_name: "libdd-x".into(),
            target: "main".into(),
            cluster: cluster.into(),
            status: "ok".into(),
            files: files.into_iter().map(|(f, r)| (f.to_string(), r)).collect(),
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
        let selection = select(&inv, &cov, &ch);
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
        let selection = select(&inv, &cov, &ch);
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
            },
        ]);
        let ch = changed(vec![("libdd-x/src/unrelated.rs", vec![(1, 1)])]);
        let selection = select(&inv, &cov, &ch);
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
        let selection = select(&inv, &cov, &ch);
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
            "Cargo.lock",
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
            let selection = select(&inv, &cov, &changed(vec![(path, vec![(100, 100)])]));
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
    fn metadata_only_bench_changes_select_the_whole_binary() {
        let inv = Inventory {
            binaries: vec![binary(&["a/x", "b/y"])],
        };
        let cov = coverage(vec![
            entry("a", vec![("libdd-x/src/lib.rs", vec![(1, 5)])]),
            entry("b", vec![("libdd-x/src/other.rs", vec![(1, 5)])]),
        ]);
        let ch = crate::diff::parse_unified_diff("diff --git a/libdd-x/benches/old.rs b/libdd-x/benches/new.rs\nsimilarity index 100%\nrename from libdd-x/benches/old.rs\nrename to libdd-x/benches/new.rs\n").unwrap();
        let selection = select(&inv, &cov, &ch);
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
        );
        assert_eq!(selection.summary.clusters_selected, 1);
        let cov = coverage(vec![entry("a", vec![])]);
        let selection = select(&inv, &cov, &ChangedLines::new());
        assert_eq!(
            selection.binaries[0].clusters[0].reason,
            Reason::CoverageFailed
        );
    }

    #[test]
    fn unrelated_documentation_does_not_force_a_full_run() {
        assert!(!requires_full_run("README.md", &[(1, 2)]));
        assert!(!requires_full_run("docs/README.md", &[]));
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
    fn clusters_helper_groups_ids() {
        let clusters = clusters_of(&["a/x".to_string(), "a/y".to_string(), "b".to_string()]);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].name, "a");
        assert_eq!(clusters[1].bench_ids, ["b"]);
    }
}
