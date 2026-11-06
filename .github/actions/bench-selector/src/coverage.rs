// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! One-shot instrumented coverage per name cluster.
//!
//! For every bench binary:
//!
//! 1. Build the whole dependency graph (dependencies included, so changes in library crates
//!    exercised by a bench are attributed to the library's source lines) with `-C
//!    instrument-coverage`, in a dedicated target dir so the clean (measuring) build cache is never
//!    polluted, and with a debug-like profile override (opt-level 0, no LTO, 16 codegen units) for
//!    build speed — which source functions are entered does not depend on optimization.
//! 2. For every name cluster of that binary, run the bench binary once per benchmark — criterion's
//!    `--test` mode — filtered to the cluster, with a dedicated `LLVM_PROFILE_FILE`, so each
//!    cluster's profile is attributable.
//! 3. Merge the profile with `llvm-profdata` and export it with `llvm-cov` as JSON, then reduce it
//!    to *executed function* line ranges: a function the cluster entered contributes ALL of its
//!    line ranges, not just the ones taken in the single run, so a changed line inside a
//!    partially-executed function of an entered function still selects the cluster
//!    (function-granularity matching).
//!
//! A cluster whose instrumented run or coverage extraction fails is recorded
//! with status `failed`; the selection step then always runs it (fail
//! conservative), never skips it.

use crate::cluster::Cluster;
use crate::inventory::BenchBinary;
use crate::inventory::Inventory;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Per-cluster coverage data, reduced to executed-function line ranges keyed
/// by workspace-relative file path.
#[derive(Debug, Serialize, Deserialize)]
pub struct Coverage {
    pub entries: Vec<CoverageEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub crate_name: String,
    pub target: String,
    pub cluster: String,
    /// `ok` or `failed`; a `failed` entry carries no line data and always
    /// selects its cluster downstream.
    pub status: String,
    /// Merged, sorted, non-overlapping inclusive `[start, end]` line ranges.
    pub files: BTreeMap<String, Vec<(u32, u32)>>,
    /// External (registry/git-checkout) packages whose code this cluster
    /// executed, by package name. Used to scope lockfile bumps to the
    /// benchmarks that actually measure the changed dependencies; empty when
    /// the cluster exercises no non-workspace code (or in coverage data from
    /// before this field existed).
    #[serde(default)]
    pub deps: BTreeSet<String>,
}

/// Resolved llvm coverage tools matching the active rustc.
struct Tools {
    profdata: PathBuf,
    cov: PathBuf,
}

/// Run the coverage pass for every binary of `inventory` (optionally restricted
/// to some crate names).
///
/// `work` is the scratch directory for profiles and profdata files. It is
/// also where compiler-side junk profiles (from instrumented proc-macros and
/// build scripts, see [`BUILD_JUNK_PROFILE_PATTERN`]) land, so everything is
/// removed together when the caller drops the directory — which `main` does
/// by default via a `tempfile::TempDir` unless `--work` pins an explicit,
/// kept directory for debugging.
pub fn run(
    workspace_root: &Path,
    inventory: &Inventory,
    crates_filter: Option<&[String]>,
    target_dir: &Path,
    work: &Path,
) -> Result<Coverage> {
    crate::inventory::validate_crates_filter(
        crates_filter,
        inventory.binaries.iter().map(|bin| bin.crate_name.as_str()),
    )?;
    if has_path_remapping(&inherited_rustflags()) {
        // A remapped library can disappear while benchmark-file ranges remain.
        // A nonempty export therefore cannot prove complete source attribution.
        log::warn!("path-remapping rustflags prevent trustworthy coverage; all clusters will run");
        let mut entries = Vec::new();
        for bin in &inventory.binaries {
            // Not a let-chain: the tool builds on checkouts older than Rust
            // 1.88, where let-chains are not stable.
            if crates_filter
                .is_some_and(|filter| !filter.iter().any(|name| name == &bin.crate_name))
            {
                continue;
            }
            for cluster in bin.clusters() {
                entries.push(CoverageEntry {
                    crate_name: bin.crate_name.clone(),
                    target: bin.target.clone(),
                    cluster: cluster.name,
                    status: "failed".into(),
                    files: BTreeMap::new(),
                    deps: BTreeSet::new(),
                });
            }
        }
        return Ok(Coverage { entries });
    }
    let tools = Tools {
        profdata: ensure_tool("llvm-profdata")?,
        cov: ensure_tool("llvm-cov")?,
    };
    // llvm-cov reports absolute file paths, and coverage files are normalized
    // against this root — canonicalize so a relative `--workspace-root .` works.
    let workspace_root = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    // Scratch paths are caller-relative, but Cargo/benchmarks change cwd.
    let work = prepare_directory(work)?;
    // Cargo interprets a relative target directory against the target checkout.
    let target_dir = prepare_directory(&workspace_root.join(target_dir))?;

    let mut entries = Vec::new();
    let in_scope: Vec<&BenchBinary> = inventory
        .binaries
        .iter()
        .filter(|bin| {
            crates_filter.is_none_or(|filter| filter.iter().any(|c| c == &bin.crate_name))
        })
        .collect();
    log::info!(
        "coverage: {} binaries, {} clusters, debug-like instrumented build",
        in_scope.len(),
        in_scope
            .iter()
            .map(|bin| bin.clusters().len())
            .sum::<usize>()
    );
    // ONE build for every binary: per-binary invocations would each pay a
    // whole-workspace cargo fingerprint check and compile shared dependencies
    // per feature set. After it, bench binaries run directly without cargo.
    let executables = build_all_benches(&workspace_root, &in_scope, &target_dir)?;
    let started = std::time::Instant::now();
    for bin in in_scope {
        let Some(executable) = executables.get(&(bin.crate_name.clone(), bin.target.clone()))
        else {
            // The union build reported no executable for this target: treat
            // every cluster as failed rather than guessing.
            log::warn!(
                "no bench executable reported for {}/{}; marking all clusters failed",
                bin.crate_name,
                bin.target
            );
            for cluster in bin.clusters() {
                entries.push(CoverageEntry {
                    crate_name: bin.crate_name.clone(),
                    target: bin.target.clone(),
                    cluster: cluster.name,
                    status: "failed".into(),
                    files: BTreeMap::new(),
                    deps: BTreeSet::new(),
                });
            }
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

fn prepare_directory(path: &Path) -> Result<PathBuf> {
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

/// Locate an LLVM coverage tool.
///
/// `llvm-profdata`/`llvm-cov` must match the rustc that produced the raw
/// profiles: a newer system LLVM rejects the profile version rustc writes
/// (seen with LLVM 20 vs rustc's format v10). The rustup `llvm-tools-preview`
/// component ships tools that always match rustc, so it is the preferred
/// source; PATH is used first so a caller can force an explicit LLVM.
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

/// Environment for every cargo invocation of the coverage pass: compiler flags
/// and CARGO_TARGET_DIR must be identical between the `--no-run` build and the
/// cluster runs, or cargo rebuilds (and the profile would differ).
///
/// The bench profile is overridden to a debug-like configuration
/// (opt-level 0, no LTO, 16 codegen units): coverage of which *source
/// functions* a benchmark executes does not depend on optimization — the same
/// functions are entered at opt-level 0 as at the measuring run's opt-level 3,
/// they just are not inlined into each other. The debug-like configuration
/// (plus no LTO and parallel codegen instead of the profile's single codegen
/// unit) builds the dependency graph several times faster, which matters
/// because this pass runs on every PR while the measuring run only runs for
/// selected clusters. `CARGO_PROFILE_*` overrides are cargo-native and scoped
/// to this pass's invocations only; the measuring run still uses the real
/// `[profile.bench]`.
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

fn apply_env(cmd: &mut Command, env: &[(String, String)]) {
    for (k, v) in env {
        cmd.env(k, v);
    }
}

/// Junk profile pattern for compiler-side profiles: `RUSTFLAGS`
/// instrumentation also applies to proc-macros and build scripts, whose code
/// runs *inside rustc/cargo* during compilation; without `LLVM_PROFILE_FILE`
/// they write `default_%m_%p.profraw` files in the cwd. The junk pattern
/// keeps the checkout clean and is never read.
pub(crate) const BUILD_JUNK_PROFILE_PATTERN: &str = "junk/%m_%p.profraw";

/// Run a bench binary DIRECTLY, without going through cargo. Cargo
/// re-fingerprints the whole workspace for every `cargo bench` invocation,
/// which dominates wall time on this many-crate workspace; after one union
/// build ([`build_all_benches`]) the binaries need no cargo at all.
///
/// The environment replicates what `cargo bench` provides to the process:
/// the working directory is the package root (benches load relative
/// fixtures from there), plus the common CARGO_* variables and criterion's
/// target dir. Benches needing something beyond this fail their cluster,
/// which fails conservative (always run) - never silently skipped.
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

/// Build every bench binary of `bins` in ONE cargo invocation and return
/// their executable paths, keyed by (crate name, bench target name).
///
/// One invocation instead of one per binary: cargo pays a whole-workspace
/// fingerprint check per invocation, and a single union build also compiles
/// shared dependencies with the same feature unification as the measuring
/// run (`run_benchmarks_ci.sh` builds its whole selection in one invocation
/// too), where per-target builds could compile several variants of a shared
/// dependency.
pub(crate) fn build_all_benches(
    workspace_root: &Path,
    bins: &[&BenchBinary],
    target_dir: &Path,
) -> Result<BTreeMap<(String, String), PathBuf>> {
    let env = cargo_env(target_dir);
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root);
    apply_env(&mut cmd, &env);
    cmd.arg("bench");
    let mut crates: BTreeSet<&str> = BTreeSet::new();
    for bin in bins {
        crates.insert(bin.crate_name.as_str());
    }
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
    // Compiler-side profiles (proc-macros, build scripts) run inside rustc:
    // route them to the junk pattern like the build phase always has.
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
            // Diagnose: the artifacts seen vs the expected selection.
            let bench_targets: Vec<String> = stdout
                .lines()
                .filter_map(|line| {
                    let message = serde_json::from_str::<serde_json::Value>(line).ok()?;
                    let target = message.get("target")?;
                    let bench = target
                        .get("kind")
                        .and_then(|k| k.as_array())
                        .map(|k| k.iter().any(|v| v.as_str() == Some("bench")))
                        .unwrap_or(false);
                    if bench {
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
) -> BTreeMap<(String, String), PathBuf> {
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut executables = BTreeMap::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let Some(target) = message.get("target") else {
            continue;
        };
        let is_bench = target
            .get("kind")
            .and_then(|k| k.as_array())
            .map(|k| k.iter().any(|v| v.as_str() == Some("bench")))
            .unwrap_or(false);
        if !is_bench {
            continue;
        }
        let Some(name) = target.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let Some(manifest) = message.get("manifest_path").and_then(|m| m.as_str()) else {
            continue;
        };
        let Some(executable) = message.get("executable").and_then(|e| e.as_str()) else {
            continue;
        };
        let Some(bin) = bins.iter().find(|bin| {
            bin.target == name && Path::new(manifest).starts_with(root.join(&bin.crate_dir))
        }) else {
            continue;
        };
        executables.insert(
            (bin.crate_name.clone(), bin.target.clone()),
            PathBuf::from(executable),
        );
    }
    executables
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
            CoverageEntry {
                crate_name: bin.crate_name.clone(),
                target: bin.target.clone(),
                cluster: cluster.name.clone(),
                status: "failed".into(),
                files: BTreeMap::new(),
                deps: BTreeSet::new(),
            }
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

    // Merge the single-run profile, dropping zero-count functions to shrink
    // the export.
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

    // Export as JSON (llvm-cov's default export format; `-format=lcov/text`
    // are the only accepted explicit values). Single-dash long-option style
    // and `=`-joined values work on rustup's llvm-cov and system LLVM alike.
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

/// Reduce llvm-cov export JSON to executed-function line ranges per
/// workspace-relative file path.
///
/// A function counts as executed when its entry count is > 0; ALL of its
/// regions then contribute their line ranges (function-granularity matching),
/// and ranges per file are merged into sorted, non-overlapping inclusive
/// ranges. Files outside `workspace_root` (cargo registry, sysroot) are
/// dropped: only this workspace's code can be changed by the PR.
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
                    // Workspace file: normalize to a workspace-relative path so
                    // it can be intersected with the PR diff.
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
                    // Third-party file: record the package name so lockfile
                    // bumps can be scoped to the benchmarks that actually
                    // execute the changed dependency.
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

/// Reduced per-cluster coverage: workspace line ranges plus the external
/// packages whose code the cluster executed.
#[derive(Debug, PartialEq)]
struct ClusterCoverage {
    files: BTreeMap<String, Vec<(u32, u32)>>,
    deps: BTreeSet<String>,
}

/// Extract the external package name from a cargo registry or git-checkout
/// source path, e.g.
/// `…/.cargo/registry/src/<registry>/serde-1.0.210/src/…` -> `serde`, or
/// `…/.cargo/git/checkouts/proptest-9e6d…/<sha>/…` -> `proptest`.
///
/// Git checkouts are named after the *repository*, which is assumed to match
/// the package name (true for this workspace's patched dependencies); when it
/// does not, the lockfile scoping under-selects for that package, which the
/// unattributable-diff fallback cannot see — accepted as a documented gap.
fn external_package_name(path: &str) -> Option<String> {
    const REGISTRY: &str = "registry/src/";
    const CHECKOUTS: &str = "git/checkouts/";
    if let Some(idx) = path.rfind(REGISTRY) {
        let mut segments = path[idx + REGISTRY.len()..].split('/');
        segments.next(); // registry identifier (URL or hash)
        let name_version = segments.next()?;
        // Strip the trailing `-<semver>` from `<name>-<version>`.
        let (name, version) = name_version.rsplit_once('-')?;
        return is_version(version).then(|| name.to_string());
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

fn is_version(s: &str) -> bool {
    let mut parts = s.split('.');
    let mut numeric = parts
        .next()
        .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    numeric &= parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    numeric
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
    fn unknown_coverage_crate_filter_fails_before_tool_discovery() {
        let result = run(
            Path::new("/nonexistent"),
            &Inventory { binaries: vec![] },
            Some(&["missing".into()]),
            Path::new("target"),
            Path::new("work"),
        );
        assert!(result.unwrap_err().to_string().contains("unknown crate"));
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
        // /ws does not exist locally: the canonicalization fallback keeps the
        // literal path, which is exactly what the matching needs.
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
        // Non-bench targets and artifacts of crates outside the selection
        // are not recorded.
        assert!(!executables.contains_key(&(
            "libdd-trace-utils".to_string(),
            "libdd-trace-utils".to_string()
        )));
        assert!(
            !executables.contains_key(&("libdd-trace-utils".to_string(), "unrelated".to_string()))
        );
    }

    #[test]
    fn merges_overlapping_ranges() {
        let merged = merge_ranges(vec![(5, 9), (1, 2), (10, 12), (20, 25), (24, 30)]);
        assert_eq!(merged, [(1, 2), (5, 12), (20, 30)]);
    }

    #[test]
    fn merges_adjacent_ranges() {
        let merged = merge_ranges(vec![(1, 2), (3, 4)]);
        assert_eq!(merged, [(1, 4)]);
    }

    #[test]
    fn reduces_executed_functions_fixture() {
        // Two functions in one workspace file: one executed (count 2, two
        // regions), one not (count 0). A third function lives outside the
        // workspace (cargo registry) and must be dropped.
        let fixture = r#"{
          "data": [
            {
              "functions": [
                {
                  "name": "hot",
                  "count": 2,
                  "filenames": ["/ws/libdd-x/src/hot.rs"],
                  "regions": [
                    [10, 1, 10, 20, 2, 0, 0],
                    [12, 1, 14, 2, 1, 0, 0]
                  ]
                },
                {
                  "name": "cold",
                  "count": 0,
                  "filenames": ["/ws/libdd-x/src/cold.rs"],
                  "regions": [[1, 1, 9, 1, 0, 0, 0]]
                },
                {
                  "name": "dep",
                  "count": 7,
                  "filenames": ["/home/u/.cargo/registry/src/x.rs"],
                  "regions": [[1, 1, 2, 1, 7, 0, 0]]
                },
                {
                  "name": "macro_user",
                  "count": 1,
                  "filenames": ["/ws/libdd-x/src/macro_user.rs"],
                  "regions": [
                    [5, 1, 5, 9, 1, 0, 3, 1],
                    [30, 1, 30, 30, 1, 0, 0]
                  ]
                }
              ]
            }
          ]
        }"#;
        let reduced = reduce_executed_functions(fixture, Path::new("/ws")).expect("reduce fixture");

        let mut expected = ClusterCoverage {
            files: BTreeMap::new(),
            deps: BTreeSet::new(),
        };
        // All regions of the executed function, merged; the cold function
        // never runs; the registry file is dropped; expansion regions (8
        // elements) contribute their invocation-site range.
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
