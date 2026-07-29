// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write;

fn github_output_path() -> Result<String> {
    std::env::var("GITHUB_OUTPUT").context("GITHUB_OUTPUT environment variable not set")
}

/// Append `key=value\n` to the file at `$GITHUB_OUTPUT`.
pub fn set_output(key: &str, value: &str) -> Result<()> {
    let path = github_output_path()?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open GITHUB_OUTPUT file at {path}"))?;
    writeln!(file, "{key}={value}")
        .with_context(|| format!("Failed to write to GITHUB_OUTPUT file at {path}"))?;
    Ok(())
}

/// Append a multiline value using the heredoc delimiter format required by GitHub Actions.
///
/// Format:
/// ```text
/// key<<_DELIMITER_
/// value line 1
/// value line 2
/// _DELIMITER_
/// ```
pub fn set_multiline_output(key: &str, value: &str) -> Result<()> {
    let path = github_output_path()?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open GITHUB_OUTPUT file at {path}"))?;
    writeln!(file, "{key}<<_DELIMITER_")?;
    writeln!(file, "{value}")?;
    writeln!(file, "_DELIMITER_")?;
    Ok(())
}
