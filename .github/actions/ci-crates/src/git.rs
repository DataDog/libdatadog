// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use std::process::Command;

/// Run `git fetch --depth=1 origin <base_ref>` to ensure the base ref is available locally.
pub fn fetch_base(base_ref: &str) -> Result<()> {
    // Strip "origin/" prefix if present — we pass it to `git fetch origin <branch>`
    let branch = base_ref.strip_prefix("origin/").unwrap_or(base_ref);
    log::info!("Fetching base branch: {branch}");

    let status = Command::new("git")
        .args([
            "fetch",
            "--depth=1",
            "origin",
            &format!("{branch}:refs/remotes/origin/{branch}"),
        ])
        .status()
        .context("Failed to run git fetch")?;

    if !status.success() {
        // Non-fatal: warn but do not abort (mirrors `|| true` in the bash version)
        log::warn!("git fetch for {base_ref} returned non-zero exit code; continuing");
    }

    Ok(())
}

/// Return the list of files changed between `<base_ref>...HEAD` (three-dot diff).
///
/// Uses `git diff --name-only <base_ref>...HEAD`.
pub fn changed_files(base_ref: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("{base_ref}...HEAD")])
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff failed against {base_ref}: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = stdout
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(files)
}
