// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Validate .github/CODEOWNERS against the repository.
//!
//! Checks, all local and fast:
//!
//! - every rule matches at least one tracked file (a pattern that matches nothing is dead weight
//!   and usually a typo)
//! - every tracked file resolves to at least one owner
//! - no pattern appears twice (GitHub applies last-match-wins silently, so the earlier rule would
//!   be dead weight)
//!
//! Owner validity (handles exist and have write access) is GitHub's own
//! validation and runs in CI through the codeowners/errors API; it cannot be
//! checked from here.

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use tools::codeowners;

#[derive(Debug, Parser)]
#[command(
    name = "validate_codeowners",
    about = "Validate .github/CODEOWNERS against the repository"
)]
struct Args {
    /// CODEOWNERS file to validate, repository-relative (default: .github/CODEOWNERS)
    path: Option<String>,
}

fn run(relative: Option<&str>) -> Result<()> {
    // tools/ sits at the repository root
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR is the tools/ directory, which has a parent");
    let path = match relative {
        Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
        Some(p) => root.join(p),
        None => root.join(".github/CODEOWNERS"),
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let rules = codeowners::parse(&text)?;
    let files = tracked_files(root)?;

    let mut problems: Vec<String> = Vec::new();

    let mut seen: HashMap<&str, usize> = HashMap::new();
    for rule in &rules {
        match seen.get(rule.pattern.as_str()) {
            Some(first) => problems.push(format!(
                "line {}: pattern {:?} duplicates line {} (last match wins, the earlier rule is dead)",
                rule.line, rule.pattern, first
            )),
            None => {
                seen.insert(rule.pattern.as_str(), rule.line);
            }
        }
    }

    for rule in &rules {
        if !files.iter().any(|f| codeowners::matches(&rule.pattern, f)) {
            problems.push(format!(
                "line {}: {:?} matches no tracked file",
                rule.line, rule.pattern
            ));
        }
    }

    let unowned: Vec<&String> = files
        .iter()
        .filter(|f| codeowners::resolve(f, &rules).is_none())
        .collect();
    if !unowned.is_empty() {
        problems.push(format!(
            "{} tracked file(s) have no owner, e.g. {}",
            unowned.len(),
            unowned.first().map(|f| f.as_str()).unwrap_or_default()
        ));
    }

    if !problems.is_empty() {
        for p in &problems {
            eprintln!("validate_codeowners: {p}");
        }
        bail!(
            "{} problem(s) in {} ({} rules, {} tracked files)",
            problems.len(),
            path.display(),
            rules.len(),
            files.len()
        );
    }
    println!(
        "ok: {} rules cover all {} tracked files",
        rules.len(),
        files.len()
    );
    Ok(())
}

fn tracked_files(root: &Path) -> Result<Vec<String>> {
    // -z: NUL-delimited and unquoted (with the default core.quotePath, git
    // C-quotes non-ASCII names, which would not match any pattern)
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("running git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed: {}", out.status);
    }
    let stdout = String::from_utf8(out.stdout).context("git ls-files output is not UTF-8")?;
    Ok(stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args.path.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("validate_codeowners: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // tracked_files() spawns `git ls-files`; Miri does not emulate subprocess
    // spawning, so the end-to-end check only runs under cargo test/nextest.
    #[cfg(not(miri))]
    fn current_codeowners_validates() {
        run(None).expect("the committed .github/CODEOWNERS must validate");
    }
}
