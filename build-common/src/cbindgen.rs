// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use cbindgen::Config;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str;

pub const HEADER_PATH: &str = "include/datadog";

pub struct OutPaths {
    // Path to the default `./target directory where cargo outputs the result of
    // it's build process `
    pub cargo_target_dir: PathBuf,

    // Directory identified by the DESTDIR env variable.
    // It is taken realtive to the cargo project root.
    // if the env variable is not defined this defaults to cargo_target_dir
    pub deliverables_dir: PathBuf,
}

/// Determines the cargo target directory and deliverables directory.
pub fn determine_paths() -> OutPaths {
    let cargo_target_dir = match env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let output = Command::new("cargo")
                .args(["locate-project", "--workspace"])
                .output()
                .expect("Failed to execute `cargo locate-project`");

            if !output.status.success() {
                panic!("`cargo locate-project --workspace` command failed");
            }

            let stdout = str::from_utf8(&output.stdout).expect("Output not valid UTF-8");
            let json: serde_json::Value =
                serde_json::from_str(stdout).expect("Failed to parse JSON output");
            let project_root = json["root"]
                .as_str()
                .expect("Failed to extract project root path")
                .replace('\"', "");

            // Correctly find the parent of the Cargo.toml file's directory to approximate the
            // workspace root
            PathBuf::from(project_root)
                .parent()
                .expect("Failed to find workspace root directory")
                .to_path_buf()
                .join("target")
        }
    };
    // Attempt to read `DESTDIR`, falling back to `CARGO_TARGET_DIR` if not set
    let mut deliverables_dir = env::var("DESTDIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| cargo_target_dir.clone());
    println!("cargo:rerun-if-env-changed=DESTDIR");

    // Check if `deliverables_dir` is relative
    if deliverables_dir.is_relative() {
        // Get the parent directory of `cargo_target_dir` to use as a base for the relative
        // `deliverables_dir`
        let parent_dir = cargo_target_dir
            .parent()
            .expect("CARGO_TARGET_DIR does not have a parent directory, aborting build.");
        deliverables_dir = parent_dir.join(&deliverables_dir);
    }
    OutPaths {
        cargo_target_dir,
        deliverables_dir,
    }
}

/// Configure the header generation using environment variables.
/// call into `generate_header` with the appropriate arguments.
///
/// Expects CARGO_MANIFEST_DIR to be set.
/// If DESTDIR is set, it will be used as the base directory for the header file.
/// DESTDIR can be either relative or absolute.
/// Either CARGO_TARGET_DIR is set, or `cargo locate-project --workspace` is used to find the base
/// of the target directory.
///
/// # Arguments
///
/// * `header_name` - The name of the header file to generate.
pub fn generate_and_configure_header(header_name: &str) {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let OutPaths {
        cargo_target_dir, ..
    } = determine_paths();

    generate_header(crate_dir, header_name, cargo_target_dir);
}

/// Generates a C header file using `cbindgen` for the specified crate.
///
/// # Arguments
///
/// * `crate_dir` - The directory of the crate to generate bindings for.
/// * `header_name` - The name of the header file to generate.
/// * `output_base_dir` - The base directory where the header file will be placed. Should be an
///   absolute path as build scripts are run from the current crate's root.
pub fn generate_header(crate_dir: PathBuf, header_name: &str, output_base_dir: PathBuf) {
    assert!(
        output_base_dir.is_absolute(),
        "output_base_dir must be an absolute path"
    );
    let output_path = output_base_dir.join(HEADER_PATH).join(header_name);

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create output directory");
    }

    if env::var("DEBUG_BUILD").is_ok() {
        println!(
            "cargo:warning=Output path for include: {}",
            output_path.display()
        );
    }

    cbindgen::Builder::new()
        .with_crate(crate_dir.clone())
        .with_config(Config::from_root_or_default(&crate_dir))
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(output_path);

    println!("cargo:rerun-if-changed=cbindgen.toml");
}

/// Copies header files from the source directory to the destination directory.
/// It considers all .*h* files in the source directory.
/// This behaves in a similar way as `generate_and_configure_header`.
pub fn copy_and_configure_headers() {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let OutPaths {
        cargo_target_dir, ..
    } = determine_paths();

    let src_dir = crate_dir.join("src");
    if src_dir.is_dir() {
        for entry in fs::read_dir(src_dir).expect("Failed to read src directory") {
            let entry = entry.expect("Failed to read entry in src directory");
            let path = entry.path();

            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("h") {
                copy_header(&path, &cargo_target_dir);
            }
        }
    }
}

/// Copies a single header file.
/// # Arguments
///
/// * `source` - The source header
/// * `destination` - The destination path
fn copy_header(source: &Path, destination: &Path) {
    let output_path = destination
        .join(HEADER_PATH)
        .join(source.file_name().unwrap());

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create output directory");
    }

    if env::var("DEBUG_BUILD").is_ok() {
        println!(
            "cargo:warning=Copy header {} to {}",
            source.display(),
            output_path.display()
        );
    }
    fs::copy(source, output_path).expect("Failed to copy header file");
}
