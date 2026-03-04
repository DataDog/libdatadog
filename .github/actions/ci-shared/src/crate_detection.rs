// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

/// Information about a Rust crate parsed from its Cargo.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateInfo {
    pub name: String,
    /// "workspace" if version.workspace = true, otherwise the literal version string
    pub version: String,
    /// Directory containing Cargo.toml
    pub path: PathBuf,
    /// Full path to Cargo.toml
    pub manifest: PathBuf,
    /// false only if publish = false explicitly
    pub publish: bool,
}

/// Find the closest Cargo.toml file by traversing up the directory tree.
/// Returns None if no Cargo.toml is found before the filesystem root.
pub fn find_closest_cargo_toml(mut path: &Path) -> Option<PathBuf> {
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
            return None;
        }

        path = parent;
    }
}

/// Parse full crate information from a Cargo.toml manifest path.
pub fn parse_crate_info(manifest: &Path) -> Result<CrateInfo> {
    let content = fs::read_to_string(manifest)
        .with_context(|| format!("Failed to read {}", manifest.display()))?;

    let toml_value: Value = content
        .parse()
        .with_context(|| format!("Failed to parse {}", manifest.display()))?;

    let package = toml_value
        .get("package")
        .with_context(|| format!("No [package] section in {}", manifest.display()))?;

    let name = package
        .get("name")
        .and_then(|v| v.as_str())
        .with_context(|| format!("No package.name in {}", manifest.display()))?
        .to_string();

    // version.workspace = true → use "workspace" as the version string
    let version = if let Some(ver_table) = package.get("version").and_then(|v| v.as_table()) {
        if ver_table.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
            "workspace".to_string()
        } else {
            "workspace".to_string() // unusual but treat as workspace
        }
    } else {
        package
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("workspace")
            .to_string()
    };

    // publish = false means not publishable; anything else (missing, true, list) means publishable
    let publish = match package.get("publish") {
        Some(Value::Boolean(false)) => false,
        _ => true,
    };

    let path = manifest
        .parent()
        .with_context(|| format!("Manifest {} has no parent directory", manifest.display()))?
        .to_path_buf();

    Ok(CrateInfo {
        name,
        version,
        path,
        manifest: manifest.to_path_buf(),
        publish,
    })
}

/// Get crate name for a given file path by finding the closest Cargo.toml.
/// Returns "unknown-crate" if no crate can be determined.
pub fn get_crate_for_file(file_path: &str) -> String {
    let path = Path::new(file_path);

    if let Some(cargo_toml_path) = find_closest_cargo_toml(path) {
        if let Ok(info) = parse_crate_info(&cargo_toml_path) {
            return info.name;
        }
    }

    log::error!(
        "Could not find crate for {}, falling back to unknown-crate",
        file_path
    );
    "unknown-crate".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    fn create_cargo_toml(path: &Path, package_name: &str) -> std::io::Result<()> {
        let mut file = File::create(path)?;
        writeln!(
            file,
            r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"
"#,
            package_name
        )?;
        Ok(())
    }

    #[test]
    fn test_get_crate_for_file_direct_parent() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_root = temp_dir.path().join("my-crate");
        fs::create_dir_all(&crate_root).expect("Failed to create crate directory");

        let cargo_toml_path = crate_root.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "my-awesome-crate")
            .expect("Failed to create Cargo.toml");

        let source_file_path = crate_root.join("lib.rs");
        File::create(&source_file_path).expect("Failed to create source file");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "my-awesome-crate",
            "Should identify the correct crate name from direct parent"
        );
    }

    #[test]
    fn test_get_crate_for_file_nested_directory() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_root = temp_dir.path().join("my-crate");
        let src_dir = crate_root.join("src");
        let module_dir = src_dir.join("module");
        fs::create_dir_all(&module_dir).expect("Failed to create nested directories");

        let cargo_toml_path = crate_root.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "nested-crate").expect("Failed to create Cargo.toml");

        let source_file_path = module_dir.join("mod.rs");
        File::create(&source_file_path).expect("Failed to create source file");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "nested-crate",
            "Should identify the correct crate name from nested directory"
        );
    }

    #[test]
    fn test_get_crate_for_file_multiple_cargo_tomls() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let workspace_dir = temp_dir.path().join("workspace");
        let parent_crate_dir = workspace_dir.join("parent-crate");
        let child_crate_dir = parent_crate_dir.join("child-crate");
        let child_src_dir = child_crate_dir.join("src");

        fs::create_dir_all(&parent_crate_dir).expect("Failed to create parent crate directory");
        fs::create_dir_all(&child_src_dir).expect("Failed to create child crate src directory");

        let parent_cargo_toml = parent_crate_dir.join("Cargo.toml");
        create_cargo_toml(&parent_cargo_toml, "parent-crate")
            .expect("Failed to create parent Cargo.toml");

        let child_cargo_toml = child_crate_dir.join("Cargo.toml");
        create_cargo_toml(&child_cargo_toml, "child-crate")
            .expect("Failed to create child Cargo.toml");

        let source_file_path = child_src_dir.join("lib.rs");
        File::create(&source_file_path).expect("Failed to create source file");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "child-crate",
            "Should identify the closest crate"
        );
    }

    #[test]
    fn test_get_crate_for_file_no_cargo_toml() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir).expect("Failed to create src directory");

        let source_file_path = src_dir.join("orphan.rs");
        File::create(&source_file_path).expect("Failed to create source file");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "unknown-crate",
            "Should return unknown-crate when no Cargo.toml is found"
        );
    }

    #[test]
    fn test_get_crate_for_file_invalid_cargo_toml() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("invalid-crate");
        fs::create_dir_all(&crate_dir).expect("Failed to create crate directory");

        let cargo_toml_path = crate_dir.join("Cargo.toml");
        let mut file = File::create(&cargo_toml_path).expect("Failed to create Cargo.toml");
        writeln!(
            file,
            r#"
[package]
version = "0.1.0"
edition = "2021"
"#
        )
        .expect("Failed to write to Cargo.toml");

        let source_file_path = crate_dir.join("lib.rs");
        File::create(&source_file_path).expect("Failed to create source file");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "unknown-crate",
            "Should return unknown-crate when Cargo.toml is invalid"
        );
    }

    #[test]
    fn test_get_crate_for_file_workspace_member() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let workspace_dir = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir).expect("Failed to create workspace directory");

        let workspace_cargo_toml = workspace_dir.join("Cargo.toml");
        let mut file =
            File::create(&workspace_cargo_toml).expect("Failed to create workspace Cargo.toml");
        writeln!(
            file,
            r#"
[workspace]
members = ["member1", "member2"]
"#
        )
        .expect("Failed to write to workspace Cargo.toml");

        let member1_dir = workspace_dir.join("member1");
        let member1_src_dir = member1_dir.join("src");
        fs::create_dir_all(&member1_src_dir).expect("Failed to create member1 src directory");

        let member1_cargo_toml = member1_dir.join("Cargo.toml");
        create_cargo_toml(&member1_cargo_toml, "workspace-member1")
            .expect("Failed to create member1 Cargo.toml");

        let source_file_path = member1_src_dir.join("lib.rs");
        File::create(&source_file_path).expect("Failed to create source file");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "workspace-member1",
            "Should identify the workspace member crate"
        );
    }

    #[test]
    fn test_get_crate_for_file_non_existent_file() {
        let crate_name = get_crate_for_file("/path/to/non/existent/file.rs");

        assert_eq!(
            crate_name, "unknown-crate",
            "Should return unknown-crate for non-existent file"
        );
    }

    #[test]
    fn test_get_crate_for_file_with_special_chars() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("my crate with spaces");
        fs::create_dir_all(&crate_dir).expect("Failed to create crate directory with spaces");

        let cargo_toml_path = crate_dir.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "special-chars-crate")
            .expect("Failed to create Cargo.toml");

        let source_file_path = crate_dir.join("special file.rs");
        File::create(&source_file_path).expect("Failed to create source file with spaces");

        let crate_name = get_crate_for_file(&source_file_path.to_string_lossy());

        assert_eq!(
            crate_name, "special-chars-crate",
            "Should handle paths with spaces correctly"
        );
    }

    #[test]
    fn test_get_crate_for_file_with_directory_not_file() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("directory-test");
        let src_dir = crate_dir.join("src");
        fs::create_dir_all(&src_dir).expect("Failed to create directories");

        let cargo_toml_path = crate_dir.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "directory-crate")
            .expect("Failed to create Cargo.toml");

        let crate_name = get_crate_for_file(&src_dir.to_string_lossy());

        assert_eq!(
            crate_name, "directory-crate",
            "Should work with directory paths"
        );
    }

    #[test]
    fn test_parse_crate_info_publish_false() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("my-crate");
        fs::create_dir_all(&crate_dir).expect("Failed to create crate directory");

        let cargo_toml_path = crate_dir.join("Cargo.toml");
        let mut file = File::create(&cargo_toml_path).expect("Failed to create Cargo.toml");
        writeln!(
            file,
            r#"[package]
name = "my-crate"
version = "1.2.3"
edition = "2021"
publish = false
"#
        )
        .expect("Failed to write Cargo.toml");

        let info = parse_crate_info(&cargo_toml_path).expect("parse should succeed");
        assert_eq!(info.name, "my-crate");
        assert_eq!(info.version, "1.2.3");
        assert!(!info.publish);
    }

    #[test]
    fn test_parse_crate_info_workspace_version() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("ws-crate");
        fs::create_dir_all(&crate_dir).expect("Failed to create crate directory");

        let cargo_toml_path = crate_dir.join("Cargo.toml");
        let mut file = File::create(&cargo_toml_path).expect("Failed to create Cargo.toml");
        writeln!(
            file,
            r#"[package]
name = "ws-crate"
version.workspace = true
edition = "2021"
"#
        )
        .expect("Failed to write Cargo.toml");

        let info = parse_crate_info(&cargo_toml_path).expect("parse should succeed");
        assert_eq!(info.name, "ws-crate");
        assert_eq!(info.version, "workspace");
        assert!(info.publish);
    }
}
