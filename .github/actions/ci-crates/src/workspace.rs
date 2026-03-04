// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;

/// A workspace member as returned by `cargo metadata`.
#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub manifest_path: String,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
pub struct Dependency {
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

/// Parsed workspace metadata with reverse-dependency index.
pub struct WorkspaceMetadata {
    packages: Vec<Package>,
    reverse_deps: HashMap<String, Vec<String>>,
}

pub fn load() -> Result<WorkspaceMetadata> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .context("Failed to run cargo metadata")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cargo metadata failed: {stderr}");
    }

    let meta: CargoMetadata =
        serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata output")?;

    let ws_member_ids: HashSet<&str> = meta.workspace_members.iter().map(|s| s.as_str()).collect();

    let ws_packages: Vec<Package> = meta
        .packages
        .into_iter()
        .filter(|p| {
            ws_member_ids
                .iter()
                .any(|id| id.starts_with(&format!("{} ", p.name)))
        })
        .collect();

    let ws_names: HashSet<String> = ws_packages.iter().map(|p| p.name.clone()).collect();
    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in &ws_packages {
        for dep in &pkg.dependencies {
            if ws_names.contains(&dep.name) {
                reverse_deps
                    .entry(dep.name.clone())
                    .or_default()
                    .push(pkg.name.clone());
            }
        }
    }

    Ok(WorkspaceMetadata {
        packages: ws_packages,
        reverse_deps,
    })
}

impl WorkspaceMetadata {
    pub fn members(&self) -> &[Package] {
        &self.packages
    }

    pub fn affected_from(&self, seeds: &[String]) -> Vec<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        for seed in seeds {
            if !visited.contains(seed) {
                visited.insert(seed.clone());
                queue.push_back(seed.clone());
            }
        }

        while let Some(current) = queue.pop_front() {
            if let Some(dependents) = self.reverse_deps.get(&current) {
                for dep in dependents {
                    if !visited.contains(dep) {
                        visited.insert(dep.clone());
                        queue.push_back(dep.clone());
                    }
                }
            }
        }

        let mut result: Vec<String> = visited.into_iter().collect();
        result.sort();
        result
    }
}
