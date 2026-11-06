// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Level-1 selection unit: bench binaries.
//!
//! One `BenchBinary` is one `[[bench]]` target of one workspace crate. Every
//! bench target is built and executed as its own process, so it is the
//! outermost selection granularity: a binary with no selected cluster does not
//! need to run at all.
//!
//! The bench IDs within a binary are discovered by running criterion's
//! `--list` mode against the built binary; the ID strings are then clustered
//! (level 2) by [`crate::cluster`].

use crate::cluster::Cluster;
use crate::cluster::clusters_of;
use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Serialized inventory of every bench binary and its name clusters.
#[derive(Debug, Serialize, Deserialize)]
pub struct Inventory {
    pub binaries: Vec<BenchBinary>,
}

/// One bench binary: a crate's `[[bench]]` target.
#[derive(Debug, Serialize, Deserialize)]
pub struct BenchBinary {
    pub crate_name: String,
    /// The `[[bench]]` target name (`cargo bench --bench <target>`).
    pub target: String,
    /// Crate directory, relative to the workspace root.
    pub crate_dir: String,
    /// Features required to build and run this bench target.
    pub features: Vec<String>,
    /// All benchmark IDs reported by criterion `--list` for this binary.
    pub bench_ids: Vec<String>,
}

impl BenchBinary {
    /// Level-2 clusters of this binary, sorted by cluster name.
    pub fn clusters(&self) -> Vec<Cluster> {
        clusters_of(&self.bench_ids)
    }

    /// Directory of this binary's bench sources, relative to the workspace
    /// root (`<crate_dir>/benches`). Changes under it always select the whole
    /// binary: bench code itself changed, so its measurement methodology did.
    pub fn bench_source_dir(&self) -> String {
        format!("{}/benches", self.crate_dir)
    }
}

/// Features that some crates need beyond their bench targets' declared
/// `required-features` for the benches to compile.
///
/// Keep in sync with `bench_features_for_crate` in
/// `benchmark/run_benchmarks_ci.sh` until the manifests declare them as
/// `required-features` on the bench targets themselves (preferred fix).
fn supplemental_features(crate_name: &str) -> &'static [&'static str] {
    match crate_name {
        "libdd-crashtracker" => &["benchmarking"],
        _ => &[],
    }
}

/// Discover every bench binary of the workspace rooted at `workspace_root`
/// (optionally restricted to the given crate names) by running
/// `cargo bench ... -- --list` per bench target.
pub fn discover(
    workspace_root: &Path,
    crates_filter: Option<&[String]>,
    instrumented_target_dir: Option<&Path>,
) -> Result<Inventory> {
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("failed to run cargo metadata against the target workspace")?;
    let root = &metadata.workspace_root;
    let members: Vec<&cargo_metadata::Package> = metadata.workspace_packages();
    validate_crates_filter(crates_filter, members.iter().map(|pkg| pkg.name.as_ref()))?;
    let wanted: Option<std::collections::BTreeSet<&str>> =
        crates_filter.map(|c| c.iter().map(|s| s.as_str()).collect());

    let mut binaries = Vec::new();
    for pkg in members {
        // Not a let-chain: the tool builds on checkouts older than Rust 1.88,
        // where let-chains are not stable.
        if matches!(&wanted, Some(wanted) if !wanted.contains(pkg.name.as_ref())) {
            continue;
        }
        let Some(crate_dir) = pkg
            .manifest_path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.as_str().to_string())
        else {
            continue;
        };
        for target in &pkg.targets {
            if !target.kind.iter().any(|k| matches!(k, TargetKind::Bench)) {
                continue;
            }
            let mut features: Vec<String> = target.required_features.clone();
            for f in supplemental_features(pkg.name.as_ref()) {
                let f = f.to_string();
                if !features.contains(&f) {
                    features.push(f);
                }
            }
            features.sort();
            let bench_ids = list_bench_ids(
                workspace_root,
                pkg.name.as_ref(),
                &target.name,
                &features,
                instrumented_target_dir,
            )?;
            if bench_ids.is_empty() {
                log::warn!(
                    "no benchmarks listed for {} --bench {}; skipping",
                    pkg.name,
                    target.name
                );
                continue;
            }
            binaries.push(BenchBinary {
                crate_name: pkg.name.to_string(),
                target: target.name.clone(),
                crate_dir: crate_dir.clone(),
                features,
                bench_ids,
            });
        }
    }
    binaries.sort_by(|a, b| (&a.crate_name, &a.target).cmp(&(&b.crate_name, &b.target)));
    Ok(Inventory { binaries })
}

/// Reject unmatched restrictions instead of silently producing empty evidence.
pub fn validate_crates_filter<'a>(
    filter: Option<&[String]>,
    available: impl Iterator<Item = &'a str>,
) -> Result<()> {
    if let Some(filter) = filter {
        anyhow::ensure!(
            !filter.is_empty(),
            "--crates must contain at least one crate name"
        );
        let available: std::collections::BTreeSet<&str> = available.collect();
        let unknown: Vec<&str> = filter
            .iter()
            .map(String::as_str)
            .filter(|name| !available.contains(name))
            .collect();
        anyhow::ensure!(
            unknown.is_empty(),
            "unknown crate(s) in --crates: {}; check the workspace/inventory crate names",
            unknown.join(", ")
        );
    }
    Ok(())
}

/// Run `cargo bench -p <crate> --bench <target> -- --list` and parse the
/// benchmark IDs from criterion's `<id>: benchmark` output lines.
///
/// The build is overridden to a debug-like profile (opt-level 0, no LTO, 16
/// codegen units): `--list` only reads the statically registered benchmark
/// IDs, which do not depend on optimization, and the fast build keeps the
/// selection job cheap. The measuring run is unaffected — it never goes
/// through this path.
fn list_bench_ids(
    workspace_root: &Path,
    crate_name: &str,
    target: &str,
    features: &[String],
    instrumented_target_dir: Option<&Path>,
) -> Result<Vec<String>> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root);
    match instrumented_target_dir {
        // Instrumented build in the coverage pass's target dir, so the two
        // passes share ONE build of the dependency graph: same RUSTFLAGS and
        // CARGO_TARGET_DIR make every coverage `--no-run` a cache hit. Bench
        // IDs are statically registered, so an instrumented `--list` reports
        // the same IDs as a clean one. The `--list` process itself writes a
        // junk profile (see coverage::BUILD_JUNK_PROFILE_PATTERN): its
        // coverage data is garbage by definition and is never read.
        Some(dir) => {
            for (key, value) in crate::coverage::cargo_env(dir) {
                cmd.env(key, value);
            }
            cmd.env(
                "LLVM_PROFILE_FILE",
                dir.join(crate::coverage::BUILD_JUNK_PROFILE_PATTERN),
            );
        }
        // Standalone use (local runs, tests): fast debug-like profile, no
        // instrumentation, default target dir.
        None => {
            cmd.env("CARGO_PROFILE_BENCH_OPT_LEVEL", "0")
                .env("CARGO_PROFILE_BENCH_CODEGEN_UNITS", "16")
                .env("CARGO_PROFILE_BENCH_LTO", "false");
        }
    }
    cmd.args(["bench", "-p", crate_name, "--bench", target]);
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    cmd.args(["--", "--list", "--format", "terse"]);
    let output = cmd
        .output()
        .with_context(|| format!("failed to run cargo bench --list for {crate_name}/{target}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "cargo bench -p {crate_name} --bench {target} -- --list failed (status {:?}): {stderr}",
            output.status.code()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_list_output(&stdout))
}

/// Parse criterion `--list` output lines into benchmark IDs.
fn parse_list_output(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.strip_suffix(": benchmark"))
        .map(|id| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("repo root")
    }

    #[test]
    fn parses_criterion_list_output() {
        let stdout = "\
ddsketch_add/add/uniform: benchmark
ddsketch_encode/into_pb/uniform: benchmark
msgpack_encoder::v04::to_vec: benchmark
Compiling foo
warning: something
";
        let ids = parse_list_output(stdout);
        assert_eq!(
            ids,
            [
                "ddsketch_add/add/uniform".to_string(),
                "ddsketch_encode/into_pb/uniform".to_string(),
                "msgpack_encoder::v04::to_vec".to_string(),
            ]
        );
    }

    #[test]
    fn unmatched_or_empty_crate_restrictions_are_errors() {
        let known = ["libdd-x", "libdd-y"];
        assert!(validate_crates_filter(Some(&["libdd-x".into()]), known.into_iter()).is_ok());
        assert!(
            validate_crates_filter(Some(&["libdd-x libdd-y".into()]), known.into_iter()).is_err()
        );
        assert!(validate_crates_filter(Some(&["typo".into()]), known.into_iter()).is_err());
        assert!(validate_crates_filter(Some(&[]), known.into_iter()).is_err());
        assert!(validate_crates_filter(None, known.into_iter()).is_ok());
    }

    #[test]
    fn supplemental_features_known_crates() {
        assert_eq!(
            supplemental_features("libdd-crashtracker"),
            ["benchmarking"]
        );
        assert!(supplemental_features("libdd-ddsketch").is_empty());
    }

    #[test]
    fn bench_source_dir_is_under_crate() {
        let bin = BenchBinary {
            crate_name: "libdd-trace-utils".into(),
            target: "main".into(),
            crate_dir: "libdd-trace-utils".into(),
            features: vec![],
            bench_ids: vec![],
        };
        assert_eq!(bin.bench_source_dir(), "libdd-trace-utils/benches");
    }

    #[test]
    fn bench_targets_expose_required_features() {
        let metadata = MetadataCommand::new()
            .manifest_path(repo_root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .expect("cargo metadata");
        let sampling = metadata
            .packages
            .iter()
            .find(|p| p.name.as_ref() == "libdd-sampling")
            .expect("libdd-sampling in workspace");
        let mut bench_targets: BTreeMap<&str, &Vec<String>> = sampling
            .targets
            .iter()
            .filter(|t| t.kind.iter().any(|k| matches!(k, TargetKind::Bench)))
            .map(|t| (t.name.as_str(), &t.required_features))
            .collect();
        assert_eq!(bench_targets.len(), 2, "sampling has two bench targets");
        assert_eq!(
            bench_targets.remove("sampling_bench").unwrap(),
            &["v04_span".to_string()]
        );
        assert_eq!(
            bench_targets.remove("glob_matcher_bench").unwrap(),
            &["bench-internals".to_string()]
        );
    }

    /// End-to-end discovery against the real workspace: one small bench crate.
    /// Requires compiling `libdd-ddsketch`'s bench target (a few seconds).
    #[test]
    fn discover_finds_ddsketch_binary() {
        let inventory =
            discover(&repo_root(), Some(&["libdd-ddsketch".to_string()]), None).expect("discovery");
        assert_eq!(inventory.binaries.len(), 1);
        let bin = &inventory.binaries[0];
        assert_eq!(bin.crate_name, "libdd-ddsketch");
        assert_eq!(bin.target, "ddsketch_bench");
        assert!(!bin.bench_ids.is_empty(), "expected ddsketch bench IDs");
        for id in &bin.bench_ids {
            assert!(id.starts_with("ddsketch_"), "unexpected bench id: {id}");
        }
        let clusters = bin.clusters();
        assert!(
            clusters.iter().any(|c| c.name == "ddsketch_add"),
            "expected a ddsketch_add cluster, got {:?}",
            clusters.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        // Coverage filters must select the cluster without swallowing siblings.
        let add = clusters.iter().find(|c| c.name == "ddsketch_add").unwrap();
        assert_eq!(add.coverage_filter(), "^ddsketch_add(/|$)");
    }
}
