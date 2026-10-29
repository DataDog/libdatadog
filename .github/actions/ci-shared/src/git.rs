// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, bail};
use std::process::Command;

fn run_git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .context("Failed to run git")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn rev_parse(rev: &str) -> Result<String> {
    Ok(run_git(&["rev-parse", rev])?.trim().to_string())
}

pub fn merge_base(a: &str, b: &str) -> Result<String> {
    Ok(run_git(&["merge-base", a, b])?.trim().to_string())
}

pub fn fetch_base(base_ref: &str) -> Result<()> {
    log::info!("Fetching base branch: {base_ref}");

    let status = Command::new("git")
        .args([
            "fetch",
            "origin",
            &format!("{base_ref}:refs/remotes/origin/{base_ref}"),
        ])
        .status()
        .context("Failed to run git fetch")?;

    if !status.success() {
        log::warn!("git fetch for {base_ref} returned non-zero exit code; continuing");
    }

    Ok(())
}

fn diff_files(range: &str) -> Result<Vec<String>> {
    let stdout = run_git(&["diff", "--name-only", range])?;
    Ok(stdout
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.contains(".github/"))
        .collect())
}

pub fn changed_files(base_ref: &str) -> Result<Vec<String>> {
    diff_files(&format!("origin/{base_ref}...HEAD"))
}

pub fn changed_files_in_range(range: &str) -> Result<Vec<String>> {
    diff_files(range)
}

pub fn has_workflow_changes(base_ref: &str) -> Result<bool> {
    has_workflow_changes_in_range(&format!("origin/{base_ref}...HEAD"))
}

pub fn has_workflow_changes_in_range(range: &str) -> Result<bool> {
    let stdout = run_git(&["diff", "--name-only", range])?;
    Ok(stdout.lines().any(|l| l.starts_with(".github/workflows/")))
}
