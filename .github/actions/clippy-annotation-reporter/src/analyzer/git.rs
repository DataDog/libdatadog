// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context as _, Result};
use log::{debug, info};
use octocrab::Octocrab;
use std::process::Command;

/// Get changed Rust files from the PR
pub(super) async fn get_changed_files(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Vec<String>> {
    info!("Getting changed files from PR #{}...", pr_number);

    let files = octocrab
        .pulls(owner, repo)
        .list_files(pr_number)
        .await
        .context("Failed to list PR files")?;

    // Filter for Rust files only
    let rust_files: Vec<String> = files
        .items
        .into_iter()
        .filter(|file| file.filename.ends_with(".rs"))
        .map(|file| file.filename)
        .collect();

    info!("Found {} changed Rust files", rust_files.len());

    Ok(rust_files)
}

/// Get file content from a specific branch
pub(super) fn get_file_content(file: &str, branch: &str) -> Result<String> {
    debug!("Getting content for {} from {}", file, branch);

    let output = Command::new("git")
        .args(["show", &format!("{}:{}", branch, file)])
        .output()
        .context(format!("Failed to execute git show command for {}", file))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git show command failed: {}", stderr);
    }

    let content =
        String::from_utf8(output.stdout).context("Failed to parse file content as UTF-8")?;

    Ok(content)
}

/// Get file content from a branch, handling common errors
pub(super) fn get_branch_content(file: &str, branch: &str) -> String {
    match get_file_content(file, branch) {
        Ok(content) => content,
        Err(e) => {
            // Skip errors for files that might not exist in one branch
            if !e.to_string().contains("did not match any file") {
                log::warn!("Failed to get {} content from {}: {}", file, branch, e);
            }
            String::new()
        }
    }
}

/// Get all Rust files in the repository
pub(super) fn get_all_rust_files() -> Result<Vec<String>> {
    info!("Getting all Rust files in the repository...");

    // Use git ls-files to get all tracked Rust files
    let output = Command::new("git")
        .args(["ls-files", "*.rs"])
        .output()
        .context("Failed to execute git ls-files command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git ls-files command failed: {}", stderr);
    }

    let files = String::from_utf8(output.stdout).context("Failed to parse git ls-files output")?;

    let rust_files: Vec<String> = files.lines().map(|line| line.to_owned()).collect();

    info!("Found {} Rust files in total", rust_files.len());

    Ok(rust_files)
}
