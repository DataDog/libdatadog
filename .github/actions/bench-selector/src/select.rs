// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Intersect executed-function coverage with changed lines and dependencies.
//! Missing evidence, changed bench methodology, and inputs outside runtime
//! coverage widen the selection. Measuring filters match the selected IDs exactly.

use crate::coverage::{Coverage, CoverageEntry};
use crate::diff::ChangedLines;
use crate::inventory::Inventory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct Selection {
    pub binaries: Vec<SelectedBinary>,
    pub summary: Summary,
    /// Root manifest changes outside dependencies affect even crates absent
    /// from the inventory. CI must run the entire suite, not just these binaries.
    #[serde(default)]
    pub full_suite: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectedBinary {
    pub crate_name: String,
    pub target: String,
    pub features: Vec<String>,
    pub bench_ids: Vec<String>,
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
    CoveredChange,
    BenchSourceChanged,
    NoCoverage,
    CoverageFailed,
    UncoveredInputChanged,
    DependencyChanged,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Summary {
    pub binaries_total: usize,
    pub binaries_selected: usize,
    pub clusters_total: usize,
    pub clusters_selected: usize,
}

fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

pub fn select(
    inventory: &Inventory,
    coverage: &Coverage,
    changed: &ChangedLines,
    lockfile: &crate::lockfile::LockfileChanges,
    manifests: &crate::manifest::ManifestChanges,
    self_test: bool,
) -> Selection {
    let coverage_by_key: BTreeMap<(&str, &str, &str), &CoverageEntry> = coverage
        .entries
        .iter()
        .map(|e| {
            (
                (e.crate_name.as_str(), e.target.as_str(), e.cluster.as_str()),
                e,
            )
        })
        .collect();
    let mut changed_packages = lockfile.packages.clone();
    changed_packages.extend(manifests.packages.iter().cloned());

    // Dependency-only manifests use package coverage; unanalyzed manifests
    // retain requires_full_run's conservative default.
    let full_run = changed
        .iter()
        .any(|(path, ranges)| match manifests.deps_only.get(path) {
            Some(deps_only) => !deps_only,
            None => requires_full_run(path, ranges, self_test),
        });
    let full_suite = manifests.deps_only.get("Cargo.toml") == Some(&false);
    let mut binaries = Vec::new();
    let mut clusters_total = 0;
    let mut clusters_selected = 0;

    for bin in &inventory.binaries {
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
                    Some(entry) if cluster_touches_changes(&entry.files, changed) => {
                        Some(Reason::CoveredChange)
                    }
                    Some(entry)
                        if lockfile.unattributable
                            || !entry.deps.is_disjoint(&changed_packages) =>
                    {
                        Some(Reason::DependencyChanged)
                    }
                    Some(_) => None,
                }
            };
            if let Some(reason) = reason {
                clusters_selected += 1;
                selected_clusters.push(SelectedCluster {
                    name: cluster.name,
                    bench_ids: cluster.bench_ids,
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

/// Self-test ignores selection tooling so validation PRs can exercise narrowing.
/// It never exempts native sources or toolchain/build inputs.
fn requires_full_run(path: &str, ranges: &[(u32, u32)], self_test: bool) -> bool {
    // This separate workspace cannot affect benchmarked crates, except when
    // changing the selector itself (which must validate against the full suite).
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
        || (!self_test
            && (path.starts_with("benchmark/")
                || tooling
                || matches!(
                    path,
                    ".gitlab/benchmarks.yml" | ".gitlab/impacted-crates.yml"
                )))
    {
        return true;
    }
    // Cargo.lock is handled by the package intersection, not the full-run guard.
    match path_obj.extension().and_then(|ext| ext.to_str()) {
        Some(
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "s" | "S" | "asm" | "m"
            | "mm" | "inc" | "proto",
        ) => true,
        // Deleted/renamed/mode-only paths may be absent from candidate coverage.
        Some("rs") => ranges.is_empty(),
        _ => false,
    }
}

fn cluster_touches_changes(
    covered: &BTreeMap<String, Vec<(u32, u32)>>,
    changed: &ChangedLines,
) -> bool {
    covered.iter().any(|(file, ranges)| {
        changed.get(file).is_some_and(|changed_ranges| {
            ranges
                .iter()
                .any(|covered| changed_ranges.iter().any(|c| overlaps(*covered, *c)))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::LineRange;
    use crate::inventory::BenchBinary;
    use crate::lockfile::LockfileChanges;
    use crate::manifest::ManifestChanges;

    struct Fixture {
        inventory: Inventory,
        coverage: Coverage,
        lockfile: LockfileChanges,
        manifests: ManifestChanges,
        self_test: bool,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                inventory: Inventory {
                    binaries: vec![BenchBinary {
                        crate_name: "libdd-x".into(),
                        target: "main".into(),
                        crate_dir: "libdd-x".into(),
                        features: vec![],
                        bench_ids: vec!["a/x".into(), "b/y".into()],
                    }],
                },
                coverage: Coverage {
                    entries: [
                        ("a", "hot", (10, 20), "zstd"),
                        ("b", "cold", (1, 5), "serde"),
                    ]
                    .into_iter()
                    .map(|(cluster, file, range, dep)| CoverageEntry {
                        crate_name: "libdd-x".into(),
                        target: "main".into(),
                        cluster: cluster.into(),
                        status: "ok".into(),
                        files: [(format!("libdd-x/src/{file}.rs"), vec![range])].into(),
                        deps: [dep.into()].into(),
                    })
                    .collect(),
                },
                lockfile: LockfileChanges::default(),
                manifests: ManifestChanges::default(),
                self_test: false,
            }
        }
    }

    impl Fixture {
        fn run(&self, files: &[(&str, &[LineRange])]) -> Selection {
            let changed = files
                .iter()
                .map(|(path, ranges)| (path.to_string(), ranges.to_vec()))
                .collect();
            select(
                &self.inventory,
                &self.coverage,
                &changed,
                &self.lockfile,
                &self.manifests,
                self.self_test,
            )
        }
    }

    fn selected(selection: &Selection) -> Vec<(&str, Reason)> {
        selection
            .binaries
            .iter()
            .flat_map(|b| &b.clusters)
            .map(|c| (c.name.as_str(), c.reason))
            .collect()
    }

    #[test]
    fn selects_only_overlapping_clusters_with_exact_filters() {
        let fixture = Fixture::default();
        let selection = fixture.run(&[("libdd-x/src/hot.rs", &[(15, 15)])]);
        assert_eq!(selected(&selection), [("a", Reason::CoveredChange)]);
        assert_eq!(selection.binaries[0].bench_ids, ["a/x"]);
        assert_eq!(selection.binaries[0].filter, "^(a/x)$");
        assert_eq!(selection.summary.binaries_total, 1);
        assert_eq!(selection.summary.binaries_selected, 1);
        assert_eq!(selection.summary.clusters_total, 2);
        assert_eq!(selection.summary.clusters_selected, 1);
        let skipped = fixture.run(&[("libdd-x/src/hot.rs", &[(50, 60)])]);
        assert!(skipped.binaries.is_empty());
        assert_eq!(skipped.summary.clusters_selected, 0);
    }

    #[test]
    fn missing_failed_and_empty_coverage_always_run() {
        let mut fixture = Fixture::default();
        fixture.coverage.entries.pop();
        for empty in [false, true] {
            fixture.coverage.entries[0].status = if empty { "ok" } else { "failed" }.into();
            if empty {
                fixture.coverage.entries[0].files.clear();
            }
            assert_eq!(
                selected(&fixture.run(&[])),
                [("a", Reason::CoverageFailed), ("b", Reason::NoCoverage),]
            );
        }
    }

    #[test]
    fn bench_changes_select_whole_binary_even_without_ranges() {
        let fixture = Fixture::default();
        for ranges in [&[(1, 1)][..], &[]] {
            let selection = fixture.run(&[("libdd-x/benches/main.rs", ranges)]);
            assert_eq!(
                selected(&selection),
                [
                    ("a", Reason::BenchSourceChanged),
                    ("b", Reason::BenchSourceChanged),
                ]
            );
            assert_eq!(selection.binaries[0].filter, "^(a/x|b/y)$");
        }
    }

    #[test]
    fn uncovered_inputs_and_self_test_exemptions() {
        for self_test in [false, true] {
            let fixture = Fixture {
                self_test,
                ..Fixture::default()
            };
            for path in [
                "Cargo.toml",
                "libdd-x/Cargo.toml",
                "rust-toolchain.toml",
                "ci-toolchain.toml",
                ".cargo/config.toml",
                "libdd-x/build.rs",
                "libdd-x/src/hot.c",
                "libdd-x/include/hot.h",
                "libdd-x/src/hot.cpp",
                "libdd-x/src/hot.S",
                "libdd-x/schema.proto",
            ] {
                assert_eq!(
                    selected(&fixture.run(&[(path, &[(100, 100)])])),
                    [
                        ("a", Reason::UncoveredInputChanged),
                        ("b", Reason::UncoveredInputChanged),
                    ],
                    "{path}, self_test={self_test}"
                );
            }
            for path in [
                "benchmark/run_benchmarks_ci.sh",
                ".gitlab/benchmarks.yml",
                ".gitlab/impacted-crates.yml",
                ".github/actions/bench-selector/src/select.rs",
            ] {
                assert_eq!(
                    fixture
                        .run(&[(path, &[(100, 100)])])
                        .summary
                        .clusters_selected,
                    if self_test { 0 } else { 2 },
                    "{path}"
                );
            }
            for path in [
                "README.md",
                "docs/README.md",
                "Cargo.lock",
                ".github/actions/Cargo.toml",
                ".github/actions/Cargo.lock",
            ] {
                assert!(
                    fixture.run(&[(path, &[(1, 2)])]).binaries.is_empty(),
                    "{path}"
                );
            }
            assert_eq!(
                fixture
                    .run(&[("libdd-x/src/deleted.rs", &[])])
                    .summary
                    .clusters_selected,
                2
            );
        }
    }

    #[test]
    fn lockfile_changes_are_scoped_to_executed_dependencies() {
        for (packages, unattributable, expected) in [
            (vec!["zstd"], false, vec![("a", Reason::DependencyChanged)]),
            (vec!["unused"], false, vec![]),
            (
                vec![],
                true,
                vec![
                    ("a", Reason::DependencyChanged),
                    ("b", Reason::DependencyChanged),
                ],
            ),
        ] {
            let fixture = Fixture {
                lockfile: LockfileChanges {
                    packages: packages.into_iter().map(str::to_owned).collect(),
                    unattributable,
                },
                ..Fixture::default()
            };
            assert_eq!(selected(&fixture.run(&[])), expected);
        }
    }

    #[test]
    fn manifests_scope_dependencies_or_widen_the_run() {
        for path in ["Cargo.toml", "libdd-x/Cargo.toml"] {
            for deps_only in [false, true] {
                for packages in [vec![], vec!["zstd"], vec!["unused"]] {
                    let fixture = Fixture {
                        manifests: ManifestChanges {
                            deps_only: [(path.into(), deps_only)].into(),
                            packages: packages.iter().map(|p| p.to_string()).collect(),
                        },
                        ..Fixture::default()
                    };
                    let selection = fixture.run(&[(path, &[(30, 30)])]);
                    let expected = if !deps_only {
                        vec![
                            ("a", Reason::UncoveredInputChanged),
                            ("b", Reason::UncoveredInputChanged),
                        ]
                    } else if packages.contains(&"zstd") {
                        vec![("a", Reason::DependencyChanged)]
                    } else {
                        vec![]
                    };
                    assert_eq!(
                        selected(&selection),
                        expected,
                        "{path}, deps_only={deps_only}, {packages:?}"
                    );
                    assert_eq!(selection.full_suite, path == "Cargo.toml" && !deps_only);
                }
            }
        }
    }

    #[test]
    fn manifest_and_lockfile_signals_merge() {
        let fixture = Fixture {
            lockfile: LockfileChanges {
                packages: ["zstd".into()].into(),
                unattributable: false,
            },
            manifests: ManifestChanges {
                deps_only: [("libdd-x/Cargo.toml".into(), true)].into(),
                packages: ["serde".into()].into(),
            },
            ..Fixture::default()
        };
        assert_eq!(
            selected(&fixture.run(&[("libdd-x/Cargo.toml", &[(30, 30)])])),
            [
                ("a", Reason::DependencyChanged),
                ("b", Reason::DependencyChanged),
            ]
        );
    }
}
