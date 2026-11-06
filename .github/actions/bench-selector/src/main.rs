// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! `bench-selector` — selects which criterion benchmarks to run on a PR.
//!
//! Three stages, each independently runnable and JSON-serialized so CI can
//! cache and inspect intermediate results:
//!
//! * `inventory`  — level-1 clustering: every bench binary, its features, and the benchmark IDs
//!   criterion `--list` reports for it (level-2 name clusters are derived from the IDs).
//! * `coverage`   — one instrumented `--test` run per name cluster, reduced to executed-function
//!   line ranges (`-C instrument-coverage`).
//! * `select`     — intersect the coverage with the PR's changed lines and emit, per binary, the
//!   exact benchmark IDs (and regex filter) to run.
//!
//! All stages are fail-conservative: missing or failing evidence selects more
//! benchmarks, never fewer.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod cluster;
mod coverage;
mod diff;
mod git;
mod inventory;
mod lockfile;
mod manifest;
mod select;

#[derive(Parser)]
#[command(about = "Select criterion benchmarks to run from a PR diff")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Enumerate bench binaries and their criterion benchmark IDs.
    Inventory {
        /// Workspace root of the benchmarked checkout (default: cwd).
        #[arg(long, default_value = ".")]
        workspace_root: PathBuf,
        /// Restrict to these crate names (space or comma separated).
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        crates: Option<Vec<String>>,
        /// Build the `--list` invocations in the coverage pass's instrumented
        /// target dir, so a following `coverage --target-dir <dir>` reuses the
        /// same build (one build of the dependency graph instead of two).
        /// Standalone invocations without this flag stay uninstrumented.
        #[arg(long)]
        target_dir: Option<PathBuf>,
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run one instrumented criterion `--test` pass per name cluster.
    Coverage {
        /// Workspace root of the benchmarked checkout (default: cwd).
        #[arg(long, default_value = ".")]
        workspace_root: PathBuf,
        /// Inventory JSON produced by `bench-selector inventory`.
        #[arg(long)]
        inventory: PathBuf,
        /// Restrict to these crate names (space or comma separated).
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        crates: Option<Vec<String>>,
        /// Dedicated cargo target dir for the instrumented build (kept separate
        /// from the clean measuring build cache). Relative to workspace-root.
        #[arg(long, default_value = "target/bench-selector-coverage")]
        target_dir: PathBuf,
        /// Scratch dir for profile data. By default a system temp dir is used
        /// and removed when the run finishes; pass an explicit directory to
        /// keep the raw `.profraw`/`.profdata` files for debugging.
        #[arg(long)]
        work: Option<PathBuf>,
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Intersect coverage with the changed lines and emit the run selection.
    Select {
        /// Workspace root of the benchmarked checkout (default: cwd).
        #[arg(long, default_value = ".")]
        workspace_root: PathBuf,
        /// Inventory JSON produced by `bench-selector inventory`.
        #[arg(long)]
        inventory: PathBuf,
        /// Coverage JSON produced by `bench-selector coverage`.
        #[arg(long)]
        coverage: PathBuf,
        /// Base ref to diff against (without the `origin/` prefix), e.g. `main`.
        #[arg(long)]
        base_ref: String,
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    // Default to info so CI job logs show the pass's progress without env
    // plumbing; RUST_LOG still overrides (e.g. RUST_LOG=warn to silence).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Inventory {
            workspace_root,
            crates,
            target_dir,
            out,
        } => {
            let crates = normalize_crates(crates);
            let inv =
                inventory::discover(&workspace_root, crates.as_deref(), target_dir.as_deref())?;
            log::info!(
                "discovered {} bench binaries, {} benchmark IDs",
                inv.binaries.len(),
                inv.binaries
                    .iter()
                    .map(|b| b.bench_ids.len())
                    .sum::<usize>()
            );
            write_json(&out, &inv)
        }
        Cmd::Coverage {
            workspace_root,
            inventory,
            crates,
            target_dir,
            work,
            out,
        } => {
            let inv: inventory::Inventory = read_json(&inventory, "inventory")?;
            let crates = normalize_crates(crates);
            // Scratch space for profiles: an OS temp dir removed on exit by
            // default; an explicit --work is kept for debugging.
            let temp;
            let work_dir: &std::path::Path = match &work {
                Some(path) => path,
                None => {
                    temp = tempfile::tempdir().context("failed to create temp dir")?;
                    temp.path()
                }
            };
            let cov = coverage::run(
                &workspace_root,
                &inv,
                crates.as_deref(),
                &target_dir,
                work_dir,
            )?;
            let ok = cov.entries.iter().filter(|e| e.status == "ok").count();
            log::info!("covered {} of {} clusters", ok, cov.entries.len());
            // Dropping `temp` here removes the profraw/profdata scratch space
            // now that the reduced coverage JSON is written.
            write_json(&out, &cov)
        }
        Cmd::Select {
            workspace_root,
            inventory,
            coverage,
            base_ref,
            out,
        } => {
            let inv: inventory::Inventory = read_json(&inventory, "inventory")?;
            let cov: coverage::Coverage = read_json(&coverage, "coverage")?;
            fetch_base(&base_ref, &workspace_root)?;
            let changed = diff::changed_ranges(&base_ref, &workspace_root)?;
            // Lockfile bumps change compiled dependency code without any
            // workspace source diff; scope them by package name against the
            // packages each cluster executes.
            let lockfile = lockfile::lockfile_changes(&base_ref, &workspace_root)?;
            // Manifests that only change dependency declarations are scoped
            // like lockfile bumps: the changed package names join the same
            // coverage intersection.
            let manifests = manifest::manifest_changes(&base_ref, &workspace_root, &changed);
            if lockfile.is_changed() {
                log::info!(
                    "lockfile changed packages: {} (unattributable: {})",
                    lockfile
                        .packages
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    lockfile.unattributable
                );
            }
            let self_test = std::env::var("BENCH_SELECTOR_SELF_TEST")
                .map(|v| v == "1")
                .unwrap_or(false);
            let selection = select::select(&inv, &cov, &changed, &lockfile, &manifests, self_test);
            log::info!(
                "selected {}/{} clusters across {}/{} binaries",
                selection.summary.clusters_selected,
                selection.summary.clusters_total,
                selection.summary.binaries_selected,
                selection.summary.binaries_total
            );
            write_json(&out, &selection)
        }
    }
}

fn normalize_crates(crates: Option<Vec<String>>) -> Option<Vec<String>> {
    crates.map(|values| {
        values
            .iter()
            .flat_map(|value| value.split(|c: char| c == ',' || c.is_whitespace()))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

// Keep fetch's working directory aligned with the subsequent diff. As in
// ci_shared, a failed fetch may still use an already available local base ref.
fn fetch_base(base_ref: &str, workspace_root: &std::path::Path) -> Result<()> {
    let status = std::process::Command::new("git")
        .current_dir(workspace_root)
        .args([
            "fetch",
            "origin",
            &format!("{base_ref}:refs/remotes/origin/{base_ref}"),
        ])
        .status()
        .context("failed to run git fetch")?;
    if !status.success() {
        log::warn!(
            "git fetch for {base_ref} returned non-zero exit code; using any existing base ref"
        );
    }
    Ok(())
}

fn write_json<T: serde::Serialize>(out: &Option<PathBuf>, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    match out {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(path, json)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path, what: &str) -> Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {what} file {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {what} JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_arguments_support_space_comma_and_quoted_forms() {
        for arguments in [
            vec!["libdd-x", "libdd-y"],
            vec!["libdd-x,libdd-y"],
            vec!["libdd-x libdd-y"],
        ] {
            for command in ["inventory", "coverage"] {
                let mut argv = vec!["bench-selector", command];
                if command == "coverage" {
                    argv.extend(["--inventory", "inventory.json"]);
                }
                argv.push("--crates");
                argv.extend(arguments.iter().copied());
                let cli = Cli::try_parse_from(argv).unwrap();
                let crates = match cli.cmd {
                    Cmd::Inventory { crates, .. } | Cmd::Coverage { crates, .. } => {
                        normalize_crates(crates)
                    }
                    _ => unreachable!(),
                };
                assert_eq!(crates.unwrap(), ["libdd-x", "libdd-y"]);
            }
        }
    }

    #[test]
    fn fetch_and_diff_use_the_target_checkout_not_the_callers_directory() {
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
        std::fs::write(repo.join("old.rs"), "fn hot() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["branch", "base"]);
        git(&["remote", "add", "origin", repo.to_str().unwrap()]);
        std::fs::rename(repo.join("old.rs"), repo.join("new.rs")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "rename"]);
        fetch_base("base", repo).unwrap();
        let changed = diff::changed_ranges("base", repo).unwrap();
        assert!(changed.contains_key("old.rs"));
        assert!(changed.contains_key("new.rs"));
    }
}
