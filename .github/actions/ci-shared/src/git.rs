// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use std::process::Command;

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

pub fn changed_files(base_ref: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("origin/{base_ref}...HEAD")])
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff failed against origin/{base_ref}: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = stdout
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.contains(".github/"))
        .collect();

    Ok(files)
}
