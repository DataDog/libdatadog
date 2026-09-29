// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Enforces panic containment for newly added `pub extern "C" fn` and
//! `pub unsafe extern "C" fn` entry points, as required by AGENTS.md's
//! "Reliability & integrability" convention.
//!
//! The check is intentionally scoped to added lines in a branch diff. Existing
//! FFI functions include simple accessors that do not all need wrapping, so a
//! whole-repository check would create unrelated legacy failures. A new entry
//! point passes when its body uses `catch_unwind`, one of the common FFI result
//! wrappers, or a `_no_catch` wrapper. The latter counts as an explicit
//! acknowledgement that panic behavior was considered. A justified
//! `// allow(ffi-panic-boundary): <justification>` comment is also accepted.

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::Command;

mod lint;

use lint::{parse_diff, Violation};

#[derive(Parser)]
#[command(about = "Check new extern \"C\" functions for panic containment")]
struct Args {
    /// Git ref to diff against. When omitted in GitHub Actions, this defaults
    /// to `origin/$GITHUB_BASE_REF`.
    #[arg(long, value_name = "REF")]
    base_ref: Option<String>,

    /// Also emit GitHub Actions error annotations. On by default inside a
    /// GitHub Actions runner.
    #[arg(long, env = "GITHUB_ACTIONS")]
    annotate: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let base_ref = resolve_base_ref(args.base_ref)?;
    let root = repository_root()?;
    let diff = git_diff(&root, &base_ref)?;
    let changed_files = parse_diff(&diff)?;

    let mut violations = Vec::new();
    for changed_file in &changed_files {
        violations.extend(changed_file.lint(&root)?);
    }

    report(&violations, changed_files.len(), args.annotate);

    if violations.is_empty() {
        Ok(())
    } else {
        // The report above is the actionable output; a second error message
        // from anyhow would only add noise.
        std::process::exit(1);
    }
}

fn resolve_base_ref(explicit: Option<String>) -> Result<String> {
    if let Some(base_ref) = explicit.filter(|value| !value.trim().is_empty()) {
        return Ok(base_ref);
    }

    if let Some(base_ref) = std::env::var_os("GITHUB_BASE_REF")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
    {
        return Ok(format!("origin/{base_ref}"));
    }

    bail!("no base ref provided; pass --base-ref <ref> or run in a GitHub pull_request context")
}

fn repository_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        bail!(
            "failed to find repository root: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let root = String::from_utf8(output.stdout).context("repository path is not valid UTF-8")?;
    Ok(PathBuf::from(root.trim()))
}

fn git_diff(root: &Path, base_ref: &str) -> Result<String> {
    let comparison = format!("{base_ref}...HEAD");
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            "--diff-filter=ACMR",
            "--end-of-options",
            &comparison,
            "--",
            "*.rs",
        ])
        .output()
        .context("failed to run git diff")?;
    if !output.status.success() {
        bail!(
            "git diff failed against {base_ref}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    String::from_utf8(output.stdout).context("git diff output is not valid UTF-8")
}

fn report(violations: &[Violation], file_count: usize, annotate: bool) {
    if annotate {
        for violation in violations {
            // https://docs.github.com/actions/reference/workflows-and-actions/workflow-commands
            println!(
                "::error file={},line={},title=ffi-panic-lint::`{}` {} {}",
                violation.file,
                violation.line,
                violation.function,
                violation.problem,
                violation.hint
            );
        }
    }

    if violations.is_empty() {
        println!("ffi-panic-lint: {file_count} changed Rust file(s) checked, no violations found");
        return;
    }

    println!("ffi-panic-lint: {} violation(s)\n", violations.len());
    for violation in violations {
        println!("{violation}\n");
    }

    println!(
        "Panic containment for C FFI entry points is documented in AGENTS.md's \
         \"Reliability & integrability\" section."
    );
}
