// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Computes all crates affected (direct + transitive) by changes in a PR or push.
//!
//! Reads env vars following the GitHub Actions INPUT_* convention and writes
//! to $GITHUB_OUTPUT.
//!
//! Outputs:
//!   changed_crates         - JSON array
//!   affected_crates        – JSON array of {name, version, path, manifest}
//!   affected_crates_count  – integer

use anyhow::{anyhow, Result};
use cargo_metadata::Package;
use ci_crates::git;
use ci_crates::workspace;
use ci_shared::crate_detection::CrateInfo;
use ci_shared::github_output::set_output;
use std::collections::HashSet;

fn main() -> Result<()> {
    env_logger::init();
    // Parse args
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return Err(anyhow!("A reference base needs to be passed"))
    }

    let base_ref = &args[1];
    
    let changed_files = git::changed_files(&base_ref)?;
    log::info!("Changed files: {:?}", changed_files);

    // TODO: Check heuristics when workspace manifest (Cargo.toml) or config.toml changed. This could indicate a
    // change in:
    // * Rust version.
    // * Edition.
    // * Profile.
    // * Compilation flags.

    let workspace = workspace::load()?;
    let changed_crates = collect_changed_crates(&changed_files, workspace.members());


    if changed_crates.is_empty() {
        log::info!("Changed crates is empty");
        return build_output(None, None, base_ref)
    }

    let seeds: Vec<String> = changed_crates.iter().map(|c| c.name.clone()).collect();

    let result = workspace.affected_from(&seeds);

    build_output(Some(changed_crates), Some(result.into_iter().collect()), base_ref)
}

fn collect_changed_crates(
    changed_files: &[String],
    members: &[Package],
) -> Vec<CrateInfo> {
    let mut crates: Vec<CrateInfo> = Vec::new();
    let mut crate_inventory: HashSet<String> = HashSet::new();

    for file in changed_files {
        for member in members {
            if file.contains(member.name.as_str()) {
                if crate_inventory.insert(member.name.to_string()) {
                    crates.push(CrateInfo { 
                        name: member.name.as_str().to_string(),
                        version: format!("{}.{}.{}", member.version.major, member.version.minor, member.version.patch),
                        manifest: member.manifest_path.clone().into(),
                        path: member.manifest_path.parent().unwrap().into(),
                        publish: if let Some(publishable) = &member.publish {
                            !publishable.is_empty()
                        } else {
                            true
                        }
                    });
                }
            }
        }
    }

    crates
}

fn build_output(changed_crates: Option<Vec<CrateInfo>>, affected_crates: Option<Vec<String>>, base_ref: &str) -> Result<()> {
    
    let (changed, changed_len): (String, String) = if let Some(crates) = changed_crates {
        (serde_json::to_string(&crates).unwrap_or("[]".to_string()), crates.len().to_string())
    } else {
        ("[]".to_string(), "0".to_string())
    };

    let (affected, affected_len): (String, String) = if let Some(crates) = affected_crates {
        (serde_json::to_string(&crates).unwrap_or("[]".to_string()), crates.len().to_string())
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
        set_output("crates", &changed)?;
        set_output("crates_count", &changed_len)?;
        set_output("affected_crates", &affected)?;
        set_output("affected_crates_count", &affected_len)?;
        set_output("base_ref", base_ref)?;
    }
    Ok(())
}
