// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Computes all crates affected (direct + transitive) by changes in a PR or push.
//!
//! Reads env vars following the GitHub Actions INPUT_* convention and writes
//! to $GITHUB_OUTPUT.
//!
//! Outputs:
//!   affected_crates        – JSON array of {name, version, path, manifest}
//!   affected_crates_count  – integer
//!   has_changes            – "true" if any crates are affected

use anyhow::Result;
use ci_crates::git;
use ci_crates::workspace;
use ci_shared::crate_detection::{find_closest_cargo_toml, parse_crate_info, CrateInfo};
use ci_shared::github_output::set_output;
use std::collections::HashSet;
use std::path::PathBuf;

const BASE_DEFAULT: &str = "origin/main";

fn main() -> Result<()> {
    env_logger::init();

    let include_non_publishable = std::env::var("INPUT_INCLUDE_NON_PUBLISHABLE")
        .unwrap_or_default()
        .to_lowercase()
        == "true";

    let base_ref = std::env::var("INPUT_BASE_REF").unwrap_or(BASE_DEFAULT.to_string());
    log::info!("Using base ref: {base_ref}");

    let changed_files = git::changed_files(&base_ref)?;
    log::info!("Changed files: {:?}", changed_files);

    // TODO: Check heuristics when workspace manifest (Cargo.toml) or config.toml changed. This could indicate a
    // change in:
    // * Rust version.
    // * Edition.
    // * Profile.
    // * Compilation flags.

    let meta = workspace::load()?;
    let ws_names: HashSet<String> = meta.members().iter().map(|p| p.name.clone()).collect();

    let direct_crates = collect_changed_crates(&changed_files, include_non_publishable, &ws_names);

    if direct_crates.is_empty() {
        return build_output(None)
    }

    // Compute transitive affected set
    let direct_names: Vec<String> = direct_crates.iter().map(|c| c.name.clone()).collect();
    log::info!("Directly changed crates: {:?}", direct_names);
    let affected_names = meta.affected_from(&direct_names);
    log::info!("Affected crates (transitive): {:?}", affected_names);

    // Build affected crate info list
    let affected_infos: Vec<&CrateInfo> = direct_crates
        .iter()
        .filter(|c| affected_names.contains(&c.name))
        .collect();

    // Build CrateInfo for transitively-added crates (not in direct_crates)
    let direct_names_set: HashSet<&String> = direct_names.iter().collect();
    let transitive_only: Vec<String> = affected_names
        .iter()
        .filter(|n| !direct_names_set.contains(n))
        .cloned()
        .collect();

    // Look up transitive-only crates from workspace metadata
    let mut all_infos: Vec<serde_json::Value> = affected_infos
        .iter()
        .map(|c| crate_info_to_json(c))
        .collect();

    for pkg in meta.members() {
        if transitive_only.contains(&pkg.name) {
            // pkg.manifest_path is the path to Cargo.toml
            let manifest = PathBuf::from(&pkg.manifest_path);
            if let Ok(info) = parse_crate_info(&manifest) {
                all_infos.push(crate_info_to_json(&info));
            }
        }
    }


    build_output(Some(all_infos))
}

fn collect_changed_crates(
    changed_files: &[String],
    include_non_publishable: bool,
    ws_names: &HashSet<String>,
) -> Vec<CrateInfo> {
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
                if !ws_names.contains(&info.name) {
                    log::debug!("Skipping {} (not in main workspace)", info.name);
                    continue;
                }
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
}

fn crate_info_to_json(c: &CrateInfo) -> serde_json::Value {
    serde_json::json!({
        "name": c.name,
        "version": c.version,
        "path": c.path,
        "manifest": c.manifest,
    })
}

fn build_output(affected_crates: Option<Vec<serde_json::Value>>) -> Result<()> {
    
    let (crates, len, has_changes): (String, String, String) = if let Some(crates) = affected_crates {
        (serde_json::to_string(&crates).unwrap_or("[]".to_string()), crates.len().to_string(), "true".to_string())
    } else {
        ("[]".to_string(), "0".to_string(), "false".to_string())
    };

    if std::env::var("DEBUG").is_ok() {
        log::info!("affected_crates: {:?}", crates);
        log::info!("len: {:?}", len);
        log::info!("has_changes: {:?}", len);
    } else {
            set_output("affected_crates", &crates)?;
            set_output("affected_crates_count", &len)?;
            set_output("has_changes", &has_changes)?;
    }
    Ok(())
}
