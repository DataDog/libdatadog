// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use log::error;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

/// Get crate information for a given file path by finding the closest Cargo.toml
pub(super) fn get_crate_for_file(file_path: &str) -> String {
    let path = Path::new(file_path);

    // Try to find the closest Cargo.toml file
    if let Some(cargo_toml_path) = find_closest_cargo_toml(path) {
        if let Some(crate_name) = extract_package_name(&cargo_toml_path) {
            return crate_name;
        }
    }

    error!(
        "Could not find crate for {}, falling back to unknown-crate",
        file_path
    );
    "unknown-crate".to_owned()
}

/// Find the closest Cargo.toml file by traversing up the directory tree
fn find_closest_cargo_toml(mut path: &Path) -> Option<PathBuf> {
    // Start with the directory containing the file
    if !path.is_dir() {
        path = path.parent()?;
    }

    // Traverse up the directory tree
    loop {
        let cargo_path = path.join("Cargo.toml");
        if cargo_path.exists() {
            return Some(cargo_path);
        }

        // Check if we've reached the root
        let parent = path.parent()?;
        if parent == path {
            // We've reached the root without finding Cargo.toml
            return None;
        }

        // Move up one directory
        path = parent;
    }
}

/// Extract package name from Cargo.toml
fn extract_package_name(cargo_toml_path: &Path) -> Option<String> {
    // Read the Cargo.toml file
    let content = match fs::read_to_string(cargo_toml_path) {
        Ok(content) => content,
        Err(e) => {
            log::warn!("Failed to read {}: {}", cargo_toml_path.display(), e);
            return None;
        }
    };

    // Parse the TOML
    let toml_value: Value = match content.parse() {
        Ok(value) => value,
        Err(e) => {
            log::warn!("Failed to parse {}: {}", cargo_toml_path.display(), e);
            return None;
        }
    };

    // Extract the package name
    toml_value
        .get("package")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}
