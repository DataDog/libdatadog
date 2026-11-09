// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Discover workspace bench targets and their criterion `--list` IDs.

use crate::cluster::{Cluster, clusters_of};
use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub binaries: Vec<BenchBinary>,
}

/// One bench binary: a crate's `[[bench]]` target.
#[derive(Debug, Clone, Serialize)]
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
    pub fn clusters(&self) -> Vec<Cluster> {
        clusters_of(&self.bench_ids)
    }

    /// Changes here select the whole binary: measurement methodology changed.
    pub fn bench_source_dir(&self) -> String {
        format!("{}/benches", self.crate_dir)
    }
}

/// Features missing from targets' `required-features`. Keep in sync with
/// `bench_features_for_crate` in `benchmark/run_benchmarks_ci.sh`.
fn supplemental_features(crate_name: &str) -> &'static [&'static str] {
    match crate_name {
        "libdd-crashtracker" => &["benchmarking"],
        _ => &[],
    }
}

pub fn discover(
    workspace_root: &Path,
    crates_filter: Option<&[String]>,
    target_dir: &Path,
) -> Result<(Inventory, crate::coverage::Executables)> {
    let started = std::time::Instant::now();
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
    match crates_filter {
        Some(crates) => log::info!(
            "inventory: {} crate(s) in scope, discovering their bench targets",
            crates.len()
        ),
        None => log::info!(
            "inventory: discovering bench targets of all {} workspace crates",
            members.len()
        ),
    }

    let mut candidates = Vec::new();
    for pkg in members {
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
            let mut features = target.required_features.clone();
            for feature in supplemental_features(pkg.name.as_ref()) {
                if !features.iter().any(|f| f == feature) {
                    features.push(feature.to_string());
                }
            }
            features.sort();
            candidates.push(BenchBinary {
                crate_name: pkg.name.to_string(),
                target: target.name.clone(),
                crate_dir: crate_dir.clone(),
                features,
                bench_ids: Vec::new(),
            });
        }
    }
    candidates.sort_by(|a, b| (&a.crate_name, &a.target).cmp(&(&b.crate_name, &b.target)));

    let target_dir = crate::coverage::prepare_directory(&workspace_root.join(target_dir))?;
    let executables = crate::coverage::build_all_benches(
        workspace_root,
        &candidates.iter().collect::<Vec<_>>(),
        &target_dir,
    )?;

    let mut binaries = Vec::new();
    for (index, bin) in candidates.iter().enumerate() {
        let step = index + 1;
        let listing = std::time::Instant::now();
        log::info!(
            "inventory: [{step}/{}] listing {} --bench {}",
            candidates.len(),
            bin.crate_name,
            bin.target
        );
        let bench_ids = list_bench_ids(
            workspace_root,
            bin,
            executables
                .get(&(bin.crate_name.clone(), bin.target.clone()))
                .with_context(|| {
                    format!(
                        "no executable for {} --bench {}",
                        bin.crate_name, bin.target
                    )
                })?,
            &target_dir,
        )?;
        log::info!(
            "inventory: [{step}/{}] {} --bench {}: {} benchmark ID(s) in {:.1?}",
            candidates.len(),
            bin.crate_name,
            bin.target,
            bench_ids.len(),
            listing.elapsed()
        );
        if bench_ids.is_empty() {
            log::warn!(
                "no benchmarks listed for {} --bench {}; skipping",
                bin.crate_name,
                bin.target
            );
            continue;
        }
        binaries.push(BenchBinary {
            bench_ids,
            ..(*bin).clone()
        });
    }
    log::info!(
        "inventory: {} bench binary(ies), {} benchmark ID(s) in {:.1?}",
        binaries.len(),
        binaries.iter().map(|b| b.bench_ids.len()).sum::<usize>(),
        started.elapsed()
    );
    Ok((Inventory { binaries }, executables))
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

fn list_bench_ids(
    workspace_root: &Path,
    bin: &BenchBinary,
    executable: &Path,
    target_dir: &Path,
) -> Result<Vec<String>> {
    let mut cmd =
        crate::coverage::direct_bench_command(workspace_root, bin, executable, target_dir);
    cmd.env(
        "LLVM_PROFILE_FILE",
        target_dir.join(crate::coverage::BUILD_JUNK_PROFILE_PATTERN),
    )
    .args(["--list", "--format", "terse"]);
    let output = cmd
        .output()
        .with_context(|| format!("listing {} --bench {}", bin.crate_name, bin.target))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "listing {} --bench {} failed (status {:?}): {stderr}",
            bin.crate_name,
            bin.target,
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
    fn discover_finds_ddsketch_binary() {
        let (inventory, _) = discover(
            &repo_root(),
            Some(&["libdd-ddsketch".to_string()]),
            Path::new("target/bench-selector-coverage"),
        )
        .expect("discovery");
        assert_eq!(inventory.binaries.len(), 1);
        let bin = &inventory.binaries[0];
        assert_eq!(bin.crate_name, "libdd-ddsketch");
        assert_eq!(bin.target, "ddsketch_bench");
        assert!(!bin.bench_ids.is_empty(), "expected ddsketch bench IDs");
        for id in &bin.bench_ids {
            assert!(id.starts_with("ddsketch_"), "unexpected bench id: {id}");
        }
        assert!(bin.clusters().iter().any(|c| c.name == "ddsketch_add"));
    }
}
