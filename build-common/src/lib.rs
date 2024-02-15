// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use cbindgen::{self, Config};
use std::path::{Path, PathBuf};
use std::{env, fs};

/// Generates a C header file using `cbindgen` for the specified crate.
///
/// # Arguments
///
/// * `crate_dir` - The directory of the crate to generate bindings for.
/// * `header_name` - The name of the header file to generate.
/// * `output_base_dir` - The base directory where the header file will be placed.
///                       Defaults to target if unspecified
pub fn generate_header(crate_dir: PathBuf, header_name: &str, output_base_dir: Option<&str>) {
    let cargo_target_dir = output_base_dir.unwrap_or("target");

    // Determine if `cargo_target_dir` is absolute or relative
    let cargo_target_path = Path::new(cargo_target_dir);
    let output_path = if cargo_target_path.is_absolute() {
        // If absolute, use it directly
        cargo_target_path.join("include/datadog/").join(header_name)
    } else {
        // If relative, adjust the path accordingly. we are in a crate, so get back to top level
        let adjusted_path = Path::new("..").join(cargo_target_path);
        adjusted_path.join("include/datadog/").join(header_name)
    };

    // Ensure the output directory exists
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).expect("Failed to create output directory");
        }
    }

    if env::var("DEBUG_BUILD_SCRIPT").is_ok() {
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
    // This assumes there is a cbindgen.toml file in the crate root
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
