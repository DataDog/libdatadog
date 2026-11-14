// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared git plumbing for the structural comparisons: resolving the merge
//! base of the PR head with the base branch, and fetching a file's blob
//! content at a revision. Both match the three-dot diff semantics used
//! everywhere else in the pipeline.

use std::path::Path;
use std::process::Command;

/// The merge base of HEAD and `origin/<base_ref>`, matching the three-dot
/// diff semantics used everywhere else in the pipeline.
pub(crate) fn merge_base(base_ref: &str, workspace_root: &Path) -> anyhow::Result<String> {
    let spec = format!("origin/{base_ref}");
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["merge-base", &spec, "HEAD"])
        .output()
        .map_err(|err| anyhow::anyhow!("git merge-base: {err}"))?;
    anyhow::ensure!(
        output.status.success(),
        "git merge-base {spec} HEAD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Fetch a blob from a revision. `Ok(None)` means the path does not exist on
/// that side; an error means git itself failed (conservative handling).
pub(crate) fn show_blob(
    rev: &str,
    path: &str,
    workspace_root: &Path,
) -> anyhow::Result<Option<String>> {
    let rev_exists = Command::new("git")
        .current_dir(workspace_root)
        .args(["cat-file", "-e", rev])
        .status()
        .map_err(|err| anyhow::anyhow!("git cat-file: {err}"))?
        .success();
    anyhow::ensure!(rev_exists, "revision {rev} does not exist");
    let spec = format!("{rev}:{path}");
    let exists = Command::new("git")
        .current_dir(workspace_root)
        .args(["cat-file", "-e", &spec])
        .status()
        .map_err(|err| anyhow::anyhow!("git cat-file: {err}"))?
        .success();
    if !exists {
        return Ok(None);
    }
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["show", &spec])
        .output()
        .map_err(|err| anyhow::anyhow!("git show: {err}"))?;
    anyhow::ensure!(
        output.status.success(),
        "git show {spec} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}
