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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    // Helper function to create a Cargo.toml file with a specific package name
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
        // Create a temporary directory with a specific crate structure
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_root = temp_dir.path().join("my-crate");
        fs::create_dir_all(&crate_root).expect("Failed to create crate directory");

        // Create Cargo.toml in the crate root
        let cargo_toml_path = crate_root.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "my-awesome-crate")
            .expect("Failed to create Cargo.toml");

        // Create a source file in the same directory
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
        // Create a temporary directory with a nested structure
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_root = temp_dir.path().join("my-crate");
        let src_dir = crate_root.join("src");
        let module_dir = src_dir.join("module");
        fs::create_dir_all(&module_dir).expect("Failed to create nested directories");

        // Create Cargo.toml in the crate root
        let cargo_toml_path = crate_root.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "nested-crate").expect("Failed to create Cargo.toml");

        // Create a source file in the nested directory
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
        // Create a temporary directory with nested crates
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let workspace_dir = temp_dir.path().join("workspace");
        let parent_crate_dir = workspace_dir.join("parent-crate");
        let child_crate_dir = parent_crate_dir.join("child-crate");
        let child_src_dir = child_crate_dir.join("src");

        fs::create_dir_all(&parent_crate_dir).expect("Failed to create parent crate directory");
        fs::create_dir_all(&child_src_dir).expect("Failed to create child crate src directory");

        // Create Cargo.toml in both crate directories
        let parent_cargo_toml = parent_crate_dir.join("Cargo.toml");
        create_cargo_toml(&parent_cargo_toml, "parent-crate")
            .expect("Failed to create parent Cargo.toml");

        let child_cargo_toml = child_crate_dir.join("Cargo.toml");
        create_cargo_toml(&child_cargo_toml, "child-crate")
            .expect("Failed to create child Cargo.toml");

        // Create a source file in the child crate
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
        // Create a temporary directory with no Cargo.toml
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir).expect("Failed to create src directory");

        // Create a source file
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
        // Create a temporary directory with an invalid Cargo.toml
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("invalid-crate");
        fs::create_dir_all(&crate_dir).expect("Failed to create crate directory");

        // Create an invalid Cargo.toml (missing package name)
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

        // Create a source file
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
        // Create a temporary directory with a workspace structure
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let workspace_dir = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir).expect("Failed to create workspace directory");

        // Create workspace Cargo.toml
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

        // Create member1 crate
        let member1_dir = workspace_dir.join("member1");
        let member1_src_dir = member1_dir.join("src");
        fs::create_dir_all(&member1_src_dir).expect("Failed to create member1 src directory");

        // Create member1 Cargo.toml
        let member1_cargo_toml = member1_dir.join("Cargo.toml");
        create_cargo_toml(&member1_cargo_toml, "workspace-member1")
            .expect("Failed to create member1 Cargo.toml");

        // Create a source file in member1
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
        // Test with a non-existent file path
        let crate_name = get_crate_for_file("/path/to/non/existent/file.rs");

        assert_eq!(
            crate_name, "unknown-crate",
            "Should return unknown-crate for non-existent file"
        );
    }

    #[test]
    fn test_get_crate_for_file_with_special_chars() {
        // Create a temporary directory with spaces and special characters
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("my crate with spaces");
        fs::create_dir_all(&crate_dir).expect("Failed to create crate directory with spaces");

        // Create Cargo.toml
        let cargo_toml_path = crate_dir.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "special-chars-crate")
            .expect("Failed to create Cargo.toml");

        // Create a source file
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
        // Create a temporary directory structure
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let crate_dir = temp_dir.path().join("directory-test");
        let src_dir = crate_dir.join("src");
        fs::create_dir_all(&src_dir).expect("Failed to create directories");

        // Create Cargo.toml
        let cargo_toml_path = crate_dir.join("Cargo.toml");
        create_cargo_toml(&cargo_toml_path, "directory-crate")
            .expect("Failed to create Cargo.toml");

        let crate_name = get_crate_for_file(&src_dir.to_string_lossy());

        assert_eq!(
            crate_name, "directory-crate",
            "Should work with directory paths"
        );
    }
}
