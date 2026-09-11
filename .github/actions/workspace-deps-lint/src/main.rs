// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Enforces how dependencies are declared across the libdatadog workspace.
//!
//! Rules:
//!
//! 1. Every external dependency of a workspace member is declared in the root
//!    `[workspace.dependencies]` and inherited with `workspace = true`.
//! 2. Every `[workspace.dependencies]` entry has all features (including default) disabled. Feature
//!    selection belongs to the members that actually need them.
//!
//! Rule 1 accepts an exception when the dependency carries a `# allow(workspace-deps):
//! <justification>` comment on the line(s) directly above it.
//!
//! Rule 2 accepts an exception when the workspace entry carries at least one comment line directly
//! above it explaining why the feature is enabled for every member (the explicit marker works too).
//!
//! Path dependencies are crates of this repository, not external dependencies, so they are ignored.

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use clap::Parser;
use std::path::{Path, PathBuf};

mod lint;

use lint::{lint_member, lint_workspace_dependencies, Violation};

#[derive(Parser)]
#[command(
    about = "Check that workspace members inherit their external dependencies from [workspace.dependencies]"
)]
struct Args {
    /// Root manifest of the workspace to lint. Defaults to the outermost
    /// workspace manifest above the current directory, so it also works from
    /// inside `.github/actions`.
    #[arg(long)]
    manifest_path: Option<PathBuf>,

    /// Also emit GitHub Actions error annotations. On by default inside a
    /// GitHub Actions runner.
    #[arg(long, env = "GITHUB_ACTIONS")]
    annotate: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let manifest_path = match args.manifest_path {
        Some(path) => path,
        None => outermost_workspace_manifest(&std::env::current_dir()?)?,
    };

    let metadata = MetadataCommand::new()
        .manifest_path(&manifest_path)
        .no_deps()
        .exec()
        .with_context(|| {
            format!(
                "failed to read cargo metadata for {}",
                manifest_path.display()
            )
        })?;
    let root = PathBuf::from(metadata.workspace_root.as_std_path());

    let mut violations =
        read_and_lint(&root, &root.join("Cargo.toml"), lint_workspace_dependencies)?;
    let mut manifests: Vec<PathBuf> = metadata
        .packages
        .iter()
        .map(|package| PathBuf::from(package.manifest_path.as_std_path()))
        .collect();

    manifests.sort();

    for manifest in &manifests {
        violations.extend(read_and_lint(&root, manifest, lint_member)?);
    }

    report(&violations, manifests.len(), args.annotate);

    if violations.is_empty() {
        Ok(())
    } else {
        // The report above is the actionable output; a second error message
        // from anyhow would only add noise.
        std::process::exit(1);
    }
}

fn read_and_lint(
    root: &Path,
    manifest: &Path,
    lint: impl Fn(&str, &str) -> Result<Vec<Violation>>,
) -> Result<Vec<Violation>> {
    let source = std::fs::read_to_string(manifest)
        .with_context(|| format!("failed to read {}", manifest.display()))?;
    let display = manifest
        .strip_prefix(root)
        .unwrap_or(manifest)
        .to_string_lossy()
        .replace('\\', "/");
    lint(&display, &source)
}

/// Walk up from `start` and return the manifest of the outermost workspace.
fn outermost_workspace_manifest(start: &Path) -> Result<PathBuf> {
    let mut found = None;
    for directory in start.ancestors() {
        let manifest = directory.join("Cargo.toml");
        let Ok(source) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if source
            .parse::<toml_edit::DocumentMut>()
            .is_ok_and(|document| document.contains_key("workspace"))
        {
            found = Some(manifest);
        }
    }

    found.with_context(|| {
        format!(
            "no workspace manifest found above {}, pass --manifest-path",
            start.display()
        )
    })
}

fn report(violations: &[Violation], manifest_count: usize, annotate: bool) {
    if annotate {
        for violation in violations {
            // https://docs.github.com/actions/reference/workflows-and-actions/workflow-commands
            println!(
                "::error file={},line={},title=workspace-deps::{} {}",
                violation.file,
                violation.line,
                format_args!("`{}` {}", violation.dep, violation.problem),
                violation.hint
            );
        }
    }

    if violations.is_empty() {
        println!("workspace-deps-lint: {manifest_count} manifest(s) checked, no violation found");
        return;
    }

    println!("workspace-deps-lint: {} violation(s)\n", violations.len());

    for violation in violations {
        println!("{violation}\n");
    }

    println!(
        "The dependency declaration policy is documented in AGENTS.md and in the \
         [workspace.dependencies] header of the root Cargo.toml."
    );
}
