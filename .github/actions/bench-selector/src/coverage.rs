// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Build all benches and dependencies with coverage in a separate, debug-like
//! target directory, then run each name cluster with criterion `--test`.
//! Reduce LLVM exports to all regions of entered functions (including untaken
//! branches). Failed runs or exports always select the affected cluster.

use crate::cluster::Cluster;
use crate::inventory::{BenchBinary, Inventory};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) type Executables = BTreeMap<(String, String), PathBuf>;

/// Per-cluster coverage data, reduced to executed-function line ranges keyed
/// by workspace-relative file path.
#[derive(Debug, Serialize)]
pub struct Coverage {
    pub entries: Vec<CoverageEntry>,
}

#[derive(Debug, Serialize)]
pub struct CoverageEntry {
    pub crate_name: String,
    pub target: String,
    pub cluster: String,
    /// `ok` or `failed`; a `failed` entry carries no line data and always
    /// selects its cluster downstream.
    pub status: String,
    /// Merged, sorted, non-overlapping inclusive `[start, end]` line ranges.
    pub files: BTreeMap<String, Vec<(u32, u32)>>,
    /// Executed external packages, used to scope dependency changes.
    pub deps: BTreeSet<String>,
}

impl CoverageEntry {
    fn failed(bin: &BenchBinary, cluster: String) -> Self {
        Self {
            crate_name: bin.crate_name.clone(),
            target: bin.target.clone(),
            cluster,
            status: "failed".into(),
            files: BTreeMap::new(),
            deps: BTreeSet::new(),
        }
    }
}

struct Tools {
    profdata: PathBuf,
    cov: PathBuf,
}

/// Collect per-cluster coverage, using `work` for raw and merged profiles.
pub fn run(
    workspace_root: &Path,
    inventory: &Inventory,
    executables: &Executables,
    target_dir: &Path,
    work: &Path,
) -> Result<Coverage> {
    let in_scope = &inventory.binaries;
    if has_path_remapping(&inherited_rustflags()) {
        // Remapped libraries can disappear even when benchmark-file ranges survive.
        log::warn!("path-remapping rustflags prevent trustworthy coverage; all clusters will run");
        let entries = in_scope
            .iter()
            .flat_map(|bin| {
                bin.clusters()
                    .into_iter()
                    .map(|cluster| CoverageEntry::failed(bin, cluster.name))
            })
            .collect();
        return Ok(Coverage { entries });
    }
    let tools = Tools {
        profdata: ensure_tool("llvm-profdata")?,
        cov: ensure_tool("llvm-cov")?,
    };
    let workspace_root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    // Scratch paths are caller-relative, but Cargo/benchmarks change cwd.
    let work = prepare_directory(work)?;
    // Cargo interprets a relative target directory against the target checkout.
    let target_dir = prepare_directory(&workspace_root.join(target_dir))?;

    let mut entries = Vec::new();
    log::info!(
        "coverage: {} binaries, {} clusters, debug-like instrumented build",
        in_scope.len(),
        in_scope
            .iter()
            .map(|bin| bin.clusters().len())
            .sum::<usize>()
    );
    let started = std::time::Instant::now();
    for bin in in_scope {
        let Some(executable) = executables.get(&(bin.crate_name.clone(), bin.target.clone()))
        else {
            log::warn!(
                "no bench executable reported for {}/{}; marking all clusters failed",
                bin.crate_name,
                bin.target
            );
            entries.extend(
                bin.clusters()
                    .into_iter()
                    .map(|cluster| CoverageEntry::failed(bin, cluster.name)),
            );
            continue;
        };
        for cluster in bin.clusters() {
            log::info!(
                "coverage: cluster \'{}\' ({} benchmark ID(s))",
                cluster.name,
                cluster.bench_ids.len()
            );
            let started = std::time::Instant::now();
            let entry = cover_cluster(
                &workspace_root,
                bin,
                &cluster,
                executable,
                &target_dir,
                &work,
                &tools,
            );
            log::info!(
                "coverage: cluster \'{}\' {} in {:.1?}",
                cluster.name,
                if entry.status == "ok" {
                    format!("{} file(s), {} dep(s)", entry.files.len(), entry.deps.len())
                } else {
                    "FAILED (will always run)".to_string()
                },
                started.elapsed()
            );
            entries.push(entry);
        }
    }
    let ok = entries.iter().filter(|e| e.status == "ok").count();
    log::info!(
        "coverage: {} of {} clusters covered in {:.1?}",
        ok,
        entries.len(),
        started.elapsed()
    );
    Ok(Coverage { entries })
}

pub(crate) fn prepare_directory(path: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))
}

fn ensure_tool(name: &str) -> Result<PathBuf> {
    find_llvm_tool(name).with_context(|| {
        format!(
            "{name} not found in PATH or in rustc's sysroot (install the \
             llvm-tools-preview rustup component or a matching LLVM in PATH)"
        )
    })
}

/// PATH overrides rustup's matching LLVM tools; callers must ensure version compatibility.
fn find_llvm_tool(name: &str) -> Option<PathBuf> {
    if let Some(path) = which_in_path(name) {
        return Some(path);
    }
    let sysroot = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    if !sysroot.status.success() {
        return None;
    }
    let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_string();
    let host = Command::new("rustc").arg("-vV").output().ok()?;
    if !host.status.success() {
        return None;
    }
    let host = String::from_utf8_lossy(&host.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))?
        .trim()
        .to_string();
    let candidate = PathBuf::from(sysroot)
        .join("lib")
        .join("rustlib")
        .join(host)
        .join("bin")
        .join(name);
    candidate.is_file().then_some(candidate)
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Fast instrumented profile shared by inventory and coverage, isolated from
/// the measuring run's real `[profile.bench]`.
pub(crate) fn cargo_env(target_dir: &Path) -> Vec<(String, String)> {
    vec![
        ("CARGO_ENCODED_RUSTFLAGS".into(), inherited_rustflags()),
        (
            "CARGO_TARGET_DIR".into(),
            target_dir.to_string_lossy().into_owned(),
        ),
        ("CARGO_PROFILE_BENCH_OPT_LEVEL".into(), "0".into()),
        ("CARGO_PROFILE_BENCH_CODEGEN_UNITS".into(), "16".into()),
        ("CARGO_PROFILE_BENCH_LTO".into(), "false".into()),
    ]
}

fn inherited_rustflags() -> String {
    instrumented_rustflags(
        std::env::var("RUSTFLAGS").ok().as_deref(),
        std::env::var("CARGO_ENCODED_RUSTFLAGS").ok().as_deref(),
    )
}

/// Cargo gives encoded flags precedence even when the value is empty. Append
/// instrumentation to that active source, rather than to an ignored RUSTFLAGS.
fn instrumented_rustflags(rustflags: Option<&str>, encoded_rustflags: Option<&str>) -> String {
    let mut flags: Vec<&str> = match encoded_rustflags {
        Some("") => Vec::new(),
        Some(flags) => flags.split('\u{1f}').collect(),
        None => rustflags.unwrap_or_default().split_whitespace().collect(),
    };
    flags.extend(["-C", "instrument-coverage"]);
    flags.join("\u{1f}")
}

fn has_path_remapping(encoded_rustflags: &str) -> bool {
    encoded_rustflags
        .split('\u{1f}')
        .any(|flag| flag == "--remap-path-prefix" || flag.starts_with("--remap-path-prefix="))
}

/// Discard proc-macro/build-script profiles without littering the checkout.
pub(crate) const BUILD_JUNK_PROFILE_PATTERN: &str = "junk/%m_%p.profraw";

/// Avoid cargo's repeated fingerprint checks, preserving the package cwd for
/// fixtures and the common cargo environment. Unsupported benches fail conservatively.
pub(crate) fn direct_bench_command(
    workspace_root: &Path,
    bin: &BenchBinary,
    executable: &Path,
    target_dir: &Path,
) -> Command {
    let crate_dir = workspace_root.join(&bin.crate_dir);
    let mut cmd = Command::new(executable);
    cmd.current_dir(&crate_dir)
        .env("CARGO_MANIFEST_DIR", &crate_dir)
        .env("CARGO_PKG_NAME", &bin.crate_name)
        .env("CARGO_TARGET_DIR", target_dir);
    cmd
}

/// One union build avoids repeated fingerprinting and dependency builds.
/// Unlike per-target measuring builds, it unifies features across all bins.
pub(crate) fn build_all_benches(
    workspace_root: &Path,
    bins: &[&BenchBinary],
    target_dir: &Path,
) -> Result<Executables> {
    // Without -p, cargo would build every workspace bench.
    if bins.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root).envs(cargo_env(target_dir));
    cmd.arg("bench");
    let crates: BTreeSet<&str> = bins.iter().map(|bin| bin.crate_name.as_str()).collect();
    for name in &crates {
        cmd.args(["-p", name]);
    }
    let features: BTreeSet<&str> = bins
        .iter()
        .flat_map(|bin| bin.features.iter().map(String::as_str))
        .collect();
    if !features.is_empty() {
        cmd.arg("--features")
            .arg(features.into_iter().collect::<Vec<_>>().join(","));
    }
    cmd.args(["--no-run", "--message-format", "json"]);
    cmd.env(
        "LLVM_PROFILE_FILE",
        target_dir.join(BUILD_JUNK_PROFILE_PATTERN),
    );
    log::info!(
        "build: {} bench binary(ies) of {} crate(s) in one cargo pass",
        bins.len(),
        crates.len()
    );
    let started = std::time::Instant::now();
    let output = cmd.output().context("failed to build the bench binaries")?;
    log::info!("build: done in {:.1?}", started.elapsed());
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("instrumented bench build failed: {stderr}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let executables = parse_bench_executables(&stdout, workspace_root, bins);
    for bin in bins {
        if !executables.contains_key(&(bin.crate_name.clone(), bin.target.clone())) {
            let bench_targets: Vec<String> = stdout
                .lines()
                .filter_map(|line| {
                    let message = serde_json::from_str::<serde_json::Value>(line).ok()?;
                    let target = message.get("target")?;
                    if is_bench_target(target) {
                        Some(format!(
                            "{}/{}",
                            message
                                .get("manifest_path")
                                .and_then(|m| m.as_str())
                                .unwrap_or("?"),
                            target.get("name").and_then(|n| n.as_str()).unwrap_or("?"),
                        ))
                    } else {
                        None
                    }
                })
                .collect();
            anyhow::bail!(
                "no bench executable reported for {} --bench {}; bench artifacts seen: {bench_targets:?}; root: {}",
                bin.crate_name,
                bin.target,
                workspace_root.canonicalize().unwrap_or_default().display()
            );
        }
    }
    Ok(executables)
}

/// Extract the bench executable paths from cargo's `--message-format=json`
/// build output, keyed by (package name, bench target name).
fn parse_bench_executables(
    stdout: &str,
    workspace_root: &Path,
    bins: &[&BenchBinary],
) -> Executables {
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    stdout
        .lines()
        .filter_map(|line| {
            let message: serde_json::Value = serde_json::from_str(line).ok()?;
            if message.get("reason")?.as_str()? != "compiler-artifact" {
                return None;
            }
            let target = message.get("target")?;
            if !is_bench_target(target) {
                return None;
            }
            let name = target.get("name")?.as_str()?;
            let manifest = message.get("manifest_path")?.as_str()?;
            let executable = message.get("executable")?.as_str()?;
            let bin = bins.iter().find(|bin| {
                bin.target == name && Path::new(manifest).starts_with(root.join(&bin.crate_dir))
            })?;
            Some((
                (bin.crate_name.clone(), bin.target.clone()),
                PathBuf::from(executable),
            ))
        })
        .collect()
}

fn is_bench_target(target: &serde_json::Value) -> bool {
    target
        .get("kind")
        .and_then(|k| k.as_array())
        .is_some_and(|k| k.iter().any(|v| v.as_str() == Some("bench")))
}

/// Run one cluster once under instrumentation and reduce its profile to
/// executed-function line ranges.
fn cover_cluster(
    workspace_root: &Path,
    bin: &BenchBinary,
    cluster: &Cluster,
    executable: &Path,
    target_dir: &Path,
    work: &Path,
    tools: &Tools,
) -> CoverageEntry {
    match cover_cluster_inner(
        workspace_root,
        bin,
        cluster,
        executable,
        target_dir,
        work,
        tools,
    ) {
        Ok(coverage) => CoverageEntry {
            crate_name: bin.crate_name.clone(),
            target: bin.target.clone(),
            cluster: cluster.name.clone(),
            status: "ok".into(),
            files: coverage.files,
            deps: coverage.deps,
        },
        Err(err) => {
            log::warn!(
                "coverage of {}/{} cluster {} failed: {err:#}; cluster will always run",
                bin.crate_name,
                bin.target,
                cluster.name
            );
            CoverageEntry::failed(bin, cluster.name.clone())
        }
    }
}

fn cover_cluster_inner(
    workspace_root: &Path,
    bin: &BenchBinary,
    cluster: &Cluster,
    executable: &Path,
    target_dir: &Path,
    work: &Path,
    tools: &Tools,
) -> Result<ClusterCoverage> {
    let slug = format!("{}-{}-{}", bin.crate_name, bin.target, cluster.name);
    let profraw = work.join(format!("{slug}.profraw"));
    let _ = std::fs::remove_file(&profraw);
    let profdata = work.join(format!("{slug}.profdata"));

    let mut cmd = direct_bench_command(workspace_root, bin, executable, target_dir);
    cmd.env("LLVM_PROFILE_FILE", &profraw);
    cmd.args(["--test", "--noplot", &cluster.coverage_filter()]);
    let output = cmd
        .output()
        .with_context(|| format!("failed to run cluster {}", cluster.name))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bench run for cluster {} failed: {stderr}", cluster.name);
    }
    if !profraw.is_file() {
        bail!("no profile written for cluster {}", cluster.name);
    }

    let status = Command::new(&tools.profdata)
        .args(["merge", "-sparse"])
        .arg(&profraw)
        .arg("-o")
        .arg(&profdata)
        .status()
        .context("failed to run llvm-profdata merge")?;
    if !status.success() {
        bail!("llvm-profdata merge failed for cluster {}", cluster.name);
    }

    // JSON is the default export format, not an accepted -format value.
    let output = Command::new(&tools.cov)
        .arg("export")
        .arg(format!("-instr-profile={}", profdata.display()))
        .arg(executable)
        .output()
        .context("failed to run llvm-cov export")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("llvm-cov export failed: {stderr}");
    }
    let json = String::from_utf8_lossy(&output.stdout);

    reduce_executed_functions(&json, workspace_root)
}

/// llvm-cov export region tuple indices. A code region is
/// `[line, col, line, col, count, file_id, kind]`; an expansion region has one
/// extra `expanded_file_id` before `kind`, but the file index is always 5.
const REGION_LINE_START: usize = 0;
const REGION_LINE_END: usize = 2;
const REGION_FILE_ID: usize = 5;

/// Retain all regions of entered functions, with workspace-relative paths and
/// external package names. Non-workspace ranges cannot intersect source diffs.
fn reduce_executed_functions(
    llvm_cov_json: &str,
    workspace_root: &Path,
) -> Result<ClusterCoverage> {
    #[derive(Deserialize)]
    struct CovJson {
        data: Vec<Data>,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(default)]
        functions: Vec<Function>,
    }
    #[derive(Deserialize)]
    struct Function {
        count: u64,
        #[serde(default)]
        filenames: Vec<String>,
        #[serde(default)]
        regions: Vec<Vec<u64>>,
    }

    let parsed: CovJson =
        serde_json::from_str(llvm_cov_json).context("failed to parse llvm-cov export JSON")?;
    let mut files: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    let mut deps: BTreeSet<String> = BTreeSet::new();
    for data in &parsed.data {
        for function in &data.functions {
            if function.count == 0 {
                continue;
            }
            for region in &function.regions {
                if region.len() <= REGION_FILE_ID {
                    continue;
                }
                let line_start = region[REGION_LINE_START];
                let line_end = region[REGION_LINE_END];
                let file_id = usize::try_from(region[REGION_FILE_ID])
                    .context("coverage file index overflow")?;
                if line_start == 0 || line_end == 0 {
                    continue;
                }
                let Some(path) = function.filenames.get(file_id) else {
                    continue;
                };
                if let Ok(rel) = Path::new(path).strip_prefix(workspace_root) {
                    let rel = rel
                        .to_str()
                        .filter(|path| !path.is_empty())
                        .context("invalid workspace coverage path")?
                        .to_owned();
                    let Ok(start) = u32::try_from(line_start) else {
                        continue;
                    };
                    let Ok(end) = u32::try_from(line_end) else {
                        continue;
                    };
                    files.entry(rel).or_default().push((start, end));
                } else if let Some(name) = external_package_name(path) {
                    deps.insert(name);
                }
            }
        }
    }
    for ranges in files.values_mut() {
        *ranges = merge_ranges(std::mem::take(ranges));
    }
    anyhow::ensure!(
        !files.is_empty(),
        "coverage has no attributable workspace ranges (possibly remapped paths)"
    );
    Ok(ClusterCoverage { files, deps })
}

#[derive(Debug, PartialEq)]
struct ClusterCoverage {
    files: BTreeMap<String, Vec<(u32, u32)>>,
    deps: BTreeSet<String>,
}

/// Extract a package name from registry or git-checkout paths.
/// Known gap: git repository names are assumed to match package names;
/// mismatches can under-select and are not caught by the lockfile fallback.
fn external_package_name(path: &str) -> Option<String> {
    const REGISTRY: &str = "registry/src/";
    const CHECKOUTS: &str = "git/checkouts/";
    if let Some(idx) = path.rfind(REGISTRY) {
        let mut segments = path[idx + REGISTRY.len()..].split('/');
        segments.next(); // registry identifier (URL or hash)
        let name_version = segments.next()?;
        return registry_package_name(name_version);
    }
    if let Some(idx) = path.rfind(CHECKOUTS) {
        let mut segments = path[idx + CHECKOUTS.len()..].split('/');
        let repo_hash = segments.next()?;
        // Strip the trailing `-<hash>` from `<repo>-<hash>`; cargo's checkout
        // hashes are ≥ 16 hex characters.
        let (repo, hash) = repo_hash.rsplit_once('-')?;
        return (hash.len() >= 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| repo.to_string());
    }
    None
}

/// Split at the first version suffix, preserving hyphenated names and
/// handling prerelease/build suffixes (e.g. `httpmock-0.8.0-alpha.1`).
fn registry_package_name(name_version: &str) -> Option<String> {
    name_version
        .match_indices('-')
        .find(|(idx, _)| is_semver(&name_version[idx + 1..]))
        .map(|(idx, _)| name_version[..idx].to_owned())
}

/// Accept numeric version components, optional prerelease, and nonempty build metadata.
fn is_semver(s: &str) -> bool {
    let core = match s.split_once('+') {
        Some((core, build)) if !build.is_empty() => core,
        Some(_) => return false,
        None => s,
    };
    let core = match core.split_once('-') {
        Some((core, prerelease)) if is_prerelease(prerelease) => core,
        Some(_) => return false,
        None => core,
    };
    is_version(core)
}

fn is_prerelease(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|seg| {
            !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn is_version(s: &str) -> bool {
    s.split('.')
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Merge inclusive ranges into sorted, non-overlapping ones (adjacent ranges
/// one line apart are also merged: `1-2` and `3-4` become `1-4`).
pub fn merge_ranges(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= last_end.saturating_add(1) => {
                *last_end = (*last_end).max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_remapped_coverage_is_failed_evidence() {
        for fixture in [
            r#"{"data":[]}"#,
            r#"{"data":[{"functions":[{"count":1,"filenames":["/remapped/src/lib.rs"],"regions":[[1,1,5,1,1,0,0]]}]}]}"#,
            r#"{"data":[{"functions":[{"count":1,"filenames":["/workspace-other/src/lib.rs"],"regions":[[1,1,5,1,1,0,0]]}]}]}"#,
        ] {
            assert!(reduce_executed_functions(fixture, Path::new("/workspace")).is_err());
        }
    }

    #[test]
    fn active_rustflags_preserve_cargo_precedence_and_add_instrumentation() {
        assert_eq!(
            instrumented_rustflags(None, None),
            "-C\u{1f}instrument-coverage"
        );
        assert_eq!(
            instrumented_rustflags(Some("-C opt-level=1"), None),
            "-C\u{1f}opt-level=1\u{1f}-C\u{1f}instrument-coverage"
        );
        assert_eq!(
            instrumented_rustflags(Some("--remap-path-prefix=src=/redacted"), Some("")),
            "-C\u{1f}instrument-coverage"
        );
        assert_eq!(
            instrumented_rustflags(Some("ignored"), Some("--cfg\u{1f}label=\"two words\"")),
            "--cfg\u{1f}label=\"two words\"\u{1f}-C\u{1f}instrument-coverage"
        );
    }

    #[test]
    fn path_remapping_checks_both_flag_forms_in_the_active_source() {
        for flags in [
            instrumented_rustflags(Some("--remap-path-prefix=src=/redacted"), None),
            instrumented_rustflags(Some("--remap-path-prefix src=/redacted"), None),
            instrumented_rustflags(None, Some("--remap-path-prefix=src=/redacted")),
            instrumented_rustflags(None, Some("--remap-path-prefix\u{1f}src=/redacted")),
        ] {
            assert!(has_path_remapping(&flags), "{flags:?}");
        }
        for flags in [
            instrumented_rustflags(None, None),
            instrumented_rustflags(Some("--remap-path-prefix=src=/redacted"), Some("")),
            instrumented_rustflags(Some("--cfg remapping_is_not_enabled"), None),
        ] {
            assert!(!has_path_remapping(&flags), "{flags:?}");
        }
    }

    #[test]
    fn partial_remapping_is_untrusted_even_when_benchmark_ranges_survive() {
        let mixed = r#"{"data":[{"functions":[
            {"count":1,"filenames":["/workspace/benches/main.rs"],"regions":[[1,1,5,1,1,0,0]]},
            {"count":1,"filenames":["/redacted/lib.rs"],"regions":[[10,1,20,1,1,0,0]]}
        ]}]}"#;
        let reduced = reduce_executed_functions(mixed, Path::new("/workspace")).unwrap();
        assert_eq!(reduced.files.len(), 1);
        assert!(reduced.files.contains_key("benches/main.rs"));
        // The export alone cannot distinguish a remapped library from a
        // third-party dependency; the active compiler flags must reject it.
        let flags = instrumented_rustflags(Some("--remap-path-prefix=src=/redacted"), None);
        assert!(has_path_remapping(&flags));
    }

    #[test]
    fn directory_paths_remain_absolute_across_subprocess_cwds() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::Builder::new()
            .prefix(".bench-selector-path-test-")
            .tempdir_in(&cwd)
            .unwrap();
        let relative = temp.path().strip_prefix(&cwd).unwrap().join("profiles");
        let work = prepare_directory(&relative).unwrap();
        let workspace = prepare_directory(&temp.path().join("workspace")).unwrap();
        let target = prepare_directory(&workspace.join("target/coverage")).unwrap();
        assert!(work.is_absolute());
        assert_eq!(work, temp.path().join("profiles").canonicalize().unwrap());
        assert_eq!(target, workspace.join("target/coverage"));
        assert_ne!(work, workspace.join(&relative));
    }

    #[test]
    fn parses_bench_executables_from_build_messages() {
        let messages = r#"
{"reason":"compiler-artifact","manifest_path":"/ws/libdd-ddsketch/Cargo.toml","target":{"name":"ddsketch_bench","kind":["bench"]},"executable":"/ws/target/bench-selection/debug/deps/ddsketch_bench-123","profile":{}}
{"reason":"compiler-artifact","manifest_path":"/ws/libdd-trace-utils/Cargo.toml","target":{"name":"main","kind":["bench"]},"executable":"/ws/target/bench-selection/debug/deps/main-456","profile":{}}
{"reason":"compiler-artifact","manifest_path":"/ws/libdd-trace-utils/Cargo.toml","target":{"name":"libdd-trace-utils","kind":["lib"]},"executable":null,"profile":{}}
{"reason":"compiler-artifact","manifest_path":"/ws/unrelated-crate/Cargo.toml","target":{"name":"main","kind":["bench"]},"executable":"/ws/other","profile":{}}
{"reason":"build-finished","success":true}
"#;
        let bin = |name: &str, target: &str| BenchBinary {
            crate_name: name.to_string(),
            target: target.to_string(),
            crate_dir: name.to_string(),
            features: Vec::new(),
            bench_ids: Vec::new(),
        };
        let bins = [
            bin("libdd-ddsketch", "ddsketch_bench"),
            bin("libdd-trace-utils", "main"),
        ];
        let executables =
            parse_bench_executables(messages, Path::new("/ws"), &bins.iter().collect::<Vec<_>>());
        assert_eq!(
            executables.get(&("libdd-ddsketch".to_string(), "ddsketch_bench".to_string())),
            Some(&std::path::PathBuf::from(
                "/ws/target/bench-selection/debug/deps/ddsketch_bench-123"
            ))
        );
        assert_eq!(
            executables.get(&("libdd-trace-utils".to_string(), "main".to_string())),
            Some(&std::path::PathBuf::from(
                "/ws/target/bench-selection/debug/deps/main-456"
            ))
        );
        assert_eq!(executables.len(), 2);
    }

    #[test]
    fn merges_overlapping_ranges() {
        let merged = merge_ranges(vec![(5, 9), (1, 2), (10, 12), (20, 25), (24, 30)]);
        assert_eq!(merged, [(1, 2), (5, 12), (20, 30)]);
    }

    #[test]
    fn registry_package_names_resolve_with_prerelease_and_build_suffixes() {
        let pkg = |dir: &str| {
            external_package_name(&format!(
                "/home/u/.cargo/registry/src/index/{dir}/src/lib.rs"
            ))
        };
        assert_eq!(pkg("serde-1.0.210").as_deref(), Some("serde"));
        assert_eq!(
            pkg("allocator-api2-0.2.1").as_deref(),
            Some("allocator-api2")
        );
        assert_eq!(pkg("ast-grep-0.34.0").as_deref(), Some("ast-grep"));
        assert_eq!(pkg("httpmock-0.8.0-alpha.1").as_deref(), Some("httpmock"));
        assert_eq!(
            pkg("wasi-0.11.0+wasi-snapshot-preview1").as_deref(),
            Some("wasi")
        );
        assert_eq!(pkg("foo-1.0.0-rc.1+build.2").as_deref(), Some("foo"));
        assert_eq!(pkg("not-a-version").as_deref(), None);
    }

    #[test]
    fn reduces_executed_functions_fixture() {
        let fixture = r#"{"data":[{"functions":[
            {"count":2,"filenames":["/ws/libdd-x/src/hot.rs"],
             "regions":[[10,1,10,20,2,0,0],[12,1,14,2,1,0,0]]},
            {"count":0,"filenames":["/ws/libdd-x/src/cold.rs"],
             "regions":[[1,1,9,1,0,0,0]]},
            {"count":7,"filenames":["/home/u/.cargo/registry/src/x.rs"],
             "regions":[[1,1,2,1,7,0,0]]},
            {"count":1,"filenames":["/ws/libdd-x/src/macro_user.rs"],
             "regions":[[5,1,5,9,1,0,3,1],[30,1,30,30,1,0,0]]}
        ]}]}"#;
        let reduced = reduce_executed_functions(fixture, Path::new("/ws")).expect("reduce fixture");

        let mut expected = ClusterCoverage {
            files: BTreeMap::new(),
            deps: BTreeSet::new(),
        };
        // Include macro invocation sites, but not unentered functions/external ranges.
        expected
            .files
            .insert("libdd-x/src/hot.rs".to_string(), vec![(10, 10), (12, 14)]);
        expected.files.insert(
            "libdd-x/src/macro_user.rs".to_string(),
            vec![(5, 5), (30, 30)],
        );
        assert_eq!(reduced, expected);
    }
}
