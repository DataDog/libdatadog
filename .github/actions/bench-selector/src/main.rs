// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Discover criterion benchmarks, collect per-cluster function coverage, and
//! intersect it with changed lines and dependencies. Missing or failed evidence
//! widens the selection. Intermediate JSON is available through `--debug-dir`.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};

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
    /// Workspace root of the benchmarked checkout (default: cwd).
    #[arg(long, default_value = ".")]
    workspace_root: PathBuf,
    /// Restrict to these crate names (space or comma separated).
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    crates: Option<Vec<String>>,
    /// Dedicated instrumented build directory, relative to workspace-root.
    #[arg(long, default_value = "target/bench-selector-coverage")]
    target_dir: PathBuf,
    /// Base ref to diff against (without the `origin/` prefix), e.g. `main`.
    #[arg(long)]
    base_ref: String,
    /// Write intermediate inventory and coverage JSON here for debugging.
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    /// Keep raw profiles here for debugging; defaults to an auto-cleaned temp dir.
    #[arg(long)]
    work: Option<PathBuf>,
    /// Selection output file (default: stdout).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let crates = normalize_crates(cli.crates);
    let (inventory, executables) =
        inventory::discover(&cli.workspace_root, crates.as_deref(), &cli.target_dir)?;
    write_debug(cli.debug_dir.as_deref(), "bench-inventory.json", &inventory)?;

    let temp;
    let work_dir = match &cli.work {
        Some(path) => path.as_path(),
        None => {
            temp = tempfile::tempdir().context("failed to create temp dir")?;
            temp.path()
        }
    };
    let coverage = coverage::run(
        &cli.workspace_root,
        &inventory,
        &executables,
        &cli.target_dir,
        work_dir,
    )?;
    write_debug(cli.debug_dir.as_deref(), "bench-coverage.json", &coverage)?;

    fetch_base(&cli.base_ref, &cli.workspace_root)?;
    let changed = diff::changed_ranges(&cli.base_ref, &cli.workspace_root)?;
    let lockfile = lockfile::lockfile_changes(&cli.base_ref, &cli.workspace_root)?;
    let manifests = manifest::manifest_changes(&cli.base_ref, &cli.workspace_root, &changed);
    let self_test = std::env::var("BENCH_SELECTOR_SELF_TEST")
        .map(|v| v == "1")
        .unwrap_or(false);
    let selection = select::select(
        &inventory, &coverage, &changed, &lockfile, &manifests, self_test,
    );
    log::info!(
        "selected {}/{} clusters across {}/{} binaries",
        selection.summary.clusters_selected,
        selection.summary.clusters_total,
        selection.summary.binaries_selected,
        selection.summary.binaries_total
    );
    write_json(cli.out.as_deref(), &selection)
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

// Keep fetch's cwd aligned with the diff; a failed fetch can use a local base ref.
fn fetch_base(base_ref: &str, workspace_root: &Path) -> Result<()> {
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

fn write_debug<T: serde::Serialize>(dir: Option<&Path>, name: &str, value: &T) -> Result<()> {
    if let Some(dir) = dir {
        write_json(Some(&dir.join(name)), value)?;
    }
    Ok(())
}

fn write_json<T: serde::Serialize>(out: Option<&Path>, value: &T) -> Result<()> {
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
            let cli = Cli::try_parse_from(
                ["bench-selector", "--base-ref", "main", "--crates"]
                    .into_iter()
                    .chain(arguments),
            )
            .unwrap();
            assert_eq!(
                normalize_crates(cli.crates).unwrap(),
                ["libdd-x", "libdd-y"]
            );
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
