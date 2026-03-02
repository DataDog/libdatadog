// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Detects which Rust crates have changed files in a PR or push.
//!
//! Reads env vars following the GitHub Actions INPUT_* convention and writes
//! to $GITHUB_OUTPUT.
//!
//! Outputs:
//!   crates        – JSON array of {name, version, path, manifest}
//!   crates_count  – integer count
//!   base_ref      – the base ref used for comparison

use anyhow::Result;
use ci_crates::git;
use ci_shared::crate_detection::{find_closest_cargo_toml, parse_crate_info, CrateInfo};
use ci_shared::github_output::set_output;
use std::collections::HashSet;
use std::path::PathBuf;

fn main() -> Result<()> {
    env_logger::init();

    let include_non_publishable = std::env::var("INPUT_INCLUDE_NON_PUBLISHABLE")
        .unwrap_or_default()
        .to_lowercase()
        == "true";

    let input_base_ref = std::env::var("INPUT_BASE_REF").unwrap_or_default();
    let event_name = std::env::var("GITHUB_EVENT_NAME").unwrap_or_default();
    let pr_base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_default();

    let base_ref = determine_base_ref(&input_base_ref, &event_name, &pr_base_ref)?;

    log::info!("Using base ref: {base_ref}");
    set_output("base_ref", &base_ref)?;

    let changed = git::changed_files(&base_ref)?;
    log::info!("Changed files: {:?}", changed);

    let crates = collect_changed_crates(&changed, include_non_publishable);

    let json = serde_json::to_string(&crates)?;
    log::info!("Changed crates: {json}");

    set_output("crates", &json)?;
    set_output("crates_count", &crates.len().to_string())?;

    Ok(())
}

fn determine_base_ref(
    input_base_ref: &str,
    event_name: &str,
    pr_base_ref: &str,
) -> Result<String> {
    if !input_base_ref.is_empty() {
        return Ok(input_base_ref.to_string());
    }

    if event_name == "pull_request" {
        let base = format!("origin/{pr_base_ref}");
        git::fetch_base(&base)?;
        return Ok(base);
    }

    Ok("HEAD~1".to_string())
}

pub fn collect_changed_crates(
    changed_files: &[String],
    include_non_publishable: bool,
) -> Vec<serde_json::Value> {
    let mut seen_manifests: HashSet<PathBuf> = HashSet::new();
    let mut crates: Vec<CrateInfo> = Vec::new();

    for file in changed_files {
        let path = std::path::Path::new(file);
        let Some(manifest) = find_closest_cargo_toml(path) else {
            continue;
        };

        if seen_manifests.contains(&manifest) {
            continue;
        }
        seen_manifests.insert(manifest.clone());

        match parse_crate_info(&manifest) {
            Ok(info) => {
                if include_non_publishable || info.publish {
                    crates.push(info);
                }
            }
            Err(e) => {
                log::warn!("Skipping {}: {e}", manifest.display());
            }
        }
    }

    crates
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "version": c.version,
                "path": c.path,
                "manifest": c.manifest,
            })
        })
        .collect()
}
