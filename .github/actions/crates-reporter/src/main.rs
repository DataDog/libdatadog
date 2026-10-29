// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Computes all crates affected (direct + transitive) by changes in a PR,
//! or by the combined PR + rebase delta of a Devflow merge-queue push.
//!
//! Reads env vars following the GitHub Actions INPUT_* convention and writes
//! to $GITHUB_OUTPUT.
//!
//! Outputs:
//!   changed_crates         - JSON array
//!   affected_crates        – JSON array of {name, version, path, manifest}
//!   affected_crates_count  – integer

use anyhow::{Context, Result};
use cargo_metadata::Package;
use cargo_metadata::camino::Utf8Path;
use ci_shared::crate_detection::CrateInfo;
use ci_shared::git;
use ci_shared::github_output::set_output;
use ci_shared::workspace;
use clap::Parser;
use std::collections::HashSet;

#[derive(Parser)]
#[command(about = "Computes all crates affected by a PR, or by a Devflow merge-queue push")]
struct Args {
    /// Base reference to compare against (e.g. origin/main)
    base_ref: Option<String>,

    /// Run in merge-queue mode: derive the scope from the mq ref geometry
    /// instead of a base ref. Requires HEAD to be a Devflow mq merge commit.
    #[arg(long)]
    mq_ref: bool,
}

/// A push to a Devflow merge-queue ref (`mq-working-branch-*`) points at a
/// merge commit whose first parent is an empty commit on main and whose
/// second parent is the PR head. The merge-base of the two parents is where
/// the PR branched off main, so the tree diff `merge-base..HEAD` covers both
/// the PR's own changes and everything main gained since the PR branched —
/// exactly what the queue re-run has to re-test.
fn mq_merge_base() -> Result<String> {
    let main_side = git::rev_parse("HEAD^1").context("not a Devflow mq ref (no first parent)")?;
    let pr_head = git::rev_parse("HEAD^2").context("not a Devflow mq ref (no second parent)")?;
    git::merge_base(&main_side, &pr_head)
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let (range, base_ref) = if args.mq_ref {
        let base = mq_merge_base()?;
        let range = format!("{base}..HEAD");
        log::info!("mq diff range: {range}");
        (range, base)
    } else {
        let base_ref = args
            .base_ref
            .context("a base ref is required unless --mq-ref is given")?;
        let base_ref = base_ref
            .strip_prefix("origin/")
            .unwrap_or(&base_ref)
            .to_string();

        git::fetch_base(&base_ref)?;
        (format!("origin/{base_ref}...HEAD"), base_ref)
    };

    let changed_files = git::changed_files_in_range(&range)?;
    log::info!("Changed files: {:?}", changed_files);

    let workspace = workspace::load()?;
    let changed_crates = collect_changed_crates(
        &changed_files,
        workspace.members(),
        workspace.workspace_root(),
    );

    if changed_crates.is_empty() {
        log::info!("Changed crates is empty");
        if git::has_workflow_changes_in_range(&range)? {
            log::info!("Workflow files changed, running all tests");
            return set_output("status", "skipped");
        }
        return build_output(None, None, &base_ref);
    }

    let seeds: Vec<String> = changed_crates.iter().map(|c| c.name.clone()).collect();

    let result = workspace.affected_from(&seeds);

    build_output(
        Some(changed_crates),
        Some(result.into_iter().collect()),
        &base_ref,
    )
}

fn collect_changed_crates(
    changed_files: &[String],
    members: &[Package],
    workspace_root: &Utf8Path,
) -> Vec<CrateInfo> {
    let mut crates: Vec<CrateInfo> = Vec::new();
    let mut crate_inventory: HashSet<String> = HashSet::new();

    for file in changed_files {
        for member in members {
            let Some(crate_dir) = member.manifest_path.parent() else {
                continue;
            };
            let Ok(relative_dir) = crate_dir.strip_prefix(workspace_root) else {
                continue;
            };
            let relative_dir_str = relative_dir.as_str();
            if (file == relative_dir_str || file.starts_with(&format!("{relative_dir_str}/")))
                && crate_inventory.insert(member.name.to_string())
            {
                crates.push(CrateInfo {
                    name: member.name.to_string(),
                    version: member.version.to_string(),
                    manifest: member.manifest_path.clone().into(),
                    path: member.manifest_path.parent().unwrap().into(),
                    publish: if let Some(publishable) = &member.publish {
                        !publishable.is_empty()
                    } else {
                        true
                    },
                });
            }
        }
    }

    crates
}

fn build_output(
    changed_crates: Option<Vec<CrateInfo>>,
    affected_crates: Option<Vec<String>>,
    base_ref: &str,
) -> Result<()> {
    let (changed, changed_len): (String, String) = if let Some(crates) = changed_crates {
        (
            serde_json::to_string(&crates).unwrap_or("[]".to_string()),
            crates.len().to_string(),
        )
    } else {
        ("[]".to_string(), "0".to_string())
    };

    let (affected, affected_len): (String, String) = if let Some(crates) = affected_crates {
        (
            serde_json::to_string(&crates).unwrap_or("[]".to_string()),
            crates.len().to_string(),
        )
    } else {
        ("[]".to_string(), "0".to_string())
    };

    if std::env::var("DEBUG").is_ok() {
        log::info!("crates: {:?}", changed);
        log::info!("crates_count: {:?}", changed_len);
        log::info!("affected_crates: {:?}", affected);
        log::info!("affected_crates_count: {:?}", affected_len);
        log::info!("base_ref: {:?}", base_ref);
    } else {
        set_output("status", "success")?;
        set_output("crates", &changed)?;
        set_output("crates_count", &changed_len)?;
        set_output("affected_crates", &affected)?;
        set_output("affected_crates_count", &affected_len)?;
        set_output("base_ref", base_ref)?;
    }
    Ok(())
}
